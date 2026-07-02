use time::OffsetDateTime;
use voom_core::{FileVersionId, JobId, OperationKind, PolicyInputSetId, PolicyVersionId, TicketId};
use voom_plan::{ExecutionPlan, NodeStatus, PlanOperationKind, TargetRef};
use voom_store::repo::backups::{BackupFailureDetail, NewBackup};
use voom_store::repo::safety_policies::{CommitMode, NewSafetyPolicy, VerificationLevel};

use super::*;
use crate::cases::cp;

const AT0: &str = "1970-01-01T00:00:00Z";

fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

/// A permissive safety policy: allows every V1 mutating operation, add-only
/// commits, no backup/approval/verification/prior-failure blocks. Tests tighten
/// one field to exercise one block reason.
fn permissive(slug: &str) -> NewSafetyPolicy {
    NewSafetyPolicy {
        slug: slug.to_owned(),
        display_name: "permissive".to_owned(),
        auto_execute_operations: vec![
            OperationKind::Remux,
            OperationKind::TranscodeVideo,
            OperationKind::TranscodeAudio,
            OperationKind::ExtractAudio,
            OperationKind::EditTracks,
        ],
        backup_required: false,
        approval_required: false,
        allowed_commit_modes: vec![CommitMode::AddOnly],
        verification_level: VerificationLevel::None,
        block_on_failed_records: false,
        block_on_recovery_required_records: false,
    }
}

/// A real plan with a planned `transcode_video` node (mp4/h264 source, hevc
/// policy) and no `verify_artifact` node. Its targets are synthetic, so the
/// prior-failure branches are inert here.
async fn transcode_plan(cp: &ControlPlane) -> (ExecutionPlan, PolicyVersionId, PolicyInputSetId) {
    let policy = cp
        .create_policy_document(
            "safety-transcode",
            "policy \"safety transcode\" { phase normalize { transcode video to hevc } }",
        )
        .await
        .unwrap();
    let input_set_id = crate::cases::transcodable_input(cp, "safety-transcode-input").await;
    let report = cp
        .generate_compliance_report(policy.version.id, input_set_id)
        .await
        .unwrap();
    (report.plan, policy.version.id, input_set_id)
}

fn plan_with_file_version(file_version_id: FileVersionId, op: PlanOperationKind) -> ExecutionPlan {
    ExecutionPlan {
        schema_version: 1,
        plan_id: "p".to_owned(),
        plan_hash: "h".to_owned(),
        policy: voom_plan::PolicyIdentity {
            slug: "s".to_owned(),
            source_hash: "sh".to_owned(),
            document_id: None,
            version_id: None,
        },
        input: voom_plan::InputIdentity {
            slug: None,
            source_label: None,
            input_set_id: None,
            fixture_labels: Vec::new(),
        },
        generated_at: None,
        summary: voom_plan::PlanSummary::default(),
        nodes: vec![voom_plan::PlanNode {
            node_id: "n".to_owned(),
            phase_name: "phase".to_owned(),
            ordinal: 0,
            target: TargetRef::FileVersion {
                id: file_version_id,
            },
            operation_kind: op,
            operation_payload: serde_json::json!({}),
            observed_state: None,
            status: NodeStatus::Planned,
            status_reason: String::new(),
            capability_hints: voom_plan::CapabilityHints::default(),
            scheduling_hints: voom_plan::SchedulingHints::default(),
            resource_estimates: voom_plan::ResourceEstimates::default(),
            artifact_expectations: voom_plan::ArtifactExpectations::default(),
            safety_hints: voom_plan::SafetyHints::default(),
        }],
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: voom_plan::PlanProvenance::default(),
    }
}

async fn seed_file_version(cp: &ControlPlane) -> (FileVersionId, JobId, TicketId) {
    let pool = cp.pool_for_test();
    let file_asset = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(AT0)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let file_version = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
         VALUES (?, 'blake3:x', 3, 'external_observed', ?)",
    )
    .bind(file_asset)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let job = sqlx::query(
        "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
         VALUES ('t', 'open', 0, ?, ?)",
    )
    .bind(AT0)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let ticket = sqlx::query(
        "INSERT INTO tickets \
         (job_id, kind, state, priority, payload, attempt, max_attempts, next_eligible_at, \
          created_at, state_changed_at) \
         VALUES (?, 't', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
    )
    .bind(job)
    .bind(AT0)
    .bind(AT0)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    (
        FileVersionId(u64::try_from(file_version).unwrap()),
        JobId(u64::try_from(job).unwrap()),
        TicketId(u64::try_from(ticket).unwrap()),
    )
}

/// Seed a fresh file version plus a full artifact-commit chain left in the
/// `recovery_required` state, returning that file version. Raw inserts so the
/// gate's recovery-required branch can be triggered without the workflow layer.
async fn seed_recovery_required_commit(cp: &ControlPlane) -> FileVersionId {
    let pool = cp.pool_for_test();
    let (file_version_id, _job, _ticket) = seed_file_version(cp).await;
    let worker = sqlx::query(
        "INSERT INTO workers (name, kind, status, registered_at, last_seen_at, epoch) \
         VALUES ('w', 'synthetic', 'active', ?, ?, 0)",
    )
    .bind(AT0)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let handle = sqlx::query(
        "INSERT INTO artifact_handles \
         (privacy_class, durability_class, allowed_access_modes, mutability, created_at) \
         VALUES ('private', 'durable', '[]', 'immutable', ?)",
    )
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let location = sqlx::query(
        "INSERT INTO artifact_locations (artifact_handle_id, kind, value, observed_at) \
         VALUES (?, 'local_path', '/a.mkv', ?)",
    )
    .bind(handle)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let verification = sqlx::query(
        "INSERT INTO artifact_verifications \
         (artifact_handle_id, artifact_location_id, path, worker_id, status, expected_size_bytes, \
          expected_checksum, failure_class, error_code, message, report, started_at, finished_at) \
         VALUES (?, ?, '/a.mkv', ?, 'failed', 1, 'blake3:x', 'io', 'VERIFICATION_FAILURE', 'boom', \
                 '{}', ?, ?)",
    )
    .bind(handle)
    .bind(location)
    .bind(worker)
    .bind(AT0)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, state, \
          failure_class, error_code, message, recovery_reason, report, started_at, finished_at) \
         VALUES (?, ?, ?, '/a.mkv', 'recovery_required', 'io', 'COMMIT_FAILURE', 'partial', \
                 'operator must inspect', '{}', ?, ?)",
    )
    .bind(handle)
    .bind(i64::try_from(file_version_id.0).unwrap())
    .bind(verification)
    .bind(AT0)
    .bind(AT0)
    .execute(pool)
    .await
    .unwrap();
    file_version_id
}

async fn safety_blocked_issues(cp: &ControlPlane) -> Vec<(String, String)> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT dedupe_key, status FROM issues \
         WHERE dedupe_key LIKE 'safety_blocked:%' ORDER BY id",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap()
}

#[tokio::test]
async fn missing_policy_blocks() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    let blocks = cp
        .evaluate_safety_policy("nope", &plan, false)
        .await
        .unwrap();
    assert_eq!(blocks, vec![SafetyBlock::PolicyNotFound]);
}

#[tokio::test]
async fn stale_schema_version_is_the_sole_block() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    // A stale row that would otherwise be permissive still fails closed.
    cp.create_safety_policy(permissive("stale")).await.unwrap();
    sqlx::query(
        "UPDATE safety_policies SET schema_version = schema_version + 1 WHERE slug = 'stale'",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let blocks = cp
        .evaluate_safety_policy("stale", &plan, false)
        .await
        .unwrap();
    assert!(
        matches!(blocks.as_slice(), [SafetyBlock::PolicyStale { .. }]),
        "got {blocks:?}"
    );
}

#[tokio::test]
async fn approval_required_blocks() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    let mut policy = permissive("appr");
    policy.approval_required = true;
    cp.create_safety_policy(policy).await.unwrap();
    let blocks = cp
        .evaluate_safety_policy("appr", &plan, false)
        .await
        .unwrap();
    assert!(
        blocks.contains(&SafetyBlock::ApprovalRequired),
        "got {blocks:?}"
    );
}

#[tokio::test]
async fn add_only_not_allowed_blocks() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    let mut policy = permissive("mode");
    policy.allowed_commit_modes = vec![CommitMode::Replace];
    cp.create_safety_policy(policy).await.unwrap();
    let blocks = cp
        .evaluate_safety_policy("mode", &plan, false)
        .await
        .unwrap();
    assert!(
        blocks.contains(&SafetyBlock::CommitModeNotAllowed { needed: "add_only" }),
        "got {blocks:?}"
    );
}

#[tokio::test]
async fn backup_required_blocks_only_without_a_root() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    let mut policy = permissive("bkp");
    policy.backup_required = true;
    cp.create_safety_policy(policy).await.unwrap();

    let without = cp
        .evaluate_safety_policy("bkp", &plan, false)
        .await
        .unwrap();
    assert!(
        without.contains(&SafetyBlock::BackupRequiredButNoRoot),
        "got {without:?}"
    );

    let with = cp.evaluate_safety_policy("bkp", &plan, true).await.unwrap();
    assert!(
        !with.contains(&SafetyBlock::BackupRequiredButNoRoot),
        "got {with:?}"
    );
}

#[tokio::test]
async fn verification_required_without_verify_node_blocks() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    let mut policy = permissive("ver");
    policy.verification_level = VerificationLevel::Full;
    cp.create_safety_policy(policy).await.unwrap();
    let blocks = cp
        .evaluate_safety_policy("ver", &plan, false)
        .await
        .unwrap();
    assert!(
        blocks.contains(&SafetyBlock::VerificationRequiredButAbsent { required: "full" }),
        "got {blocks:?}"
    );
}

#[tokio::test]
async fn operation_not_in_allowlist_blocks() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    let mut policy = permissive("ops");
    policy.auto_execute_operations = Vec::new();
    cp.create_safety_policy(policy).await.unwrap();
    let blocks = cp
        .evaluate_safety_policy("ops", &plan, false)
        .await
        .unwrap();
    assert!(
        blocks.contains(&SafetyBlock::OperationNotAutoExecutable {
            operation: "transcode_video"
        }),
        "got {blocks:?}"
    );
}

#[tokio::test]
async fn permissive_policy_produces_no_blocks() {
    let (cp, _tmp) = cp().await;
    let (plan, _pv, _is) = transcode_plan(&cp).await;
    cp.create_safety_policy(permissive("ok")).await.unwrap();
    let blocks = cp.evaluate_safety_policy("ok", &plan, false).await.unwrap();
    assert!(blocks.is_empty(), "got {blocks:?}");
}

#[tokio::test]
async fn latest_failed_backup_blocks_and_a_later_verified_clears_it() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, job_id, ticket_id) = seed_file_version(&cp).await;
    let new_backup = || NewBackup {
        source_file_version_id: file_version_id,
        job_id,
        ticket_id,
        provider: "p".to_owned(),
        destination_path: "/b".to_owned(),
    };
    let failed = cp
        .backups
        .insert_pending(new_backup(), at(0))
        .await
        .unwrap();
    cp.backups
        .mark_failed(
            failed.id,
            &BackupFailureDetail {
                failure_class: "io".to_owned(),
                error_code: "BACKUP_FAILURE".to_owned(),
                message: "x".to_owned(),
            },
            at(1),
        )
        .await
        .unwrap();

    let plan = plan_with_file_version(file_version_id, PlanOperationKind::TranscodeVideo);
    let mut policy = permissive("fr");
    policy.block_on_failed_records = true;
    cp.create_safety_policy(policy).await.unwrap();

    let blocked = cp.evaluate_safety_policy("fr", &plan, false).await.unwrap();
    assert_eq!(
        blocked,
        vec![SafetyBlock::BlockedByFailedRecord {
            source_file_version_id: file_version_id.0
        }]
    );

    // A later verified backup supersedes the failed one — self-clearing.
    let retried = cp
        .backups
        .insert_pending(new_backup(), at(2))
        .await
        .unwrap();
    cp.backups
        .mark_verified(retried.id, 1, "blake3:1", at(3))
        .await
        .unwrap();
    let cleared = cp.evaluate_safety_policy("fr", &plan, false).await.unwrap();
    assert!(cleared.is_empty(), "got {cleared:?}");
}

#[tokio::test]
async fn recovery_required_record_blocks_when_policy_sets_flag() {
    let (cp, _tmp) = cp().await;
    let file_version_id = seed_recovery_required_commit(&cp).await;
    let plan = plan_with_file_version(file_version_id, PlanOperationKind::TranscodeVideo);

    // With the flag off, the recovery-required record does not block.
    cp.create_safety_policy(permissive("rr-off")).await.unwrap();
    assert!(
        cp.evaluate_safety_policy("rr-off", &plan, false)
            .await
            .unwrap()
            .is_empty()
    );

    // With the flag on, it blocks.
    let mut policy = permissive("rr-on");
    policy.block_on_recovery_required_records = true;
    cp.create_safety_policy(policy).await.unwrap();
    let blocks = cp
        .evaluate_safety_policy("rr-on", &plan, false)
        .await
        .unwrap();
    assert_eq!(
        blocks,
        vec![SafetyBlock::BlockedByRecoveryRequiredRecord {
            source_file_version_id: file_version_id.0
        }]
    );
}

#[tokio::test]
async fn enforce_opens_a_blocked_issue_and_errors() {
    let (cp, _tmp) = cp().await;
    let (plan, pv, is) = transcode_plan(&cp).await;
    let mut policy = permissive("blk");
    policy.approval_required = true;
    cp.create_safety_policy(policy).await.unwrap();

    let err = cp
        .enforce_safety_policy("blk", &plan, pv, is, false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");

    let issues = safety_blocked_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].1, "open");
}

#[tokio::test]
async fn enforce_resolves_the_blocked_issue_once_the_policy_permits() {
    let (cp, _tmp) = cp().await;
    let (plan, pv, is) = transcode_plan(&cp).await;
    let mut policy = permissive("fixme");
    policy.approval_required = true;
    cp.create_safety_policy(policy).await.unwrap();
    cp.enforce_safety_policy("fixme", &plan, pv, is, false)
        .await
        .unwrap_err();

    // Operator relaxes the policy; a clean gate resolves the prior open issue.
    cp.update_safety_policy(permissive("fixme")).await.unwrap();
    cp.enforce_safety_policy("fixme", &plan, pv, is, false)
        .await
        .unwrap();

    let issues = safety_blocked_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].1, "resolved");
}
