//! The fail-closed safety gate for `compliance execute` (T12, #281).
//!
//! When an execute run names a safety policy, [`ControlPlane::enforce_safety_policy`]
//! reads it and evaluates it against the generated plan and run options *before*
//! any dispatch. A missing, stale, or insufficient policy blocks the run and
//! records a durable issue rather than synthesizing a default (design doc ->
//! Security And Safety; ADR 0028). A clean evaluation resolves any prior block
//! issue so a fixed policy does not leave a dangling open issue.

use voom_core::{OperationKind, PolicyInputSetId, PolicyVersionId, VoomError};
use voom_events::{Event, SubjectType, payload::IssueLifecyclePayload};
use voom_plan::{ExecutionPlan, PlanOperationKind};
use voom_store::repo::backups::BackupStatus;
use voom_store::repo::safety_policies::{
    CommitMode, SAFETY_POLICY_SCHEMA_VERSION, SafetyPolicy, VerificationLevel,
};
use voom_store::repo::{
    PolicyIssueDraft, PolicyIssueMutation, PolicyIssueMutationKind, PolicyIssueStatus,
};

use crate::ControlPlane;
use crate::cases::policy::compliance::plan_file_version_targets;
use crate::cases::{append_event, begin_tx, commit_tx};

/// One reason a safety policy blocks an execute run. `Serialize` so the CLI can
/// surface the machine-readable reason set alongside the human message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SafetyBlock {
    /// No safety policy with the requested slug.
    PolicyNotFound,
    /// The policy row's `schema_version` differs from this binary's — the row is
    /// too old (or too new) to trust; fail closed rather than read stale fields.
    PolicyStale {
        row_schema_version: u32,
        required_schema_version: u32,
    },
    /// The policy requires approval, for which no grant path exists pre-daemon.
    ApprovalRequired,
    /// The execute path commits add-only, which the policy does not permit.
    CommitModeNotAllowed { needed: &'static str },
    /// Backup is required but no `--backup-root` was supplied.
    BackupRequiredButNoRoot,
    /// A non-`none` verification level is required but the plan has no verify step.
    VerificationRequiredButAbsent { required: &'static str },
    /// A planned mutating operation is not in the auto-execute allowlist.
    OperationNotAutoExecutable { operation: &'static str },
    /// A prior failed record blocks automation for this file version.
    BlockedByFailedRecord { source_file_version_id: u64 },
    /// A prior recovery-required record blocks automation for this file version.
    BlockedByRecoveryRequiredRecord { source_file_version_id: u64 },
}

impl SafetyBlock {
    /// A one-line human description for the issue body / error message.
    fn describe(&self) -> String {
        match self {
            Self::PolicyNotFound => "safety policy not found".to_owned(),
            Self::PolicyStale {
                row_schema_version,
                required_schema_version,
            } => format!(
                "safety policy is stale (row schema_version {row_schema_version}, \
                 this binary requires {required_schema_version}); re-create it"
            ),
            Self::ApprovalRequired => {
                "safety policy requires approval, which has no pre-daemon grant path".to_owned()
            }
            Self::CommitModeNotAllowed { needed } => {
                format!("safety policy does not allow the required commit mode {needed:?}")
            }
            Self::BackupRequiredButNoRoot => {
                "safety policy requires backup but no --backup-root was supplied".to_owned()
            }
            Self::VerificationRequiredButAbsent { required } => format!(
                "safety policy requires verification level {required:?} but the plan has no \
                 verify_artifact step"
            ),
            Self::OperationNotAutoExecutable { operation } => format!(
                "operation {operation:?} is not in the safety policy's auto-execute allowlist"
            ),
            Self::BlockedByFailedRecord {
                source_file_version_id,
            } => format!(
                "a failed record blocks automation for file version {source_file_version_id}"
            ),
            Self::BlockedByRecoveryRequiredRecord {
                source_file_version_id,
            } => format!(
                "a recovery-required record blocks automation for file version \
                 {source_file_version_id}"
            ),
        }
    }
}

/// Map a planned operation to the `OperationKind` the auto-execute allowlist is
/// keyed on, or `None` for a non-mutating operation (control flow / read-only
/// verify) that the allowlist does not gate. The wildcard-free `match` makes a
/// new `PlanOperationKind` a compile error, so the gate cannot silently fail
/// open as the operation vocabulary grows (ADR 0028).
fn mutating_operation_kind(kind: PlanOperationKind) -> Option<OperationKind> {
    match kind {
        PlanOperationKind::Remux | PlanOperationKind::SetContainer => Some(OperationKind::Remux),
        PlanOperationKind::TranscodeVideo => Some(OperationKind::TranscodeVideo),
        PlanOperationKind::TranscodeAudio => Some(OperationKind::TranscodeAudio),
        PlanOperationKind::ExtractAudio => Some(OperationKind::ExtractAudio),
        PlanOperationKind::KeepTracks
        | PlanOperationKind::RemoveTracks
        | PlanOperationKind::ReorderTracks
        | PlanOperationKind::SetDefaults
        | PlanOperationKind::ClearTrackActions
        | PlanOperationKind::ClearTags
        | PlanOperationKind::SetTag
        | PlanOperationKind::DeleteTag => Some(OperationKind::EditTracks),
        PlanOperationKind::VerifyArtifact
        | PlanOperationKind::Conditional
        | PlanOperationKind::Rules => None,
    }
}

/// Distinct `OperationKind`s the plan's *planned* nodes will dispatch as
/// mutations, in deterministic order.
fn planned_mutating_operations(plan: &ExecutionPlan) -> Vec<OperationKind> {
    let mut kinds: Vec<OperationKind> = Vec::new();
    for node in &plan.nodes {
        if node.status != voom_plan::NodeStatus::Planned {
            continue;
        }
        if let Some(kind) = mutating_operation_kind(node.operation_kind)
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }
    kinds
}

/// `true` when the plan contains any `verify_artifact` node (regardless of
/// status): verification is part of the pipeline.
fn plan_has_verify(plan: &ExecutionPlan) -> bool {
    plan.nodes
        .iter()
        .any(|node| node.operation_kind == PlanOperationKind::VerifyArtifact)
}

/// Blocks decided from the plan and run options alone (no extra DB reads).
fn static_blocks(
    policy: &SafetyPolicy,
    plan: &ExecutionPlan,
    backup_root_present: bool,
) -> Vec<SafetyBlock> {
    let mut blocks = Vec::new();
    if policy.approval_required {
        blocks.push(SafetyBlock::ApprovalRequired);
    }
    if !policy.allows_commit_mode(CommitMode::AddOnly) {
        blocks.push(SafetyBlock::CommitModeNotAllowed {
            needed: CommitMode::AddOnly.as_str(),
        });
    }
    if policy.backup_required && !backup_root_present {
        blocks.push(SafetyBlock::BackupRequiredButNoRoot);
    }
    if policy.verification_level != VerificationLevel::None && !plan_has_verify(plan) {
        blocks.push(SafetyBlock::VerificationRequiredButAbsent {
            required: policy.verification_level.as_str(),
        });
    }
    for operation in planned_mutating_operations(plan) {
        if !policy.allows_auto_execute(operation) {
            blocks.push(SafetyBlock::OperationNotAutoExecutable {
                operation: operation.as_str(),
            });
        }
    }
    blocks
}

impl ControlPlane {
    /// Evaluate a safety policy against a plan and run options, returning the set
    /// of blocks (empty ⇒ the run may proceed). A missing or stale policy short-
    /// circuits: a stale row's other fields must not be trusted, so staleness is
    /// the sole block.
    pub(crate) async fn evaluate_safety_policy(
        &self,
        slug: &str,
        plan: &ExecutionPlan,
        backup_root_present: bool,
    ) -> Result<Vec<SafetyBlock>, VoomError> {
        let Some(policy) = self.safety_policies.get_by_slug(slug).await? else {
            return Ok(vec![SafetyBlock::PolicyNotFound]);
        };
        if !policy.is_current_schema() {
            return Ok(vec![SafetyBlock::PolicyStale {
                row_schema_version: policy.schema_version,
                required_schema_version: SAFETY_POLICY_SCHEMA_VERSION,
            }]);
        }
        let mut blocks = static_blocks(&policy, plan, backup_root_present);
        self.append_prior_failure_blocks(&policy, plan, &mut blocks)
            .await?;
        Ok(blocks)
    }

    /// Blocks decided from durable failed / recovery-required records for the
    /// plan's targeted file versions (latest-record, self-clearing semantics).
    async fn append_prior_failure_blocks(
        &self,
        policy: &SafetyPolicy,
        plan: &ExecutionPlan,
        blocks: &mut Vec<SafetyBlock>,
    ) -> Result<(), VoomError> {
        if !policy.block_on_failed_records && !policy.block_on_recovery_required_records {
            return Ok(());
        }
        for file_version_id in plan_file_version_targets(plan) {
            if policy.block_on_failed_records {
                let latest = self.backups.latest_by_file_version(file_version_id).await?;
                if latest.is_some_and(|b| b.status == BackupStatus::Failed) {
                    blocks.push(SafetyBlock::BlockedByFailedRecord {
                        source_file_version_id: file_version_id.0,
                    });
                }
            }
            if policy.block_on_recovery_required_records
                && self
                    .artifacts
                    .has_recovery_required_for_source_version(file_version_id)
                    .await?
            {
                blocks.push(SafetyBlock::BlockedByRecoveryRequiredRecord {
                    source_file_version_id: file_version_id.0,
                });
            }
        }
        Ok(())
    }

    /// Enforce a named safety policy before dispatch. On one or more blocks,
    /// opens a durable `safety_blocked` issue and returns
    /// [`VoomError::PolicyValidationError`]; on a clean evaluation, resolves any
    /// prior `safety_blocked` issue for the same key and returns `Ok(())`.
    pub(crate) async fn enforce_safety_policy(
        &self,
        slug: &str,
        plan: &ExecutionPlan,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        backup_root_present: bool,
    ) -> Result<(), VoomError> {
        let blocks = self
            .evaluate_safety_policy(slug, plan, backup_root_present)
            .await?;
        let dedupe_key = safety_blocked_dedupe_key(slug, policy_version_id, input_set_id);
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        if blocks.is_empty() {
            self.resolve_safety_issue(&mut tx, &dedupe_key, policy_version_id, now)
                .await?;
            commit_tx(tx).await?;
            return Ok(());
        }
        let draft = safety_issue_draft(&dedupe_key, slug, &blocks);
        let mutation = self
            .issues
            .upsert_policy_noncompliant_in_tx(&mut tx, draft, now)
            .await?;
        emit_safety_issue_event(self, &mut tx, &mutation, policy_version_id, now).await?;
        commit_tx(tx).await?;
        Err(VoomError::PolicyValidationError(format!(
            "safety policy {slug:?} blocked compliance execute: {}",
            join_reasons(&blocks)
        )))
    }

    async fn resolve_safety_issue(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        dedupe_key: &str,
        policy_version_id: PolicyVersionId,
        now: time::OffsetDateTime,
    ) -> Result<(), VoomError> {
        if let Some(mutation) = self
            .issues
            .resolve_policy_noncompliant_by_dedupe_key_in_tx(
                tx,
                dedupe_key,
                "Safety policy no longer blocks this compliance execute",
                "The safety policy now permits this run; prior block resolved.",
                now,
            )
            .await?
        {
            emit_safety_issue_event(self, tx, &mutation, policy_version_id, now).await?;
        }
        Ok(())
    }
}

fn safety_blocked_dedupe_key(
    slug: &str,
    policy_version_id: PolicyVersionId,
    input_set_id: PolicyInputSetId,
) -> String {
    format!(
        "safety_blocked:v1:policy={slug}:pv={}:is={}",
        policy_version_id.0, input_set_id.0
    )
}

fn join_reasons(blocks: &[SafetyBlock]) -> String {
    blocks
        .iter()
        .map(SafetyBlock::describe)
        .collect::<Vec<_>>()
        .join("; ")
}

fn safety_issue_draft(dedupe_key: &str, slug: &str, blocks: &[SafetyBlock]) -> PolicyIssueDraft {
    PolicyIssueDraft {
        dedupe_key: dedupe_key.to_owned(),
        status: PolicyIssueStatus::Open,
        title: format!("Safety policy '{slug}' blocked compliance execute"),
        body: format!("Blocked because: {}.", join_reasons(blocks)),
        priority_reason: format!("safety policy {slug} fail-closed gate"),
    }
}

async fn emit_safety_issue_event(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mutation: &PolicyIssueMutation,
    policy_version_id: PolicyVersionId,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = IssueLifecyclePayload {
        issue_id: mutation.row.id,
        kind: "policy_noncompliant".to_owned(),
        status: mutation.row.status.as_str().to_owned(),
        dedupe_key: Some(mutation.row.dedupe_key.clone()),
        policy_version_id: Some(policy_version_id),
        report_id: None,
    };
    let event = match mutation.kind {
        PolicyIssueMutationKind::Created => Event::IssueOpened(payload),
        PolicyIssueMutationKind::Updated => Event::IssueUpdated(payload),
        PolicyIssueMutationKind::Resolved => Event::IssueResolved(payload),
        PolicyIssueMutationKind::Unchanged => return Ok(()),
    };
    append_event(
        &cp.events,
        tx,
        SubjectType::System,
        Some(mutation.row.id.0),
        now,
        event,
    )
    .await
}

#[cfg(test)]
#[path = "safety_gate_test.rs"]
mod tests;
