//! The executor's dispatch seam: spawning ticket dispatches onto the join set,
//! processing joined dispatch outcomes, worker-candidate selection, and the
//! local reservation/capacity bookkeeping. Named `spawn` to avoid clashing with
//! the sibling `workflow::execution::dispatch` module.

use std::collections::{HashMap, HashSet};

use voom_core::OperationKind;
use voom_core::{JobId, TicketId, TicketOperation, VoomError, WorkerId};
use voom_scheduler::{LeastLoadedWorkerSelector, WorkerSelector, WorkerView};
use voom_store::repo::leases::{LeaseAcquireOutcome, NewLease};
use voom_store::repo::tickets::{Ticket, TicketState};
use voom_store::repo::workers::WorkerOperationCandidate;
use voom_worker_protocol::{
    TranscodeVideoProfile, VideoAcceleratorDescriptor, VideoHardwareAssignment,
    VideoHardwareRequirement, VideoToolboxDecodeRequirement,
};

use crate::video_hardware::{candidate_accelerator_descriptor, historical_accelerator_descriptor};
use crate::workflow::execution::dispatch::{DispatchOutcome, DispatchTerminal, dispatch_ticket};
use crate::workflow::execution::executor::errors::selector_failure_class;
use crate::workflow::execution::executor::tickets::parse_payload;
use crate::workflow::execution::executor::{RunFailureMode, RunLoopState, WorkflowExecutor};
use crate::workflow::execution::leases::{
    acquire_lease_with_retry, failure_class_for_error, time_duration,
};
use crate::workflow::execution::operation_adapters::uses_bundled_policy_verification;
use crate::workflow::execution::runtime::{WorkerRuntime, WorkerRuntimeRegistry};
use crate::workflow::plan::model::WorkflowPlan;
use crate::workflow::summary::WorkflowRunSummary;

#[derive(Debug)]
pub(super) enum SpawnOutcome {
    Spawned(Vec<String>),
    PreLeaseRetriable,
    PreLeaseTerminal(VoomError),
    CapacityDeferred,
    AcceleratorUnavailable(Vec<String>),
}

impl WorkflowExecutor {
    pub(super) async fn try_spawn_dispatch(
        &self,
        state: &mut RunLoopState,
        ticket: Ticket,
        accelerator_runtimes: &mut Option<WorkerRuntimeRegistry>,
    ) -> Result<SpawnOutcome, VoomError> {
        let mut workflow_payload = parse_payload(&ticket)?;
        let projected = self
            .candidate_workers(
                workflow_payload.operation,
                &workflow_payload.rendered_payload,
                &state.reservations,
                &mut state.accelerator_history,
                accelerator_runtimes,
            )
            .await?;
        let ProjectedCandidates {
            workers: candidates,
            assignments,
            unavailable_tokens,
            mut recovery_tokens,
        } = projected;
        if candidates.is_empty() && !unavailable_tokens.is_empty() {
            return Ok(SpawnOutcome::AcceleratorUnavailable(unavailable_tokens));
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
                state.summary.failure_count += u64::from(outcome.terminal);
                if outcome.terminal {
                    return Ok(SpawnOutcome::PreLeaseTerminal(source));
                }
                return Ok(SpawnOutcome::PreLeaseRetriable);
            }
        };
        let selected_assignment = assignments.get(&worker_id);
        let uses_accelerator = apply_hardware_assignment(
            &mut workflow_payload.rendered_payload,
            selected_assignment,
            &mut recovery_tokens,
        )?;
        let uses_bundled_verify = uses_bundled_policy_verification(
            workflow_payload.operation,
            &workflow_payload.rendered_payload,
        );
        let runtime = self.dispatch_runtime(
            uses_bundled_verify,
            uses_accelerator,
            accelerator_runtimes.as_ref(),
            worker_id,
        )?;
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
        increment_reservation(&mut state.reservations, worker_id);
        state.summary.dispatch_count += 1;
        state
            .summary
            .record_dispatch(workflow_payload.operation, worker_id, &state.reservations);

        let control = self.control_plane.clone();
        let options = self.options.dispatch_options();
        state.active.spawn(async move {
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
        Ok(SpawnOutcome::Spawned(recovery_tokens))
    }

    fn dispatch_runtime(
        &self,
        uses_bundled_verify: bool,
        uses_accelerator: bool,
        accelerator_runtimes: Option<&WorkerRuntimeRegistry>,
        worker_id: WorkerId,
    ) -> Result<Option<WorkerRuntime>, VoomError> {
        if uses_bundled_verify {
            return Ok(None);
        }
        let runtimes = if uses_accelerator {
            accelerator_runtimes.ok_or_else(|| {
                VoomError::Internal("NVIDIA candidate projection omitted live runtimes".to_owned())
            })?
        } else {
            &self.runtimes
        };
        runtimes.get(worker_id).map(Some)
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
        history: &mut HashMap<String, Vec<String>>,
        accelerator_runtimes: &mut Option<WorkerRuntimeRegistry>,
    ) -> Result<ProjectedCandidates, VoomError> {
        let candidates = self
            .control_plane
            .workers
            .operation_candidates(&TicketOperation::from(operation))
            .await?;
        let requirement = video_hardware_requirement(operation, payload)?;
        if matches!(requirement, Some(VideoHardwareRequirement::Nvidia(_)))
            && accelerator_runtimes.is_none()
        {
            *accelerator_runtimes = Some(self.control_plane.live_policy_runtime_registry().await?);
        }
        let conflicts = conflicting_accelerator_tokens(&candidates)?;
        let mut workers = Vec::new();
        let mut assignments = HashMap::new();
        for candidate in candidates {
            if matches!(requirement, Some(VideoHardwareRequirement::Nvidia(_)))
                && !accelerator_runtimes
                    .as_ref()
                    .is_some_and(|runtimes| runtimes.contains(candidate.worker_id))
            {
                continue;
            }
            let assignment =
                match compatible_assignment(&candidate, requirement.as_ref(), &conflicts)? {
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
        let recovery_tokens = self
            .historical_accelerator_tokens(operation, requirement.as_ref(), history)
            .await?
            .into_iter()
            .filter(|token| !conflicts.contains(token))
            .collect::<Vec<_>>();
        let unavailable_tokens = if workers.is_empty() {
            recovery_tokens.clone()
        } else {
            Vec::new()
        };
        Ok(ProjectedCandidates {
            workers,
            assignments,
            unavailable_tokens,
            recovery_tokens,
        })
    }

    async fn historical_accelerator_tokens(
        &self,
        operation: OperationKind,
        requirement: Option<&VideoHardwareRequirement>,
        history: &mut HashMap<String, Vec<String>>,
    ) -> Result<Vec<String>, VoomError> {
        let Some(requirement) = requirement else {
            return Ok(Vec::new());
        };
        if matches!(requirement, VideoHardwareRequirement::Software(_)) {
            return Ok(Vec::new());
        }
        let cache_key = serde_json::to_string(requirement)
            .map_err(|error| VoomError::Internal(format!("serialize requirement: {error}")))?;
        if let Some(tokens) = history.get(&cache_key) {
            return Ok(tokens.clone());
        }
        let capabilities = self
            .control_plane
            .workers
            .operation_capability_history(&TicketOperation::from(operation))
            .await?;
        let mut tokens = Vec::new();
        for capability in capabilities {
            let Some(descriptor) = historical_accelerator_descriptor(&capability)? else {
                continue;
            };
            let hardware_token = descriptor.hardware_token().to_owned();
            let compatible = match (requirement, descriptor) {
                (
                    VideoHardwareRequirement::Nvidia(required),
                    VideoAcceleratorDescriptor::Nvidia(descriptor),
                ) => {
                    descriptor.encoders.contains(&required.encoder)
                        && required
                            .decoder
                            .as_ref()
                            .is_none_or(|decoder| descriptor.decoders.contains(decoder))
                }
                (
                    VideoHardwareRequirement::VideoToolbox(required),
                    VideoAcceleratorDescriptor::VideoToolbox(descriptor),
                ) => {
                    descriptor.encoders.contains(&required.encoder)
                        && required.decoder.as_ref().is_none_or(|decoder| {
                            videotoolbox_decoder_matches(&descriptor.decoders, decoder)
                        })
                }
                _ => false,
            };
            if compatible {
                tokens.push(hardware_token);
            }
        }
        tokens.sort();
        tokens.dedup();
        history.insert(cache_key, tokens.clone());
        Ok(tokens)
    }
}

fn apply_hardware_assignment(
    payload: &mut serde_json::Value,
    assignment: Option<&VideoHardwareAssignment>,
    recovery_tokens: &mut Vec<String>,
) -> Result<bool, VoomError> {
    let Some(assignment) = assignment else {
        return Ok(false);
    };
    let hardware_token = match assignment {
        VideoHardwareAssignment::Nvidia(value) => &value.hardware_token,
        VideoHardwareAssignment::VideoToolbox(value) => &value.hardware_token,
        VideoHardwareAssignment::Software(_) => {
            return Err(VoomError::Internal(
                "software dispatch unexpectedly carried a hardware assignment".to_owned(),
            ));
        }
    };
    recovery_tokens.push(hardware_token.clone());
    recovery_tokens.sort();
    recovery_tokens.dedup();
    payload["hardware_assignment"] = serde_json::to_value(assignment)
        .map_err(|error| VoomError::Internal(format!("serialize hardware assignment: {error}")))?;
    Ok(true)
}

struct ProjectedCandidates {
    workers: Vec<WorkerView>,
    assignments: HashMap<WorkerId, VideoHardwareAssignment>,
    unavailable_tokens: Vec<String>,
    recovery_tokens: Vec<String>,
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
    if profile.encoder == "h264_videotoolbox" || profile.encoder == "hevc_videotoolbox" {
        let decoder = if profile.decode.is_video_toolbox() {
            let codec = source_video_codec(payload).ok_or_else(|| {
                VoomError::Config(
                    "VideoToolbox decode requires a known source video codec".to_owned(),
                )
            })?;
            let pixel_format = source_video_pixel_format(payload).ok_or_else(|| {
                VoomError::Config(
                    "VideoToolbox decode requires a known source video pixel format".to_owned(),
                )
            })?;
            Some(VideoToolboxDecodeRequirement {
                codec: codec.to_owned(),
                pixel_format: pixel_format.to_owned(),
            })
        } else {
            None
        };
        return Ok(Some(VideoHardwareRequirement::video_toolbox(
            profile.encoder,
            decoder,
        )));
    }
    if profile.encoder != "hevc_nvenc" {
        return Ok(Some(VideoHardwareRequirement::software()));
    }
    let decoder = if profile.decode.is_nvidia() {
        let codec = source_video_codec(payload).ok_or_else(|| {
            VoomError::Config("NVIDIA decode requires a known source video codec".to_owned())
        })?;
        Some(
            voom_core::nvidia_decoder_for_video_codec(codec)
                .ok_or_else(|| {
                    VoomError::Config(format!(
                        "NVIDIA decode does not support source video codec `{codec}`"
                    ))
                })?
                .to_owned(),
        )
    } else {
        None
    };
    Ok(Some(VideoHardwareRequirement::nvidia(
        "hevc_nvenc",
        decoder,
    )))
}

fn source_video_pixel_format(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("source_video_pixel_format")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("input")
                .and_then(|input| input.get("video_pixel_format"))
                .and_then(serde_json::Value::as_str)
        })
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidateCompatibility {
    Incompatible,
    Compatible(Option<VideoHardwareAssignment>),
}

fn compatible_assignment(
    candidate: &WorkerOperationCandidate,
    requirement: Option<&VideoHardwareRequirement>,
    conflicts: &HashSet<String>,
) -> Result<CandidateCompatibility, VoomError> {
    match requirement {
        None => Ok(CandidateCompatibility::Compatible(None)),
        Some(VideoHardwareRequirement::Software(_)) => {
            if candidate_accelerator_descriptor(candidate)?.is_none()
                && candidate.hardware.is_empty()
            {
                Ok(CandidateCompatibility::Compatible(None))
            } else {
                Ok(CandidateCompatibility::Incompatible)
            }
        }
        Some(VideoHardwareRequirement::Nvidia(required)) => {
            let Some(descriptor) = candidate_accelerator_descriptor(candidate)? else {
                return Ok(CandidateCompatibility::Incompatible);
            };
            let VideoAcceleratorDescriptor::Nvidia(descriptor) = descriptor else {
                return Ok(CandidateCompatibility::Incompatible);
            };
            if conflicts.contains(&descriptor.hardware_token)
                || !candidate.hardware.contains(&descriptor.hardware_token)
                || !descriptor.encoders.contains(&required.encoder)
                || required
                    .decoder
                    .as_ref()
                    .is_some_and(|decoder| !descriptor.decoders.contains(decoder))
            {
                return Ok(CandidateCompatibility::Incompatible);
            }
            Ok(CandidateCompatibility::Compatible(Some(
                VideoHardwareAssignment::nvidia(descriptor.hardware_token, descriptor.device_uuid),
            )))
        }
        Some(VideoHardwareRequirement::VideoToolbox(required)) => {
            let Some(VideoAcceleratorDescriptor::VideoToolbox(descriptor)) =
                candidate_accelerator_descriptor(candidate)?
            else {
                return Ok(CandidateCompatibility::Incompatible);
            };
            if conflicts.contains(&descriptor.hardware_token)
                || !candidate.hardware.contains(&descriptor.hardware_token)
                || !descriptor.encoders.contains(&required.encoder)
                || required.decoder.as_ref().is_some_and(|decoder| {
                    !videotoolbox_decoder_matches(&descriptor.decoders, decoder)
                })
            {
                return Ok(CandidateCompatibility::Incompatible);
            }
            Ok(CandidateCompatibility::Compatible(Some(
                VideoHardwareAssignment::video_toolbox(
                    descriptor.hardware_token,
                    descriptor.resource_id,
                ),
            )))
        }
    }
}

fn conflicting_accelerator_tokens(
    candidates: &[WorkerOperationCandidate],
) -> Result<HashSet<String>, VoomError> {
    let mut capacities = HashMap::new();
    let mut conflicts = HashSet::new();
    for candidate in candidates {
        let Some(descriptor) = candidate_accelerator_descriptor(candidate)? else {
            continue;
        };
        let hardware_token = descriptor.hardware_token().to_owned();
        let max_sessions = descriptor.max_sessions();
        if let Some(capacity) = capacities.insert(hardware_token.clone(), max_sessions)
            && capacity != max_sessions
        {
            conflicts.insert(hardware_token);
        }
    }
    Ok(conflicts)
}

fn videotoolbox_decoder_matches(
    capabilities: &[voom_worker_protocol::VideoToolboxDecodeCapability],
    required: &VideoToolboxDecodeRequirement,
) -> bool {
    capabilities.iter().any(|capability| {
        capability.codec == required.codec
            && capability.pixel_formats.contains(&required.pixel_format)
    })
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
