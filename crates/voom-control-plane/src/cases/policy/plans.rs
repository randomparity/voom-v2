use voom_core::{PolicyInputSetId, PolicyVersionId, VoomError};
use voom_policy::{
    BundleTargetInput, IdentityEvidenceInput, IssueInput, MediaSnapshotInput, PolicyInputSetDraft,
    PolicySyntheticTarget, QualityProfileSelection, TargetRef,
};
use voom_store::repo::{
    policies::PolicyRepo,
    policy_inputs::{PolicyInputRepo, PolicyInputSet, PolicyInputTargetRef},
};

use crate::ControlPlane;

pub fn plan_compiled_policy_with_input(
    policy: voom_policy::CompiledPolicy,
    input: PolicyInputSetDraft,
    mut context: voom_plan::PlanningContext,
) -> Result<voom_plan::ExecutionPlan, VoomError> {
    context.schema_version = 1;
    voom_plan::generate_plan(voom_plan::PlanningRequest {
        policy,
        input,
        context,
    })
    .map_err(voom_plan::PlanGenerationError::into_voom_error)
}

pub fn plan_policy_source_with_input(
    source: &str,
    input: PolicyInputSetDraft,
    input_source_label: Option<&str>,
) -> Result<voom_plan::ExecutionPlan, VoomError> {
    let mut compiled = voom_policy::compile_policy(source)
        .map_err(|err| err.error)?
        .policy;
    // The offline fixture planner is store-free, so it can only resolve inline
    // profiles; named references require the store-backed `voom plan show`.
    crate::transcode::resolve::resolve_inline_profiles_in_policy(&mut compiled)?;
    plan_compiled_policy_with_input(
        compiled,
        input,
        voom_plan::PlanningContext {
            input_source_label: input_source_label.map(str::to_owned),
            ..voom_plan::PlanningContext::default()
        },
    )
}

/// Resolves every `TranscodeVideo` operation's profile reference against the
/// store-backed registry, populating `resolved_profile` (and overwriting
/// `target_codec`/`container`) in memory before the pure planner runs.
///
/// # Errors
/// Returns `CONFIG_INVALID` when a named profile does not exist or inline
/// settings fail descriptor validation.
pub(crate) async fn resolve_profiles_in_policy(
    cp: &ControlPlane,
    policy: &mut voom_policy::CompiledPolicy,
) -> Result<(), VoomError> {
    for phase in &mut policy.phases {
        for operation in &mut phase.operations {
            if let voom_policy::CompiledOperation::TranscodeVideo {
                profile,
                target_codec,
                container,
                resolved_profile,
            } = operation
            {
                let resolved = crate::transcode::resolve::resolve_video_profile_ref(
                    &cp.video_profiles,
                    profile,
                )
                .await?;
                target_codec.clone_from(&resolved.profile.target_codec);
                container.clone_from(&resolved.output_container);
                *resolved_profile = Some(resolved.profile);
            }
        }
    }
    Ok(())
}

impl ControlPlane {
    /// Generate an execution plan from stored policy and input rows.
    ///
    /// # Errors
    /// Returns `NotFound` for missing durable inputs, `PlanGeneration` for
    /// invalid stored compiled JSON or identity mismatch, and propagates
    /// repository/planner errors.
    pub async fn plan_accepted_policy_version_with_input_set(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<voom_plan::ExecutionPlan, VoomError> {
        let version = self
            .policies
            .get_version(policy_version_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("policy version {policy_version_id} not found"))
            })?;
        let mut policy: voom_policy::CompiledPolicy = serde_json::from_value(version.compiled_json)
            .map_err(|e| {
                VoomError::PlanGeneration(format!("stored compiled policy JSON is invalid: {e}"))
            })?;
        if policy.source_hash != version.source_hash
            || policy.schema_version != version.schema_version
        {
            return Err(VoomError::PlanGeneration(format!(
                "stored compiled policy identity mismatch for policy version {policy_version_id}"
            )));
        }
        // Resolve profile references before the pure planner; shared with the
        // execute path for dry-run/execute parity.
        resolve_profiles_in_policy(self, &mut policy).await?;
        let input = self
            .policy_inputs
            .get_input_set(input_set_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("policy input set {input_set_id} not found"))
            })?;
        plan_compiled_policy_with_input(
            policy,
            input_set_to_draft(input),
            voom_plan::PlanningContext {
                policy_document_id: Some(version.policy_document_id),
                policy_version_id: Some(version.id),
                policy_input_set_id: Some(input_set_id),
                ..voom_plan::PlanningContext::default()
            },
        )
    }
}

pub(crate) fn input_set_to_draft(input: PolicyInputSet) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: input.slug,
        display_name: input.display_name,
        schema_version: input.schema_version,
        source_kind: input.source_kind,
        created_at: input.created_at,
        description: input.description,
        fixture_labels: input.fixture_labels,
        synthetic_targets: input
            .synthetic_targets
            .into_iter()
            .map(|target| PolicySyntheticTarget {
                synthetic_key: target.synthetic_key,
                target_kind: target.target_kind,
                display_name: target.display_name,
            })
            .collect(),
        media_snapshots: input
            .media_snapshots
            .into_iter()
            .map(|snapshot| MediaSnapshotInput {
                ordinal: snapshot.ordinal,
                target: target_ref_to_policy(snapshot.target),
                container: snapshot.container,
                stream_summary: snapshot.stream_summary,
                video_codec: snapshot.video_codec,
                width: snapshot.width,
                height: snapshot.height,
                hdr: snapshot.hdr,
                bitrate: snapshot.bitrate,
                duration_millis: snapshot.duration_millis,
                audio_languages: snapshot.audio_languages,
                subtitle_languages: snapshot.subtitle_languages,
                health_flags: snapshot.health_flags,
                existing_media_snapshot_id: snapshot.existing_media_snapshot_id,
            })
            .collect(),
        identity_evidence: input
            .identity_evidence
            .into_iter()
            .map(|evidence| IdentityEvidenceInput {
                ordinal: evidence.ordinal,
                target: target_ref_to_policy(evidence.target),
                assertion_type: evidence.assertion_type,
                provider: evidence.provider,
                provider_version: evidence.provider_version,
                confidence: evidence.confidence,
                provenance: evidence.provenance,
                observed_at: evidence.observed_at,
                existing_evidence_id: evidence.existing_evidence_id,
            })
            .collect(),
        bundle_targets: input
            .bundle_targets
            .into_iter()
            .map(|bundle| BundleTargetInput {
                ordinal: bundle.ordinal,
                target: target_ref_to_policy(bundle.target),
                role: bundle.role,
                desired_state: bundle.desired_state,
                language: bundle.language,
                label: bundle.label,
                disposition: bundle.disposition,
                artifact_expectation: bundle.artifact_expectation,
            })
            .collect(),
        quality_profiles: input
            .quality_profiles
            .into_iter()
            .map(|profile| QualityProfileSelection {
                ordinal: profile.ordinal,
                target: target_ref_to_policy(profile.target),
                profile_name: profile.profile_name,
                profile_version: profile.profile_version,
                dimension_weights: profile.dimension_weights,
            })
            .collect(),
        issues: input
            .issues
            .into_iter()
            .map(|issue| IssueInput {
                ordinal: issue.ordinal,
                target: target_ref_to_policy(issue.target),
                kind: issue.kind,
                severity: issue.severity,
                priority: issue.priority,
                state: issue.state,
                reason: issue.reason,
                provenance: issue.provenance,
                existing_issue_id: issue.existing_issue_id,
            })
            .collect(),
    }
}

fn target_ref_to_policy(target: PolicyInputTargetRef) -> TargetRef {
    match target {
        PolicyInputTargetRef::MediaWork { id } => TargetRef::MediaWork { id },
        PolicyInputTargetRef::MediaVariant { id } => TargetRef::MediaVariant { id },
        PolicyInputTargetRef::AssetBundle { id } => TargetRef::AssetBundle { id },
        PolicyInputTargetRef::FileAsset { id } => TargetRef::FileAsset { id },
        PolicyInputTargetRef::FileVersion { id } => TargetRef::FileVersion { id },
        PolicyInputTargetRef::FileLocation { id } => TargetRef::FileLocation { id },
        PolicyInputTargetRef::Synthetic { key, kind, .. } => TargetRef::Synthetic { key, kind },
    }
}

#[cfg(test)]
#[path = "plans_test.rs"]
mod tests;
