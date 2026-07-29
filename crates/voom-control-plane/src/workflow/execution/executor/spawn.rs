//! The executor's dispatch seam: spawning ticket dispatches onto the join set,
//! processing joined dispatch outcomes, worker-candidate selection, and the
//! local reservation/capacity bookkeeping. Named `spawn` to avoid clashing with
//! the sibling `workflow::execution::dispatch` module.

use std::collections::{HashMap, HashSet};

use tokio::task::JoinSet;
use voom_core::OperationKind;
use voom_core::{JobId, TicketId, TicketOperation, VoomError, WorkerId};
use voom_scheduler::{LeastLoadedWorkerSelector, WorkerSelector, WorkerView};
use voom_store::repo::leases::{LeaseAcquireOutcome, NewLease};
use voom_store::repo::tickets::{Ticket, TicketState};
use voom_store::repo::workers::WorkerOperationCandidate;
use voom_store::repo::workers::WorkerOperationCapability;
use voom_worker_protocol::{
    NvidiaVideoAcceleratorDescriptor, TranscodeVideoProfile, VideoHardwareAssignment,
    VideoHardwareRequirement,
};

use crate::workflow::execution::dispatch::{DispatchOutcome, DispatchTerminal, dispatch_ticket};
use crate::workflow::execution::executor::RunFailureMode;
use crate::workflow::execution::executor::WorkflowExecutor;
use crate::workflow::execution::executor::errors::selector_failure_class;
use crate::workflow::execution::executor::tickets::parse_payload;
use crate::workflow::execution::leases::{
    acquire_lease_with_retry, failure_class_for_error, time_duration,
};
use crate::workflow::execution::operation_adapters::uses_bundled_policy_verification;
use crate::workflow::plan::model::WorkflowPlan;
use crate::workflow::summary::WorkflowRunSummary;

#[derive(Debug)]
pub(super) enum SpawnOutcome {
    Spawned(Option<String>),
    PreLeaseRetriable,
    PreLeaseTerminal(VoomError),
    CapacityDeferred,
    AcceleratorUnavailable(String),
}

impl WorkflowExecutor {
    pub(super) async fn try_spawn_dispatch(
        &self,
        active: &mut JoinSet<DispatchOutcome>,
        reservations: &mut HashMap<WorkerId, u32>,
        summary: &mut WorkflowRunSummary,
        ticket: Ticket,
    ) -> Result<SpawnOutcome, VoomError> {
        let mut workflow_payload = parse_payload(&ticket)?;
        let projected = self
            .candidate_workers(
                workflow_payload.operation,
                &workflow_payload.rendered_payload,
                reservations,
            )
            .await?;
        let candidates = projected.workers;
        if candidates.is_empty()
            && let Some(hardware_token) = projected.unavailable_token
        {
            return Ok(SpawnOutcome::AcceleratorUnavailable(hardware_token));
        }
        let selector = LeastLoadedWorkerSelector;
        let worker_id = match selector.select(workflow_payload.operation, &candidates) {
            Ok(worker_id) => worker_id,
            Err(source) => {
                if matches!(source, VoomError::NoEligibleWorker(_))
                    && all_candidates_at_capacity(&candidates)
                {
                    return Ok(SpawnOutcome::CapacityDeferred);
                }
                let class = selector_failure_class(&source)?;
                let outcome = self
                    .control_plane
                    .record_pre_lease_ticket_failure(
                        ticket.id,
                        class,
                        self.control_plane.clock().now(),
                    )
                    .await?;
                summary.failure_count += u64::from(outcome.terminal);
                if outcome.terminal {
                    return Ok(SpawnOutcome::PreLeaseTerminal(source));
                }
                return Ok(SpawnOutcome::PreLeaseRetriable);
            }
        };
        let selected_hardware_token =
            projected
                .assignments
                .get(&worker_id)
                .and_then(|assignment| match assignment {
                    VideoHardwareAssignment::Software(_) => None,
                    VideoHardwareAssignment::Nvidia(assignment) => {
                        Some(assignment.hardware_token.clone())
                    }
                });
        if let Some(assignment) = projected.assignments.get(&worker_id) {
            workflow_payload.rendered_payload["hardware_assignment"] =
                serde_json::to_value(assignment).map_err(|error| {
                    VoomError::Internal(format!("serialize hardware assignment: {error}"))
                })?;
        }
        let uses_bundled_verify = uses_bundled_policy_verification(
            workflow_payload.operation,
            &workflow_payload.rendered_payload,
        );
        let runtime = if uses_bundled_verify {
            None
        } else {
            Some(self.runtimes.get(worker_id)?)
        };
        let acquisition = acquire_lease_with_retry(
            &self.control_plane,
            NewLease {
                ticket_id: ticket.id,
                worker_id,
                ttl: time_duration(self.options.timing.lease_ttl)?,
                now: self.control_plane.clock().now(),
            },
        )
        .await?;
        let LeaseAcquireOutcome::Acquired(lease) = acquisition else {
            return Ok(SpawnOutcome::CapacityDeferred);
        };
        increment_reservation(reservations, worker_id);
        summary.dispatch_count += 1;
        summary.record_dispatch(workflow_payload.operation, worker_id, reservations);

        let control = self.control_plane.clone();
        let options = self.options.dispatch_options();
        active.spawn(async move {
            dispatch_ticket(
                control,
                worker_id,
                runtime,
                ticket,
                workflow_payload,
                lease.id,
                options,
            )
            .await
        });
        Ok(SpawnOutcome::Spawned(selected_hardware_token))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "completion handling needs shared scheduler state plus immutable workflow context"
    )]
    pub(super) async fn process_joined_dispatch(
        &self,
        joined: Result<DispatchOutcome, tokio::task::JoinError>,
        plan: &WorkflowPlan,
        workflow_id: &str,
        job_id: JobId,
        reservations: &mut HashMap<WorkerId, u32>,
        summary: &mut WorkflowRunSummary,
        failure_mode: RunFailureMode,
        fatal_error: &mut Option<VoomError>,
        isolated_error: &mut Option<VoomError>,
    ) {
        let outcome = match joined {
            Ok(outcome) => outcome,
            Err(err) => DispatchOutcome {
                ticket_id: TicketId(0),
                worker_id: WorkerId(0),
                operation: OperationKind::HashFile,
                terminal: DispatchTerminal::Failure {
                    source: VoomError::WorkerCrash(format!(
                        "workflow dispatch task crashed: {err}"
                    )),
                },
            },
        };
        decrement_reservation(reservations, outcome.worker_id);
        match outcome.terminal {
            DispatchTerminal::Success => {
                summary.record_success(outcome.operation);
                if let Err(source) = self
                    .expand_successful_ticket(plan, workflow_id, job_id, outcome.ticket_id)
                    .await
                {
                    *fatal_error = Some(source);
                }
            }
            DispatchTerminal::Failure { source } => {
                let class = match self.ticket_failure_class(outcome.ticket_id).await {
                    Ok(Some(class)) => class,
                    Ok(None) => failure_class_for_error(&source),
                    Err(err) => {
                        summary.record_failure(outcome.operation, failure_class_for_error(&source));
                        *fatal_error = Some(err);
                        return;
                    }
                };
                summary.record_failure(outcome.operation, class);
                match self.control_plane.tickets.get(outcome.ticket_id).await {
                    Ok(Some(ticket)) if ticket.state == TicketState::Failed => match failure_mode {
                        RunFailureMode::AbortJob => *fatal_error = Some(source),
                        RunFailureMode::ContinueIndependent => {
                            if isolated_error.is_none() {
                                *isolated_error = Some(source);
                            }
                        }
                    },
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        *fatal_error = Some(VoomError::NotFound(format!(
                            "ticket {} vanished after dispatch failure",
                            outcome.ticket_id
                        )));
                    }
                    Err(err) => {
                        *fatal_error = Some(err);
                    }
                }
            }
        }
    }

    async fn candidate_workers(
        &self,
        operation: OperationKind,
        payload: &serde_json::Value,
        reservations: &HashMap<WorkerId, u32>,
    ) -> Result<ProjectedCandidates, VoomError> {
        let candidates = self
            .control_plane
            .workers
            .operation_candidates(&TicketOperation::from(operation))
            .await?;
        let requirement = video_hardware_requirement(operation, payload)?;
        let conflicts = conflicting_accelerator_tokens(&candidates);
        let mut workers = Vec::new();
        let mut assignments = HashMap::new();
        for candidate in candidates {
            let assignment =
                match compatible_assignment(&candidate, requirement.as_ref(), &conflicts) {
                    CandidateCompatibility::Incompatible => continue,
                    CandidateCompatibility::Compatible(assignment) => assignment,
                };
            if let Some(assignment) = assignment {
                assignments.insert(candidate.worker_id, assignment);
            }
            workers.push(WorkerView {
                worker_id: candidate.worker_id,
                supports: vec![operation],
                active_leases: candidate
                    .active_leases
                    .max(reservations.get(&candidate.worker_id).copied().unwrap_or(0)),
                max_parallel: candidate.max_parallel,
            });
        }
        let unavailable_token = if workers.is_empty() {
            self.historical_accelerator_token(operation, requirement.as_ref(), &conflicts)
                .await?
        } else {
            None
        };
        Ok(ProjectedCandidates {
            workers,
            assignments,
            unavailable_token,
        })
    }

    async fn historical_accelerator_token(
        &self,
        operation: OperationKind,
        requirement: Option<&VideoHardwareRequirement>,
        conflicts: &HashSet<String>,
    ) -> Result<Option<String>, VoomError> {
        let Some(VideoHardwareRequirement::Nvidia(requirement)) = requirement else {
            return Ok(None);
        };
        let capabilities = self
            .control_plane
            .workers
            .operation_capability_history(&TicketOperation::from(operation))
            .await?;
        Ok(capabilities.into_iter().find_map(|capability| {
            historical_descriptor(&capability).and_then(|descriptor| {
                if conflicts.contains(&descriptor.hardware_token)
                    || !descriptor.encoders.contains(&requirement.encoder)
                    || requirement
                        .decoder
                        .as_ref()
                        .is_some_and(|decoder| !descriptor.decoders.contains(decoder))
                {
                    None
                } else {
                    Some(descriptor.hardware_token)
                }
            })
        }))
    }
}

struct ProjectedCandidates {
    workers: Vec<WorkerView>,
    assignments: HashMap<WorkerId, VideoHardwareAssignment>,
    unavailable_token: Option<String>,
}

fn video_hardware_requirement(
    operation: OperationKind,
    payload: &serde_json::Value,
) -> Result<Option<VideoHardwareRequirement>, VoomError> {
    if operation != OperationKind::TranscodeVideo {
        return Ok(None);
    }
    let profile_value = payload
        .get("resolved_profile")
        .or_else(|| payload.get("profile"))
        .ok_or_else(|| VoomError::Config("transcode payload missing profile".to_owned()))?;
    let profile: TranscodeVideoProfile = serde_json::from_value(profile_value.clone())
        .map_err(|error| VoomError::Config(format!("transcode profile malformed: {error}")))?;
    if profile.encoder != "hevc_nvenc" {
        return Ok(Some(VideoHardwareRequirement::software()));
    }
    let decoder = if profile.decode.is_nvidia() {
        let codec = source_video_codec(payload).ok_or_else(|| {
            VoomError::Config("NVIDIA decode requires a known source video codec".to_owned())
        })?;
        Some(nvidia_decoder(codec).to_owned())
    } else {
        None
    };
    Ok(Some(VideoHardwareRequirement::nvidia(
        "hevc_nvenc",
        decoder,
    )))
}

fn source_video_codec(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("source_video_codec")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("input")
                .and_then(|input| input.get("video_codec"))
                .and_then(serde_json::Value::as_str)
        })
}

fn nvidia_decoder(codec: &str) -> &str {
    match codec {
        "h264" => "h264_cuvid",
        "hevc" | "h265" => "hevc_cuvid",
        "av1" => "av1_cuvid",
        _ => "<unsupported>",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidateCompatibility {
    Incompatible,
    Compatible(Option<VideoHardwareAssignment>),
}

fn compatible_assignment(
    candidate: &WorkerOperationCandidate,
    requirement: Option<&VideoHardwareRequirement>,
    conflicts: &HashSet<String>,
) -> CandidateCompatibility {
    match requirement {
        None => CandidateCompatibility::Compatible(None),
        Some(VideoHardwareRequirement::Software(_)) => {
            if accelerator_descriptor(candidate).is_none() && candidate.hardware.is_empty() {
                CandidateCompatibility::Compatible(None)
            } else {
                CandidateCompatibility::Incompatible
            }
        }
        Some(VideoHardwareRequirement::Nvidia(required)) => {
            let Some(descriptor) = accelerator_descriptor(candidate) else {
                return CandidateCompatibility::Incompatible;
            };
            if conflicts.contains(&descriptor.hardware_token)
                || !candidate.hardware.contains(&descriptor.hardware_token)
                || !descriptor.encoders.contains(&required.encoder)
                || required
                    .decoder
                    .as_ref()
                    .is_some_and(|decoder| !descriptor.decoders.contains(decoder))
            {
                return CandidateCompatibility::Incompatible;
            }
            CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::nvidia(
                descriptor.hardware_token,
                descriptor.device_uuid,
            )))
        }
    }
}

fn accelerator_descriptor(
    candidate: &WorkerOperationCandidate,
) -> Option<NvidiaVideoAcceleratorDescriptor> {
    let mut descriptors = candidate.capability_extra.iter().filter_map(|extra| {
        extra
            .get("accelerator")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
    });
    let descriptor = descriptors.next();
    if descriptors.next().is_some() {
        return None;
    }
    descriptor
}

fn historical_descriptor(
    capability: &WorkerOperationCapability,
) -> Option<NvidiaVideoAcceleratorDescriptor> {
    capability
        .extra
        .get("accelerator")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn conflicting_accelerator_tokens(candidates: &[WorkerOperationCandidate]) -> HashSet<String> {
    let mut capacities = HashMap::new();
    let mut conflicts = HashSet::new();
    for candidate in candidates {
        let Some(descriptor) = accelerator_descriptor(candidate) else {
            continue;
        };
        if let Some(capacity) =
            capacities.insert(descriptor.hardware_token.clone(), descriptor.max_sessions)
            && capacity != descriptor.max_sessions
        {
            conflicts.insert(descriptor.hardware_token);
        }
    }
    conflicts
}

fn increment_reservation(reservations: &mut HashMap<WorkerId, u32>, worker_id: WorkerId) {
    *reservations.entry(worker_id).or_default() += 1;
}

fn decrement_reservation(reservations: &mut HashMap<WorkerId, u32>, worker_id: WorkerId) {
    if let Some(count) = reservations.get_mut(&worker_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            reservations.remove(&worker_id);
        }
    }
}

fn all_candidates_at_capacity(candidates: &[WorkerView]) -> bool {
    !candidates.is_empty()
        && candidates
            .iter()
            .all(|candidate| candidate.active_leases >= candidate.max_parallel)
}

#[cfg(test)]
#[path = "spawn_test.rs"]
mod tests;
