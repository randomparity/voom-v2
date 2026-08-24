//! The executor's dispatch seam: spawning ticket dispatches onto the join set,
//! processing joined dispatch outcomes, worker-candidate selection, and the
//! local reservation/capacity bookkeeping. Named `spawn` to avoid clashing with
//! the sibling `workflow::execution::dispatch` module.

use std::collections::{HashMap, HashSet};

use voom_core::OperationKind;
use voom_core::{
    FailureClass, TicketId, TicketOperation, VideoEncoderBackend, VoomError, WorkerId,
};
use voom_scheduler::{WorkerView, select_least_loaded_worker};
use voom_store::repo::execution::leases::{LeaseAcquireOutcome, NewLease};
use voom_store::repo::execution::tickets::{Ticket, TicketState};
use voom_store::repo::execution::workers::WorkerOperationCandidate;
use voom_worker_protocol::{
    NvidiaVideoAcceleratorDescriptor, NvidiaVideoHardwareRequirement, TranscodeVideoProfile,
    VaapiVideoAcceleratorDescriptor, VaapiVideoHardwareRequirement, VideoAcceleratorDescriptor,
    VideoHardwareAssignment, VideoHardwareRequirement, VideoToolboxDecodeRequirement,
    VideoToolboxVideoAcceleratorDescriptor, VideoToolboxVideoHardwareRequirement,
    vaapi_hardware_token,
};

use crate::video_hardware::{candidate_accelerator_descriptor, historical_accelerator_descriptor};
use crate::workflow::execution::dispatch::{DispatchOutcome, DispatchTerminal, dispatch_ticket};
use crate::workflow::execution::executor::errors::selector_failure_class;
use crate::workflow::execution::executor::tickets::parse_payload;
use crate::workflow::execution::executor::{
    DispatchIdentity, RunFailureMode, RunInvocation, RunLoopState, WorkflowDispatchOptions,
    WorkflowExecutor,
};
use crate::workflow::execution::leases::{
    acquire_lease_with_retry, fail_lease_with_retry, failure_class_for_error, time_duration,
};
use crate::workflow::execution::operation_adapters::uses_bundled_policy_verification;
use crate::workflow::execution::runtime::{WorkerRuntime, WorkerRuntimeRegistry};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

#[derive(Debug)]
pub(super) enum SpawnOutcome {
    Spawned(Vec<String>),
    PreLeaseRetriable,
    PreLeaseTerminal(VoomError),
    CapacityDeferred,
    AcceleratorUnavailable(Vec<String>),
    /// The ticket executes through its storage owner's node agent (ADR 0075):
    /// the executor neither leases nor pushes it. It stays `ready` until an
    /// agent acquires it through the remote-lease flow, and the run loop waits
    /// for that externally owned progress.
    NodeLocalDispatched,
}

struct DispatchTask {
    identity: DispatchIdentity,
    control: crate::ControlPlane,
    runtime: Option<WorkerRuntime>,
    ticket: Ticket,
    workflow_payload: WorkflowTicketPayload,
    options: WorkflowDispatchOptions,
    #[cfg(test)]
    panic_after_lease: Option<OperationKind>,
}

impl DispatchTask {
    async fn run(self) -> DispatchOutcome {
        #[cfg(test)]
        assert!(
            self.panic_after_lease != Some(self.identity.operation),
            "injected dispatch panic after lease acquisition"
        );
        dispatch_ticket(
            self.control,
            self.identity.worker_id,
            self.runtime,
            self.ticket,
            self.workflow_payload,
            self.identity.lease_id,
            self.options,
        )
        .await
    }
}

fn spawn_dispatch_task(state: &mut RunLoopState, task: DispatchTask) {
    let identity = task.identity;
    let abort_handle = state.active.spawn(task.run());
    state.active_identities.insert(abort_handle.id(), identity);
}

impl WorkflowExecutor {
    fn start_dispatch(
        &self,
        state: &mut RunLoopState,
        ticket: Ticket,
        workflow_payload: WorkflowTicketPayload,
        runtime: Option<WorkerRuntime>,
        identity: DispatchIdentity,
    ) {
        increment_reservation(&mut state.reservations, identity.worker_id);
        state.summary.dispatch_count += 1;
        state
            .summary
            .record_dispatch(identity.operation, identity.worker_id, &state.reservations);
        spawn_dispatch_task(
            state,
            DispatchTask {
                identity,
                control: self.control_plane.clone(),
                runtime,
                ticket,
                workflow_payload,
                options: self.options.dispatch_options(),
                #[cfg(test)]
                panic_after_lease: self.options.chaos.panic_after_lease_operation,
            },
        );
    }
}

impl WorkflowExecutor {
    pub(super) async fn try_spawn_dispatch(
        &self,
        state: &mut RunLoopState,
        ticket: Ticket,
        accelerator_runtimes: &mut Option<WorkerRuntimeRegistry>,
    ) -> Result<SpawnOutcome, VoomError> {
        let mut workflow_payload = parse_payload(&ticket)?;
        if let Some(outcome) = node_local_dispatch_outcome(state, ticket.id, &workflow_payload) {
            return Ok(outcome);
        }
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
        let worker_id = match select_least_loaded_worker(workflow_payload.operation, &candidates) {
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
        let lease = match acquisition {
            LeaseAcquireOutcome::Acquired(lease) => lease,
            // Only capacity saturation defers the spawn: the worker may gain
            // capacity later. A ticket that is not ready or a worker that is
            // ineligible is a loud error with the legacy classification,
            // never a silent deferral loop.
            LeaseAcquireOutcome::CapacityFull(_) => {
                return Ok(SpawnOutcome::CapacityDeferred);
            }
            rejection => {
                return match rejection.into_lease_result() {
                    Err(error) => Err(error),
                    Ok(_) => Err(VoomError::Internal(
                        "lease acquisition reported no lease".to_owned(),
                    )),
                };
            }
        };
        let identity = DispatchIdentity {
            ticket_id: ticket.id,
            worker_id,
            lease_id: lease.id,
            operation: workflow_payload.operation,
        };
        self.start_dispatch(state, ticket, workflow_payload, runtime, identity);
        Ok(SpawnOutcome::Spawned(recovery_tokens))
    }

    /// Poll the node-locally dispatched media tickets (ADR 0075) and settle
    /// their durable outcomes into the run.
    ///
    /// The executor holds no lease for these tickets: the storage owner's
    /// agent drives the fenced remote-lease flow, so their completion is
    /// observed here rather than arriving through a dispatch task. A succeeded
    /// ticket expands exactly as a locally dispatched one would; a failed one
    /// keeps its recorded failure class and follows the run's failure mode;
    pub(super) async fn settle_node_local_completions(
        &self,
        state: &mut RunLoopState,
        invocation: &RunInvocation<'_>,
    ) -> Result<(), VoomError> {
        // Settlement is driven off durable state, not just the outstanding
        // map: an agent can complete a ticket between observation windows —
        // before the executor ever observed it `ready` — and its expansion
        // children must still run. Candidates are therefore the union of the
        // outstanding map and every terminal media ticket durably recorded
        // for this workflow; `node_local_settled` folds each in exactly once.
        let mut candidate_ids: HashSet<TicketId> =
            state.node_local_outstanding.keys().copied().collect();
        for (ticket_id, _, _) in self
            .control_plane
            .tickets
            .terminal_workflow_media_tickets(invocation.job_id, invocation.workflow_id)
            .await?
        {
            candidate_ids.insert(ticket_id);
        }

        for ticket_id in candidate_ids {
            if state.node_local_settled.contains(&ticket_id) {
                continue;
            }
            let Some(ticket) = self.control_plane.tickets.get(ticket_id).await? else {
                state.node_local_outstanding.remove(&ticket_id);
                continue;
            };
            if !matches!(ticket.state, TicketState::Succeeded | TicketState::Failed) {
                // Still awaiting its storage owner's agent.
                continue;
            }
            let operation = match state.node_local_outstanding.remove(&ticket_id) {
                Some(operation) => operation,
                None => parse_payload(&ticket)?.operation,
            };
            state.node_local_settled.insert(ticket_id);
            match ticket.state {
                TicketState::Succeeded => {
                    state.summary.record_success(operation);
                    self.expand_successful_ticket(
                        invocation.plan,
                        invocation.workflow_id,
                        invocation.job_id,
                        ticket_id,
                    )
                    .await?;
                }
                TicketState::Failed => {
                    let class = self
                        .ticket_failure_class(ticket_id)
                        .await?
                        .unwrap_or(FailureClass::VerificationFailure);
                    state.summary.record_failure(operation, class);
                    let source = VoomError::Internal(format!(
                        "node-locally dispatched {operation:?} ticket {ticket_id} failed \
                         with class {class:?}"
                    ));
                    record_terminal_dispatch_failure(state, source, invocation.failure_mode, false);
                }
                TicketState::Pending | TicketState::Ready | TicketState::Leased => {}
            }
        }
        Ok(())
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
                VoomError::Internal(
                    "accelerator candidate projection omitted live runtimes".to_owned(),
                )
            })?
        } else {
            &self.runtimes
        };
        runtimes.get(worker_id).map(Some)
    }

    pub(super) async fn process_joined_dispatch(
        &self,
        joined: Result<DispatchOutcome, tokio::task::JoinError>,
        identity: DispatchIdentity,
        invocation: &RunInvocation<'_>,
        state: &mut RunLoopState,
    ) {
        match joined {
            Ok(outcome) => {
                self.process_dispatch_outcome(outcome, identity, invocation, state)
                    .await;
            }
            Err(error) => {
                self.process_dispatch_join_error(error, identity, invocation, state)
                    .await;
            }
        }
    }

    async fn process_dispatch_outcome(
        &self,
        outcome: DispatchOutcome,
        identity: DispatchIdentity,
        invocation: &RunInvocation<'_>,
        state: &mut RunLoopState,
    ) {
        if outcome.ticket_id != identity.ticket_id
            || outcome.worker_id != identity.worker_id
            || outcome.operation != identity.operation
        {
            state.record_fatal_error(VoomError::Internal(format!(
                "workflow dispatch identity mismatch: registered {identity:?}, \
                 returned ticket {}, worker {}, operation {:?}",
                outcome.ticket_id, outcome.worker_id, outcome.operation
            )));
            return;
        }
        match outcome.terminal {
            DispatchTerminal::Success => {
                state.summary.record_success(identity.operation);
                if let Err(source) = self
                    .expand_successful_ticket(
                        invocation.plan,
                        invocation.workflow_id,
                        invocation.job_id,
                        identity.ticket_id,
                    )
                    .await
                {
                    state.record_fatal_error(source);
                }
            }
            DispatchTerminal::Failure { source } => {
                self.process_dispatch_failure(identity, source, invocation, state, false)
                    .await;
            }
        }
    }

    async fn process_dispatch_join_error(
        &self,
        error: tokio::task::JoinError,
        identity: DispatchIdentity,
        invocation: &RunInvocation<'_>,
        state: &mut RunLoopState,
    ) {
        let lifecycle_fatal = error.is_cancelled();
        let source = VoomError::WorkerCrash(format!(
            "workflow dispatch task {} failed for ticket {}, worker {}, lease {}, \
             operation {:?}: {error}",
            error.id(),
            identity.ticket_id,
            identity.worker_id,
            identity.lease_id,
            identity.operation
        ));
        if let Err(transition) = fail_lease_with_retry(
            &self.control_plane,
            identity.lease_id,
            source.to_string(),
            FailureClass::WorkerCrash,
        )
        .await
        {
            state
                .summary
                .record_failure(identity.operation, FailureClass::WorkerCrash);
            state.record_fatal_error(join_cleanup_failure(&source, transition, identity));
            return;
        }
        self.process_dispatch_failure(identity, source, invocation, state, lifecycle_fatal)
            .await;
    }

    async fn process_dispatch_failure(
        &self,
        identity: DispatchIdentity,
        source: VoomError,
        invocation: &RunInvocation<'_>,
        state: &mut RunLoopState,
        lifecycle_fatal: bool,
    ) {
        let fallback_class = failure_class_for_error(&source);
        let class = match self
            .resolved_ticket_failure_class(identity.ticket_id, fallback_class)
            .await
        {
            Ok(class) => class,
            Err(error) => {
                state
                    .summary
                    .record_failure(identity.operation, fallback_class);
                state.record_fatal_error(error);
                return;
            }
        };
        state.summary.record_failure(identity.operation, class);
        let ticket = match self.control_plane.tickets.get(identity.ticket_id).await {
            Ok(ticket) => ticket,
            Err(error) => {
                state.record_fatal_error(error);
                return;
            }
        };
        let Some(ticket) = ticket else {
            state.record_fatal_error(VoomError::NotFound(format!(
                "ticket {} vanished after dispatch failure",
                identity.ticket_id
            )));
            return;
        };
        if ticket.state == TicketState::Failed {
            record_terminal_dispatch_failure(
                state,
                source,
                invocation.failure_mode,
                lifecycle_fatal,
            );
        } else if lifecycle_fatal {
            state.record_fatal_error(source);
        }
    }

    async fn resolved_ticket_failure_class(
        &self,
        ticket_id: TicketId,
        fallback: FailureClass,
    ) -> Result<FailureClass, VoomError> {
        Ok(self
            .ticket_failure_class(ticket_id)
            .await?
            .unwrap_or(fallback))
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
        let device_bound = requires_accelerator(requirement.as_ref());
        if device_bound && accelerator_runtimes.is_none() {
            *accelerator_runtimes = Some(self.control_plane.live_policy_runtime_registry().await?);
        }
        let conflicts = conflicting_accelerator_tokens(&candidates);
        let mut workers = Vec::new();
        let mut assignments = HashMap::new();
        for candidate in candidates {
            if device_bound
                && !accelerator_runtimes
                    .as_ref()
                    .is_some_and(|runtimes| runtimes.contains(candidate.worker_id))
            {
                continue;
            }
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
            // A historical row written by a build that knew a backend this one does
            // not must not make the whole token lookup fail.
            let descriptor = match historical_accelerator_descriptor(&capability) {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "skipping historical capability with an unreadable accelerator descriptor"
                    );
                    continue;
                }
            };
            let hardware_token = descriptor.hardware_token();
            let compatible = match (requirement, &descriptor) {
                (
                    VideoHardwareRequirement::Nvidia(required),
                    VideoAcceleratorDescriptor::Nvidia(device),
                ) => {
                    device.encoders.contains(&required.encoder)
                        && required
                            .decoder
                            .as_ref()
                            .is_none_or(|decoder| device.decoders.contains(decoder))
                }
                (
                    VideoHardwareRequirement::Vaapi(required),
                    VideoAcceleratorDescriptor::Vaapi(device),
                ) => {
                    device.encoders.contains(&required.encoder)
                        && required
                            .decoder
                            .as_ref()
                            .is_none_or(|decoder| device.decoders.contains(decoder))
                }
                (
                    VideoHardwareRequirement::VideoToolbox(required),
                    VideoAcceleratorDescriptor::VideoToolbox(device),
                ) => {
                    device.encoders.contains(&required.encoder)
                        && required.decoder.as_ref().is_none_or(|decoder| {
                            videotoolbox_decoder_matches(&device.decoders, decoder)
                        })
                }
                (
                    VideoHardwareRequirement::Software(_)
                    | VideoHardwareRequirement::Nvidia(_)
                    | VideoHardwareRequirement::Vaapi(_)
                    | VideoHardwareRequirement::VideoToolbox(_),
                    _,
                ) => false,
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

fn join_cleanup_failure(
    source: &VoomError,
    transition: VoomError,
    identity: DispatchIdentity,
) -> VoomError {
    let context = format!(
        "{source}; failing lease {} for ticket {}, worker {}, operation {:?} also failed",
        identity.lease_id, identity.ticket_id, identity.worker_id, identity.operation
    );
    match transition {
        VoomError::Database {
            message,
            source: database_source,
        } => VoomError::Database {
            message: format!("{context}: database error: {message}"),
            source: database_source,
        },
        transition => VoomError::Internal(format!("{context}: {transition}")),
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
        VideoHardwareAssignment::Nvidia(nvidia) => nvidia.hardware_token.clone(),
        VideoHardwareAssignment::Vaapi(vaapi) => vaapi.hardware_token.clone(),
        VideoHardwareAssignment::VideoToolbox(videotoolbox) => videotoolbox.hardware_token.clone(),
        VideoHardwareAssignment::Software(_) => {
            return Err(VoomError::Internal(
                "software dispatch unexpectedly carried a hardware assignment".to_owned(),
            ));
        }
    };
    recovery_tokens.push(hardware_token);
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
    let descriptor = voom_core::encoder_descriptor(&profile.encoder).ok_or_else(|| {
        VoomError::Config(format!(
            "transcode profile names unknown encoder `{}`",
            profile.encoder
        ))
    })?;
    match descriptor.backend {
        VideoEncoderBackend::Software => Ok(Some(VideoHardwareRequirement::software())),
        VideoEncoderBackend::Nvidia => Ok(Some(VideoHardwareRequirement::nvidia(
            &profile.encoder,
            nvidia_decode_requirement(&profile, payload)?,
        ))),
        VideoEncoderBackend::Vaapi => Ok(Some(VideoHardwareRequirement::vaapi(
            &profile.encoder,
            vaapi_decode_requirement(&profile, payload)?,
        ))),
        VideoEncoderBackend::VideoToolbox => Ok(Some(VideoHardwareRequirement::video_toolbox(
            &profile.encoder,
            videotoolbox_decode_requirement(&profile, payload)?,
        ))),
    }
}

fn nvidia_decode_requirement(
    profile: &TranscodeVideoProfile,
    payload: &serde_json::Value,
) -> Result<Option<String>, VoomError> {
    if !profile.decode.is_nvidia() {
        return Ok(None);
    }
    let codec = source_video_codec(payload).ok_or_else(|| {
        VoomError::Config("NVIDIA decode requires a known source video codec".to_owned())
    })?;
    let decoder = voom_core::nvidia_decoder_for_video_codec(codec).ok_or_else(|| {
        VoomError::Config(format!(
            "NVIDIA decode does not support source video codec `{codec}`"
        ))
    })?;
    Ok(Some(decoder.to_owned()))
}

/// A VAAPI decode requirement names the canonical *source codec*, not a decoder:
/// `-hwaccel vaapi` uses the codec's own decoder, so the descriptor's `decoders`
/// list holds codecs and the requirement must be spelled the same way to match.
fn vaapi_decode_requirement(
    profile: &TranscodeVideoProfile,
    payload: &serde_json::Value,
) -> Result<Option<String>, VoomError> {
    if !profile.decode.is_vaapi() {
        return Ok(None);
    }
    let codec = source_video_codec(payload).ok_or_else(|| {
        VoomError::Config("VAAPI decode requires a known source video codec".to_owned())
    })?;
    let decode_codec = voom_core::vaapi_video_decode_codec(codec).ok_or_else(|| {
        VoomError::Config(format!(
            "VAAPI decode does not support source video codec `{codec}`"
        ))
    })?;
    Ok(Some(decode_codec.to_owned()))
}

/// A `VideoToolbox` decode requirement carries the source codec *and* its pixel
/// format: the platform advertises decode capability per (codec, pixel format) pair,
/// so naming the codec alone would match a device that cannot take these frames.
fn videotoolbox_decode_requirement(
    profile: &TranscodeVideoProfile,
    payload: &serde_json::Value,
) -> Result<Option<VideoToolboxDecodeRequirement>, VoomError> {
    if !profile.decode.is_video_toolbox() {
        return Ok(None);
    }
    let codec = source_video_codec(payload).ok_or_else(|| {
        VoomError::Config("VideoToolbox decode requires a known source video codec".to_owned())
    })?;
    let pixel_format = source_video_pixel_format(payload).ok_or_else(|| {
        VoomError::Config(
            "VideoToolbox decode requires a known source video pixel format".to_owned(),
        )
    })?;
    Ok(Some(VideoToolboxDecodeRequirement {
        codec: codec.to_owned(),
        pixel_format: pixel_format.to_owned(),
    }))
}

/// A device-bound requirement can only be satisfied by a worker with a live
/// endpoint, because dispatch re-verifies the device's identity before acquiring the
/// lease. Written as an exhaustive match so a fifth backend cannot default to the
/// software path and skip that verification.
const fn requires_accelerator(requirement: Option<&VideoHardwareRequirement>) -> bool {
    match requirement {
        Some(
            VideoHardwareRequirement::Nvidia(_)
            | VideoHardwareRequirement::Vaapi(_)
            | VideoHardwareRequirement::VideoToolbox(_),
        ) => true,
        Some(VideoHardwareRequirement::Software(_)) | None => false,
    }
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

/// Pairs a profile-derived requirement with the device a candidate advertises.
///
/// The match is exhaustive over both the requirement and the descriptor
/// vocabularies with no wildcard arm, so a fifth backend on either side fails to
/// compile rather than inheriting whichever default happened to sit last. A
/// requirement and a descriptor naming different backends are incompatible, never
/// an error: ADR 0049 §6 forbids one worker's hardware from breaking projection for
/// the rest of the fleet.
fn compatible_assignment(
    candidate: &WorkerOperationCandidate,
    requirement: Option<&VideoHardwareRequirement>,
    conflicts: &HashSet<String>,
) -> CandidateCompatibility {
    let Some(requirement) = requirement else {
        return CandidateCompatibility::Compatible(None);
    };
    // A descriptor this build cannot read excludes the candidate outright. It must
    // not become an error — that is what ADR 0049 §6 forbids and what the comment
    // above promises — and it must not read as "no accelerator" either, which is
    // why this returns `Incompatible` rather than falling through with `None`: a
    // device-bound worker passing as unaccelerated is the ADR 0049 §5 hazard.
    let descriptor = match candidate_accelerator_descriptor(candidate) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            tracing::warn!(
                worker_id = candidate.worker_id.0,
                %error,
                "excluding candidate with an unreadable accelerator descriptor"
            );
            return CandidateCompatibility::Incompatible;
        }
    };
    match (requirement, &descriptor) {
        (VideoHardwareRequirement::Software(_), None) if candidate.hardware.is_empty() => {
            CandidateCompatibility::Compatible(None)
        }
        (VideoHardwareRequirement::Software(_), None | Some(_))
        | (
            VideoHardwareRequirement::Nvidia(_)
            | VideoHardwareRequirement::Vaapi(_)
            | VideoHardwareRequirement::VideoToolbox(_),
            None,
        )
        | (
            VideoHardwareRequirement::Nvidia(_),
            Some(
                VideoAcceleratorDescriptor::Vaapi(_) | VideoAcceleratorDescriptor::VideoToolbox(_),
            ),
        )
        | (
            VideoHardwareRequirement::Vaapi(_),
            Some(
                VideoAcceleratorDescriptor::Nvidia(_) | VideoAcceleratorDescriptor::VideoToolbox(_),
            ),
        )
        | (
            VideoHardwareRequirement::VideoToolbox(_),
            Some(VideoAcceleratorDescriptor::Nvidia(_) | VideoAcceleratorDescriptor::Vaapi(_)),
        ) => CandidateCompatibility::Incompatible,
        (
            VideoHardwareRequirement::Nvidia(required),
            Some(VideoAcceleratorDescriptor::Nvidia(device)),
        ) => nvidia_compatibility(candidate, conflicts, required, device),
        (
            VideoHardwareRequirement::Vaapi(required),
            Some(VideoAcceleratorDescriptor::Vaapi(device)),
        ) => vaapi_compatibility(candidate, conflicts, required, device),
        (
            VideoHardwareRequirement::VideoToolbox(required),
            Some(VideoAcceleratorDescriptor::VideoToolbox(device)),
        ) => videotoolbox_compatibility(candidate, conflicts, required, device),
    }
}

fn nvidia_compatibility(
    candidate: &WorkerOperationCandidate,
    conflicts: &HashSet<String>,
    required: &NvidiaVideoHardwareRequirement,
    device: &NvidiaVideoAcceleratorDescriptor,
) -> CandidateCompatibility {
    if conflicts.contains(&device.hardware_token)
        || !candidate.hardware.contains(&device.hardware_token)
        || !device.encoders.contains(&required.encoder)
        || required
            .decoder
            .as_ref()
            .is_some_and(|decoder| !device.decoders.contains(decoder))
    {
        return CandidateCompatibility::Incompatible;
    }
    CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::nvidia(
        device.hardware_token.clone(),
        device.device_uuid.clone(),
    )))
}

/// A VAAPI requirement matches only a live, identity-verified VAAPI descriptor on
/// the same device: the token derived from the descriptor's PCI address must be one
/// the candidate still advertises in `hardware`, so the assignment names the device
/// the worker actually bound (ADR 0052 §1). A VAAPI-decode requirement additionally
/// needs the source codec to have decoded on that device at startup — the
/// descriptor lists codecs, not decoder names, because `-hwaccel vaapi` has none.
fn vaapi_compatibility(
    candidate: &WorkerOperationCandidate,
    conflicts: &HashSet<String>,
    required: &VaapiVideoHardwareRequirement,
    device: &VaapiVideoAcceleratorDescriptor,
) -> CandidateCompatibility {
    let token = vaapi_hardware_token(&device.pci_address);
    if conflicts.contains(&token)
        || !candidate.hardware.contains(&token)
        || !device.encoders.contains(&required.encoder)
        || required
            .decoder
            .as_ref()
            .is_some_and(|codec| !device.decoders.contains(codec))
    {
        return CandidateCompatibility::Incompatible;
    }
    CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::vaapi(
        token,
        device.pci_address.clone(),
    )))
}

/// A `VideoToolbox` requirement matches only a host-bound `VideoToolbox` descriptor
/// whose advertised decode capability covers the exact (codec, pixel format) pair the
/// source needs — the platform advertises decode per pair, not per codec.
fn videotoolbox_compatibility(
    candidate: &WorkerOperationCandidate,
    conflicts: &HashSet<String>,
    required: &VideoToolboxVideoHardwareRequirement,
    device: &VideoToolboxVideoAcceleratorDescriptor,
) -> CandidateCompatibility {
    if conflicts.contains(&device.hardware_token)
        || !candidate.hardware.contains(&device.hardware_token)
        || !device.encoders.contains(&required.encoder)
        || required
            .decoder
            .as_ref()
            .is_some_and(|decoder| !videotoolbox_decoder_matches(&device.decoders, decoder))
    {
        return CandidateCompatibility::Incompatible;
    }
    CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::video_toolbox(
        device.hardware_token.clone(),
        device.resource_id.clone(),
    )))
}

fn conflicting_accelerator_tokens(candidates: &[WorkerOperationCandidate]) -> HashSet<String> {
    let mut capacities = HashMap::new();
    let mut conflicts = HashSet::new();
    for candidate in candidates {
        // Same rule as `compatible_assignment`: an unreadable descriptor drops that
        // one candidate out of the conflict survey rather than failing the survey.
        let descriptor = match candidate_accelerator_descriptor(candidate) {
            Ok(Some(descriptor)) => descriptor,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    worker_id = candidate.worker_id.0,
                    %error,
                    "skipping candidate with an unreadable accelerator descriptor"
                );
                continue;
            }
        };
        let token = descriptor.hardware_token();
        if let Some(capacity) = capacities.insert(token.clone(), descriptor.max_sessions())
            && capacity != descriptor.max_sessions()
        {
            conflicts.insert(token);
        }
    }
    conflicts
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

/// The ADR 0075 routing gate: an envelope-bearing byte-touching ticket
/// executes ONLY through its storage owner's agent via the remote-lease flow.
/// The bundled executor never leases or pushes it, so no candidate projection,
/// accelerator reservation, or local runtime applies; the ticket stays `ready`
/// until an agent takes it. Tickets still rendered without the
/// `media_dispatch` object keep the pre-ADR-0075 path until the payload
/// renderers flip.
fn node_local_dispatch_outcome(
    state: &mut RunLoopState,
    ticket_id: TicketId,
    workflow_payload: &WorkflowTicketPayload,
) -> Option<SpawnOutcome> {
    if !workflow_payload.operation.is_node_local_media_dispatch()
        || workflow_payload
            .rendered_payload
            .get("media_dispatch")
            .is_none()
    {
        return None;
    }
    state
        .node_local_outstanding
        .insert(ticket_id, workflow_payload.operation);
    Some(SpawnOutcome::NodeLocalDispatched)
}

fn record_terminal_dispatch_failure(
    state: &mut RunLoopState,
    source: VoomError,
    failure_mode: RunFailureMode,
    lifecycle_fatal: bool,
) {
    if lifecycle_fatal || failure_mode == RunFailureMode::AbortJob {
        state.record_fatal_error(source);
    } else if state.isolated_error.is_none() {
        state.isolated_error = Some(source);
    }
}

pub(super) fn decrement_reservation(
    reservations: &mut HashMap<WorkerId, u32>,
    worker_id: WorkerId,
) {
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
