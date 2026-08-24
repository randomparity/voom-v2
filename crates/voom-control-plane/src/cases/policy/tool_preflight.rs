use std::collections::{BTreeMap, BTreeSet};

use voom_core::{
    FileVersionId, NodeId, OperationKind, TicketOperation, VideoDecodeMode, VideoEncoderBackend,
    VoomError, WorkerId,
};
use voom_policy::compiled::CompiledTranscodeVideoOperation;
use voom_policy::{CompiledOperation, CompiledPolicy, PolicyTool, VideoProfileRef};
use voom_store::repo::execution::artifact_access_resolution::{
    AccessResolutionError, resolve_file_location,
};
use voom_store::repo::execution::nodes::NodeStatus;
use voom_store::repo::execution::workers::{Worker, WorkerOperationCandidate};
use voom_worker_protocol::VideoAcceleratorDescriptor;

use crate::ControlPlane;
use crate::operation_source::select_location;
use crate::video_hardware::candidate_accelerator_descriptor;

const LEGACY_REQUIRES_TOOLS_WARNING: &str = "metadata_requires_tools_deferred";

/// One stored policy target whose storage owner must satisfy the tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PolicyToolTarget {
    pub(crate) ordinal: u32,
    pub(crate) file_version_id: FileVersionId,
}

struct UnavailableTool {
    subject: String,
    reason: String,
    guidance: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EligibilityFinding {
    MissingCapability,
    MissingGrant,
    Denied,
    Effective,
}

/// Where one stored target's storage lives, or why it cannot dispatch.
enum TargetOwner {
    Owned(NodeId),
    Unavailable(UnavailableTool),
}

struct OwnerWorkerSet {
    node_name: String,
    live_worker_ids: BTreeSet<WorkerId>,
}

/// Why the node itself cannot vouch for any worker right now.
fn node_readiness_problem(
    node: &voom_store::repo::execution::nodes::Node,
    now: time::OffsetDateTime,
) -> Option<String> {
    if node.status == NodeStatus::Stale || node.status == NodeStatus::Retired {
        return crate::cases::execution::remote_execution::validate_remote_node_freshness(
            node.status,
            node.last_seen_at,
            node.heartbeat_ttl_seconds,
            node.id,
            now,
            true,
        )
        .err()
        .map(|error| error.to_string());
    }
    if node.active_incarnation_id.is_none() {
        return Some("owner node has no active agent incarnation".to_owned());
    }
    crate::cases::execution::remote_execution::validate_remote_node_freshness(
        node.status,
        node.last_seen_at,
        node.heartbeat_ttl_seconds,
        node.id,
        now,
        true,
    )
    .err()
    .map(|error| error.to_string())
}

fn push_all_unavailable(
    unavailable: &mut Vec<UnavailableTool>,
    tools: &[PolicyTool],
    node_name: &str,
    reason: &str,
) {
    for tool in tools {
        unavailable.push(UnavailableTool {
            subject: format!("{} on node \"{node_name}\"", tool.as_str()),
            reason: reason.to_owned(),
            guidance: guidance(*tool),
        });
    }
}

impl ControlPlane {
    pub(crate) async fn preflight_policy_tools(
        &self,
        policy: &mut CompiledPolicy,
        targets: &[PolicyToolTarget],
    ) -> Result<(), VoomError> {
        let tools = normalize_policy_tool_requirements(policy)?;
        let video_requirements = policy_video_backend_requirements(policy)?;
        if tools.is_empty() && !video_requirements.any() {
            return Ok(());
        }

        let mut unavailable = Vec::new();
        let mut owners: BTreeSet<NodeId> = BTreeSet::new();
        for target in targets {
            match self.resolve_target_owner(target).await? {
                TargetOwner::Owned(node_id) => {
                    owners.insert(node_id);
                }
                TargetOwner::Unavailable(item) => unavailable.push(item),
            }
        }
        let mut owner_workers = BTreeMap::new();
        for node_id in &owners {
            let workers = self
                .observe_node_tools(*node_id, &tools, &mut unavailable)
                .await?;
            owner_workers.insert(*node_id, workers);
        }
        if !unavailable.is_empty() {
            return Err(VoomError::PolicyExecution(format_unavailable_tools(
                &policy.slug,
                &unavailable,
            )));
        }
        self.preflight_video_hardware(&policy.slug, &video_requirements, &owner_workers)
            .await
    }

    /// Resolve one stored target to the node that owns its storage.
    ///
    /// A target that can never dispatch — no live rooted location, or a root
    /// nobody owns — is an unavailable observation rather than a host guess
    /// (ADR 0076 §1). Database failures propagate and abort observation.
    async fn resolve_target_owner(
        &self,
        target: &PolicyToolTarget,
    ) -> Result<TargetOwner, VoomError> {
        let subject = format!("target {}", target.ordinal);
        let location = match select_location(self, target.file_version_id, None).await {
            Ok(location) => location,
            Err(error @ VoomError::Config(_)) => {
                return Ok(TargetOwner::Unavailable(UnavailableTool {
                    subject,
                    reason: error.to_string(),
                    guidance: "re-scan or re-ingest the file onto an owned storage root",
                }));
            }
            Err(error) => return Err(error),
        };
        let (storage_root_id, _) = location.rooted_address()?;
        let resolved = match resolve_file_location(&self.pool, location.id, storage_root_id).await {
            Ok(resolved) => resolved,
            Err(
                error @ (AccessResolutionError::InvalidRootState { .. }
                | AccessResolutionError::InvalidLocationState { .. }),
            ) => {
                return Ok(TargetOwner::Unavailable(UnavailableTool {
                    subject: format!("{subject} (storage root {})", storage_root_id.0),
                    reason: error.to_string(),
                    guidance: "restore the storage root before executing",
                }));
            }
            Err(
                error @ (AccessResolutionError::InvalidRootEpoch { .. }
                | AccessResolutionError::DatabaseError(_)
                | AccessResolutionError::StorageRootNotFound { .. }
                | AccessResolutionError::FileLocationNotFound { .. }
                | AccessResolutionError::LocationRootInvalid { .. }
                | AccessResolutionError::MixedOwner { .. }
                | AccessResolutionError::NoActiveIncarnation { .. }),
            ) => {
                return Err(VoomError::database(format!(
                    "resolve policy target storage owner: {error}"
                )));
            }
        };
        let owner_node_id = u64::try_from(resolved.owner_node_id).map_err(|_| {
            VoomError::database(format!(
                "storage root {} owner node id was negative: {}",
                storage_root_id.0, resolved.owner_node_id
            ))
        })?;
        Ok(TargetOwner::Owned(NodeId(owner_node_id)))
    }

    /// Observe every required tool against one owner node (ADR 0076 §2).
    ///
    /// Every unavailable finding is appended; database errors propagate and
    /// abort observation immediately with their specific context.
    async fn observe_node_tools(
        &self,
        node_id: NodeId,
        tools: &[PolicyTool],
        unavailable: &mut Vec<UnavailableTool>,
    ) -> Result<OwnerWorkerSet, VoomError> {
        let node = self.nodes.get(node_id).await?.ok_or_else(|| {
            VoomError::database(format!("owner node {node_id} disappeared during preflight"))
        })?;
        if let Some(reason) = node_readiness_problem(&node, self.clock().now()) {
            push_all_unavailable(unavailable, tools, &node.name, &reason);
            return Ok(OwnerWorkerSet {
                node_name: node.name,
                live_worker_ids: BTreeSet::new(),
            });
        }
        let Some(active_incarnation) = node.active_incarnation_id.as_ref() else {
            return Ok(OwnerWorkerSet {
                node_name: node.name,
                live_worker_ids: BTreeSet::new(),
            });
        };
        let workers = self.workers.list_by_node(node_id).await?;
        let bound: Vec<&Worker> = workers
            .iter()
            .filter(|worker| worker.node_incarnation_id.as_ref() == Some(active_incarnation))
            .collect();
        let live: Vec<&Worker> = bound
            .iter()
            .copied()
            .filter(|worker| is_live(worker))
            .collect();
        if bound.is_empty() || live.is_empty() {
            let reason = if workers.is_empty() {
                "no agent-supervised workers are registered".to_owned()
            } else if bound.is_empty() {
                format!(
                    "none of the node's {} registered worker(s) is bound to its \
                     active incarnation",
                    workers.len()
                )
            } else {
                "every worker bound to the active incarnation is declared but not ready, stale, or retired"
                    .to_owned()
            };
            push_all_unavailable(unavailable, tools, &node.name, &reason);
            return Ok(OwnerWorkerSet {
                node_name: node.name,
                live_worker_ids: BTreeSet::new(),
            });
        }
        for tool in tools {
            let mut findings = Vec::new();
            for worker in &live {
                findings.extend(
                    self.worker_eligibility_findings(worker, tool_operations(*tool))
                        .await?,
                );
            }
            if !findings.contains(&EligibilityFinding::Effective) {
                unavailable.push(UnavailableTool {
                    subject: format!("{} on node \"{}\"", tool.as_str(), node.name),
                    reason: unavailable_eligibility_reason(&findings),
                    guidance: guidance(*tool),
                });
            }
        }
        Ok(OwnerWorkerSet {
            node_name: node.name,
            live_worker_ids: live.iter().map(|worker| worker.id).collect(),
        })
    }

    async fn preflight_video_hardware(
        &self,
        policy_slug: &str,
        requirements: &VideoBackendRequirements,
        owner_workers: &BTreeMap<NodeId, OwnerWorkerSet>,
    ) -> Result<(), VoomError> {
        if !requirements.any() {
            return Ok(());
        }
        let candidates = self
            .workers
            .operation_candidates(&TicketOperation::from(OperationKind::TranscodeVideo))
            .await?;
        let mut missing = Vec::new();
        for owner in owner_workers.values() {
            let availability =
                backend_availability(&candidates, &owner.live_worker_ids, requirements);
            missing.extend(missing_backend_workers(
                requirements,
                &availability,
                &owner.node_name,
            ));
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(VoomError::PolicyExecution(format!(
            "video hardware preflight failed for policy `{policy_slug}`:\n- {}",
            missing.join("\n- ")
        )))
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
}

/// The backends that currently have a live, capable worker. A set rather than one
/// flag per backend so adding a backend needs no new field here.
type BackendAvailability = BTreeSet<VideoEncoderBackend>;

fn backend_availability(
    candidates: &[WorkerOperationCandidate],
    proven: &BTreeSet<WorkerId>,
    requirements: &VideoBackendRequirements,
) -> BackendAvailability {
    let mut availability = BackendAvailability::new();
    for candidate in candidates {
        if !proven.contains(&candidate.worker_id) {
            continue;
        }
        // ADR 0049 §6: one unreadable descriptor contributes no availability,
        // but never turns the whole fleet observation into a repository error.
        let descriptor = match candidate_accelerator_descriptor(candidate) {
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
    availability
}

/// One operator-actionable line per required backend that has no live worker.
fn missing_backend_workers(
    requirements: &VideoBackendRequirements,
    availability: &BackendAvailability,
    owner_node: &str,
) -> Vec<String> {
    let guidance =
        |worker: &str| format!("run a node agent with {worker} on owner node \"{owner_node}\"");
    let mut missing = Vec::new();
    if requirements.software && !availability.contains(&VideoEncoderBackend::Software) {
        missing.push(format!(
            "software transcode profiles require an unbound ffmpeg worker; {}",
            guidance("an unbound ffmpeg worker")
        ));
    }
    if requirements.nvidia.required && !availability.contains(&VideoEncoderBackend::Nvidia) {
        let decoder = if requirements.nvidia.hardware_decode {
            " with at least one advertised CUVID decoder"
        } else {
            ""
        };
        missing.push(format!(
            "hevc_nvenc profiles require a live NVIDIA-bound ffmpeg worker{decoder} on owner \
             node \"{owner_node}\"; configure the owner node's ffmpeg worker with its \
             probe-verified NVIDIA accelerator descriptor"
        ));
    }
    if requirements.vaapi.required && !availability.contains(&VideoEncoderBackend::Vaapi) {
        let decoder = if requirements.vaapi.hardware_decode {
            " with at least one probed VAAPI decoder"
        } else {
            ""
        };
        missing.push(format!(
            "hevc_vaapi profiles require a live VAAPI-bound ffmpeg worker{decoder} on owner \
             node \"{owner_node}\"; configure the owner node's ffmpeg worker with its \
             probe-verified VAAPI accelerator descriptor"
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
             [{encoders}]{decoder} on owner node \"{owner_node}\"; configure the owner node's \
             ffmpeg worker with its probe-verified VideoToolbox accelerator descriptor"
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

impl VideoBackendRequirements {
    fn any(&self) -> bool {
        self.software || self.nvidia.required || self.vaapi.required || self.videotoolbox.required
    }
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
        return "the node's live workers matching this tool are denied and none is effective"
            .to_owned();
    }
    if findings.contains(&EligibilityFinding::MissingGrant) {
        return "the node's live workers have the capability but no execute grant".to_owned();
    }
    "the node's live workers do not advertise a matching capability".to_owned()
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

fn is_live(worker: &Worker) -> bool {
    worker.accepts_new_work()
}

fn format_unavailable_tools(policy_slug: &str, unavailable: &[UnavailableTool]) -> String {
    let mut message = format!("tool requirement preflight failed for policy `{policy_slug}`:");
    for item in unavailable {
        message.push_str("\n- ");
        message.push_str(&item.subject);
        message.push_str(": ");
        message.push_str(&item.reason);
        message.push_str("; ");
        message.push_str(item.guidance);
    }
    message
}

/// The operation set a published tool token requires (ADR 0034 §3).
const fn tool_operations(tool: PolicyTool) -> &'static [OperationKind] {
    match tool {
        PolicyTool::Ffmpeg => &[
            OperationKind::TranscodeVideo,
            OperationKind::TranscodeAudio,
            OperationKind::ExtractAudio,
        ],
        PolicyTool::Mkvtoolnix => &[OperationKind::Remux],
        PolicyTool::Ffprobe => &[OperationKind::ProbeFile],
    }
}

const fn guidance(tool: PolicyTool) -> &'static str {
    match tool {
        PolicyTool::Ffmpeg => {
            "run a node agent with an ffmpeg worker on this storage owner \
             (voom agent documentation)"
        }
        PolicyTool::Mkvtoolnix => {
            "run a node agent with an mkvtoolnix worker on this storage owner \
             (voom agent documentation)"
        }
        PolicyTool::Ffprobe => {
            "run a node agent with an ffprobe worker on this storage owner; remove any \
             probe_file deny"
        }
    }
}

#[cfg(test)]
#[path = "tool_preflight_test.rs"]
mod tests;
