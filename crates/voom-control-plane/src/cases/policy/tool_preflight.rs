use std::collections::BTreeSet;
use std::future::Future;

use voom_core::{
    OperationKind, PROTOCOL_VERSION, TicketOperation, VideoDecodeMode, VideoEncoderBackend,
    VoomError,
};
use voom_policy::compiled::CompiledTranscodeVideoOperation;
use voom_policy::{CompiledOperation, CompiledPolicy, PolicyTool, VideoProfileRef};
use voom_store::repo::execution::workers::{Worker, WorkerKind, WorkerStatus};
use voom_worker_protocol::VideoAcceleratorDescriptor;

use crate::ControlPlane;
use crate::cases::{begin_immediate_tx, commit_tx};
use crate::scan::worker::ScanWorkerError;
use crate::video_hardware::candidate_accelerator_descriptor;
use crate::workflow::WorkerRuntimeRegistry;

const FFMPEG_NAME: &str = "local-ffmpeg";
const FFMPEG_PREFIX: &str = "local-ffmpeg-";
const MKVTOOLNIX_NAME: &str = "local-mkvtoolnix";
const MKVTOOLNIX_PREFIX: &str = "local-mkvtoolnix-";
const LEGACY_REQUIRES_TOOLS_WARNING: &str = "metadata_requires_tools_deferred";

struct UnavailableTool {
    tool: PolicyTool,
    reason: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EligibilityFinding {
    MissingCapability,
    MissingGrant,
    Denied,
    Effective,
}

impl ControlPlane {
    pub(crate) async fn preflight_policy_tools(
        &self,
        policy: &mut CompiledPolicy,
        runtimes: &WorkerRuntimeRegistry,
    ) -> Result<(), VoomError> {
        self.preflight_policy_tools_with_ffprobe_readiness(
            policy,
            runtimes,
            crate::scan::worker::verify_bundled_ffprobe_readiness,
        )
        .await
    }

    async fn preflight_policy_tools_with_ffprobe_readiness<F, Fut>(
        &self,
        policy: &mut CompiledPolicy,
        runtimes: &WorkerRuntimeRegistry,
        ffprobe_readiness: F,
    ) -> Result<(), VoomError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<(), ScanWorkerError>>,
    {
        let tools = normalize_policy_tool_requirements(policy)?;

        let mut unavailable = Vec::new();
        for tool in tools {
            let reason = match tool {
                PolicyTool::Ffmpeg => {
                    self.observe_endpoint_tool(
                        FFMPEG_NAME,
                        FFMPEG_PREFIX,
                        &[
                            OperationKind::TranscodeVideo,
                            OperationKind::TranscodeAudio,
                            OperationKind::ExtractAudio,
                        ],
                        runtimes,
                    )
                    .await?
                }
                PolicyTool::Mkvtoolnix => {
                    self.observe_endpoint_tool(
                        MKVTOOLNIX_NAME,
                        MKVTOOLNIX_PREFIX,
                        &[OperationKind::Remux],
                        runtimes,
                    )
                    .await?
                }
                PolicyTool::Ffprobe => {
                    self.observe_bundled_ffprobe(ffprobe_readiness().await)
                        .await?
                }
            };
            if let Some(reason) = reason {
                unavailable.push(UnavailableTool { tool, reason });
            }
        }
        if !unavailable.is_empty() {
            return Err(VoomError::PolicyExecution(format_unavailable_tools(
                &policy.slug,
                &unavailable,
            )));
        }
        self.preflight_video_hardware(policy, runtimes).await
    }

    async fn preflight_video_hardware(
        &self,
        policy: &CompiledPolicy,
        runtimes: &WorkerRuntimeRegistry,
    ) -> Result<(), VoomError> {
        let requirements = policy_video_backend_requirements(policy)?;
        if !requirements.software
            && !requirements.nvidia.required
            && !requirements.vaapi.required
            && !requirements.videotoolbox.required
        {
            return Ok(());
        }
        let candidates = self
            .workers
            .operation_candidates(&TicketOperation::from(OperationKind::TranscodeVideo))
            .await?;
        let mut availability = BackendAvailability::new();
        for candidate in candidates {
            if runtimes.get_optional(candidate.worker_id).is_none() {
                continue;
            }
            // ADR 0049 §6: a problem with one worker never escapes candidate
            // projection as a job-fatal error. A descriptor this build cannot
            // parse — a newer worker's backend tag, or a field added since —
            // makes that one worker contribute no availability, which fails
            // closed: dispatch still refuses it, and the operator gets
            // "no worker advertises <backend>" instead of a repository error
            // that blocks every policy on the fleet.
            let descriptor = match candidate_accelerator_descriptor(&candidate) {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    tracing::warn!(
                        worker_id = candidate.worker_id.0,
                        %error,
                        "skipping worker with an unreadable accelerator descriptor \
                         during video hardware preflight"
                    );
                    continue;
                }
            };
            match descriptor {
                Some(VideoAcceleratorDescriptor::Nvidia(device)) => {
                    let has_encoder = device
                        .encoders
                        .iter()
                        .any(|encoder| encoder == "hevc_nvenc");
                    let has_decoder =
                        !requirements.nvidia.hardware_decode || !device.decoders.is_empty();
                    if has_encoder && has_decoder {
                        availability.insert(VideoEncoderBackend::Nvidia);
                    }
                }
                Some(VideoAcceleratorDescriptor::Vaapi(device)) => {
                    let has_encoder = device
                        .encoders
                        .iter()
                        .any(|encoder| encoder == "hevc_vaapi");
                    let has_decoder =
                        !requirements.vaapi.hardware_decode || !device.decoders.is_empty();
                    if has_encoder && has_decoder {
                        availability.insert(VideoEncoderBackend::Vaapi);
                    }
                }
                Some(VideoAcceleratorDescriptor::VideoToolbox(device)) => {
                    let has_encoders = requirements
                        .videotoolbox_encoders
                        .iter()
                        .all(|required| device.encoders.iter().any(|item| item == required));
                    let has_decoder =
                        !requirements.videotoolbox.hardware_decode || !device.decoders.is_empty();
                    if has_encoders && has_decoder {
                        availability.insert(VideoEncoderBackend::VideoToolbox);
                    }
                }
                None if candidate.hardware.is_empty() => {
                    availability.insert(VideoEncoderBackend::Software);
                }
                None => {}
            }
        }
        let missing = missing_backend_workers(&requirements, &availability);
        if missing.is_empty() {
            return Ok(());
        }
        Err(VoomError::PolicyExecution(format!(
            "video hardware preflight failed for policy `{}`:\n- {}",
            policy.slug,
            missing.join("\n- ")
        )))
    }

    async fn observe_endpoint_tool(
        &self,
        legacy_name: &str,
        prefix: &str,
        operations: &[OperationKind],
        runtimes: &WorkerRuntimeRegistry,
    ) -> Result<Option<String>, VoomError> {
        let workers = self
            .workers
            .list_by_name_namespace(legacy_name, prefix)
            .await?;
        validate_reserved_workers(&workers)?;
        if workers.is_empty() {
            return Ok(Some("no reserved local provider is registered".to_owned()));
        }

        let live: Vec<&Worker> = workers.iter().filter(|worker| is_live(worker)).collect();
        if live.is_empty() {
            return Ok(Some(
                "every reserved provider is stale or retired".to_owned(),
            ));
        }

        let mut effective = Vec::new();
        let mut findings = Vec::new();
        for worker in live {
            let worker_findings = self.worker_eligibility_findings(worker, operations).await?;
            if worker_findings.contains(&EligibilityFinding::Effective) {
                effective.push(worker);
            }
            findings.extend(worker_findings);
        }
        if effective.is_empty() {
            return Ok(Some(unavailable_eligibility_reason(&findings)));
        }

        let mut identity_failures = Vec::new();
        for worker in effective {
            let Some(runtime) = runtimes.get_optional(worker.id) else {
                identity_failures.push(format!("{} has no live endpoint", worker.name));
                continue;
            };
            match runtime.client.handshake(PROTOCOL_VERSION).await {
                Ok(response) if response.agreed == PROTOCOL_VERSION => {}
                Ok(response) => {
                    identity_failures.push(format!(
                        "{} handshake agreed to protocol {}",
                        worker.name, response.agreed
                    ));
                    continue;
                }
                Err(error) => {
                    identity_failures.push(format!("{} handshake failed: {error}", worker.name));
                    continue;
                }
            }
            match runtime.client.identity(&runtime.credentials).await {
                Ok(identity)
                    if identity.worker_id == worker.id
                        && identity.worker_epoch == worker.epoch
                        && identity.protocol_version == PROTOCOL_VERSION =>
                {
                    return Ok(None);
                }
                Ok(identity) => identity_failures.push(format!(
                    "{} returned identity {}:{} at protocol {}",
                    worker.name,
                    identity.worker_id,
                    identity.worker_epoch,
                    identity.protocol_version
                )),
                Err(error) => {
                    identity_failures.push(format!("{} identity failed: {error}", worker.name));
                }
            }
        }
        Ok(Some(identity_failures.join("; ")))
    }

    async fn worker_eligibility_findings(
        &self,
        worker: &Worker,
        operations: &[OperationKind],
    ) -> Result<Vec<EligibilityFinding>, VoomError> {
        let mut findings = Vec::with_capacity(operations.len());
        for operation in operations {
            let operation = TicketOperation::from(*operation);
            let eligibility = self
                .workers
                .operation_eligibility(worker.id, &operation)
                .await?;
            let finding = if !eligibility.has_capability {
                EligibilityFinding::MissingCapability
            } else if eligibility.is_denied {
                EligibilityFinding::Denied
            } else if !eligibility.has_grant {
                EligibilityFinding::MissingGrant
            } else {
                EligibilityFinding::Effective
            };
            findings.push(finding);
        }
        Ok(findings)
    }

    async fn observe_bundled_ffprobe(
        &self,
        readiness: Result<(), ScanWorkerError>,
    ) -> Result<Option<String>, VoomError> {
        if let Err(error) = readiness {
            return Ok(Some(format!("bundled ffprobe readiness failed: {error}")));
        }

        let mut tx = begin_immediate_tx(&self.pool).await?;
        let worker =
            crate::scan::bootstrap::resolve_builtin_ffprobe_worker_in_tx(self, &mut tx).await?;
        let operation = TicketOperation::from(OperationKind::ProbeFile);
        let mut eligibility = self
            .workers
            .operation_eligibility_in_tx(&mut tx, worker.id, &operation)
            .await?;
        if eligibility.is_denied {
            commit_tx(tx).await?;
            return Ok(Some(format!(
                "live built-in provider {} is denied {}",
                worker.name,
                operation.as_str()
            )));
        }
        if !eligibility.has_capability || !eligibility.has_grant {
            crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(self, &mut tx).await?;
            eligibility = self
                .workers
                .operation_eligibility_in_tx(&mut tx, worker.id, &operation)
                .await?;
        }
        if !eligibility.has_capability || !eligibility.has_grant || eligibility.is_denied {
            return Err(VoomError::Conflict(format!(
                "built-in ffprobe worker {} could not establish effective {} eligibility",
                worker.name,
                operation.as_str()
            )));
        }
        commit_tx(tx).await?;
        Ok(None)
    }
}

/// The backends that currently have a live, capable worker. A set rather than one
/// flag per backend so adding a backend needs no new field here.
type BackendAvailability = BTreeSet<VideoEncoderBackend>;

/// One operator-actionable line per required backend that has no live worker.
fn missing_backend_workers(
    requirements: &VideoBackendRequirements,
    availability: &BackendAvailability,
) -> Vec<String> {
    let mut missing = Vec::new();
    if requirements.software && !availability.contains(&VideoEncoderBackend::Software) {
        missing.push(
            "software transcode profiles require an unbound ffmpeg worker; \
             start one with: voom worker run-local --kind ffmpeg"
                .to_owned(),
        );
    }
    if requirements.nvidia.required && !availability.contains(&VideoEncoderBackend::Nvidia) {
        let decoder = if requirements.nvidia.hardware_decode {
            " with at least one advertised CUVID decoder"
        } else {
            ""
        };
        missing.push(format!(
            "hevc_nvenc profiles require a live NVIDIA-bound ffmpeg worker{decoder}; \
             start one with: voom worker run-local --kind ffmpeg \
             --nvidia-device GPU-<uuid>"
        ));
    }
    if requirements.vaapi.required && !availability.contains(&VideoEncoderBackend::Vaapi) {
        let decoder = if requirements.vaapi.hardware_decode {
            " with at least one probed VAAPI decoder"
        } else {
            ""
        };
        missing.push(format!(
            "hevc_vaapi profiles require a live VAAPI-bound ffmpeg worker{decoder}; \
             start one with: voom worker run-local --kind ffmpeg \
             --vaapi-device <pci-address>"
        ));
    }
    if requirements.videotoolbox.required
        && !availability.contains(&VideoEncoderBackend::VideoToolbox)
    {
        let encoders = requirements
            .videotoolbox_encoders
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let decoder = if requirements.videotoolbox.hardware_decode {
            " with at least one advertised VideoToolbox decoder"
        } else {
            ""
        };
        missing.push(format!(
            "VideoToolbox profiles require a live host-bound ffmpeg worker advertising \
             [{encoders}]{decoder}; start one with: voom worker run-local --kind ffmpeg \
             --videotoolbox"
        ));
    }
    missing
}

/// What one accelerator backend's profiles need from the fleet: whether any profile
/// targets it at all, and whether any of those also selects hardware decode, which
/// needs a device that probed a decoder as well as an encoder.
#[derive(Default)]
struct BackendRequirement {
    required: bool,
    hardware_decode: bool,
}

/// Software has no decode variant to track: software decode is the omitted default
/// and needs nothing from a device.
#[derive(Default)]
struct VideoBackendRequirements {
    software: bool,
    nvidia: BackendRequirement,
    vaapi: BackendRequirement,
    videotoolbox: BackendRequirement,
    /// `VideoToolbox` is the one backend with more than one encoder, so preflight
    /// must check the device advertises the specific encoders the policy names
    /// rather than only that some `VideoToolbox` device is bound.
    videotoolbox_encoders: BTreeSet<String>,
}

fn policy_video_backend_requirements(
    policy: &CompiledPolicy,
) -> Result<VideoBackendRequirements, VoomError> {
    let mut requirements = VideoBackendRequirements::default();
    for phase in &policy.phases {
        collect_video_backend_requirements(&phase.operations, &mut requirements)?;
    }
    Ok(requirements)
}

fn collect_video_backend_requirements(
    operations: &[CompiledOperation],
    requirements: &mut VideoBackendRequirements,
) -> Result<(), VoomError> {
    for operation in operations {
        match operation {
            CompiledOperation::TranscodeVideo(operation) => {
                record_transcode_video_requirement(operation, requirements)?;
            }
            CompiledOperation::Conditional(conditional) => {
                collect_video_backend_requirements(&conditional.operations, requirements)?;
            }
            CompiledOperation::Rules(rules) => {
                for rule in &rules.rules {
                    collect_video_backend_requirements(&rule.operations, requirements)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Which backend a profile needs is its encoder descriptor's `backend`, never a
/// string comparison against one encoder name. Classifying by name gives every
/// unrecognized hardware encoder a *software* requirement, so it would be
/// preflighted and dispatched against a CPU worker — the silent software fallback
/// issue #409 forbids. An encoder with no descriptor cannot be classified at all
/// and fails loud.
fn record_transcode_video_requirement(
    operation: &CompiledTranscodeVideoOperation,
    requirements: &mut VideoBackendRequirements,
) -> Result<(), VoomError> {
    let (encoder, decode) = transcode_video_encoder_and_decode(operation)?;
    let descriptor = voom_core::encoder_descriptor(encoder).ok_or_else(|| {
        VoomError::PolicyExecution(format!(
            "video hardware preflight cannot classify unknown encoder `{encoder}`"
        ))
    })?;
    match descriptor.backend {
        VideoEncoderBackend::Software => requirements.software = true,
        VideoEncoderBackend::Nvidia => {
            requirements.nvidia.required = true;
            requirements.nvidia.hardware_decode |= decode.is_nvidia();
        }
        VideoEncoderBackend::Vaapi => {
            requirements.vaapi.required = true;
            requirements.vaapi.hardware_decode |= decode.is_vaapi();
        }
        VideoEncoderBackend::VideoToolbox => {
            requirements.videotoolbox.required = true;
            requirements.videotoolbox.hardware_decode |= decode.is_video_toolbox();
            requirements
                .videotoolbox_encoders
                .insert(encoder.to_owned());
        }
    }
    Ok(())
}

fn transcode_video_encoder_and_decode(
    operation: &CompiledTranscodeVideoOperation,
) -> Result<(&str, &VideoDecodeMode), VoomError> {
    if let Some(profile) = &operation.resolved_profile {
        return Ok((profile.encoder.as_str(), &profile.decode));
    }
    match &operation.profile {
        VideoProfileRef::Inline(settings) => Ok((settings.encoder.as_str(), &settings.decode)),
        VideoProfileRef::Named(_) => Err(VoomError::PolicyExecution(
            "video hardware preflight requires resolved named profiles".to_owned(),
        )),
    }
}

fn unavailable_eligibility_reason(findings: &[EligibilityFinding]) -> String {
    if findings.contains(&EligibilityFinding::Denied) {
        return "at least one matching live reserved capability is denied and none is effective"
            .to_owned();
    }
    if findings.contains(&EligibilityFinding::MissingGrant) {
        return "live reserved providers have the capability but no execute grant".to_owned();
    }
    "live reserved providers do not advertise a matching capability".to_owned()
}

/// Type stored tool metadata and remove the superseded deferred warning in memory.
///
/// # Errors
/// Returns `PolicyExecution` when a stored requirement is malformed or unknown.
pub(crate) fn normalize_policy_tool_requirements(
    policy: &mut CompiledPolicy,
) -> Result<Vec<PolicyTool>, VoomError> {
    let tools = policy.required_tools().map_err(|error| {
        VoomError::PolicyExecution(format!(
            "policy `{}` has invalid tool requirements: {error}",
            policy.slug
        ))
    })?;
    policy
        .warnings
        .retain(|warning| warning.code != LEGACY_REQUIRES_TOOLS_WARNING);
    Ok(tools)
}

fn validate_reserved_workers(workers: &[Worker]) -> Result<(), VoomError> {
    for worker in workers {
        if worker.kind != WorkerKind::Local || worker.node_id.is_some() {
            return Err(VoomError::Conflict(format!(
                "reserved provider {} must be node-less and local",
                worker.name
            )));
        }
    }
    Ok(())
}

fn is_live(worker: &Worker) -> bool {
    worker.status == WorkerStatus::Registered || worker.status == WorkerStatus::Active
}

fn format_unavailable_tools(policy_slug: &str, unavailable: &[UnavailableTool]) -> String {
    let mut message = format!("tool requirement preflight failed for policy `{policy_slug}`:");
    for item in unavailable {
        message.push_str("\n- ");
        message.push_str(item.tool.as_str());
        message.push_str(": ");
        message.push_str(&item.reason);
        message.push_str("; ");
        message.push_str(guidance(item.tool));
    }
    message
}

const fn guidance(tool: PolicyTool) -> &'static str {
    match tool {
        PolicyTool::Ffmpeg => "start one with: voom worker run-local --kind ffmpeg",
        PolicyTool::Mkvtoolnix => "start one with: voom worker run-local --kind mkvtoolnix",
        PolicyTool::Ffprobe => {
            "verify the bundled ffprobe worker and dependency; remove a probe_file deny or retire \
             the denied built-in incarnation"
        }
    }
}

#[cfg(test)]
#[path = "tool_preflight_test.rs"]
mod tests;
