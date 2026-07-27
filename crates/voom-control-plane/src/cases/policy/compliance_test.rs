use std::path::PathBuf;

use sqlx::Row;
use time::OffsetDateTime;
use voom_core::{OperationKind, TicketOperation};
use voom_events::EventKind;
use voom_plan::PlanOperationKind;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::tickets::NewTicket;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};

use crate::cases::policy::policy_inputs::PolicyInputFromScanInput;
use crate::cases::{count, cp, transcodable_input};
use crate::workflow::WorkerRuntimeRegistry;
use crate::workflow::execution::executor::WorkflowExecutorOptions;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn file_phase_view(
    branch_id: &str,
    phase_ordinal: u32,
    outcome: &'static str,
) -> super::FilePhaseSummaryView {
    super::FilePhaseSummaryView {
        phase_ordinal,
        branch_id: branch_id.to_owned(),
        outcome,
        ticket_ids: Vec::new(),
        produced_file_version_id: None,
        produced_file_location_id: None,
        artifact_handle_id: None,
        artifact_verification_id: None,
        reprobe_snapshot_id: None,
    }
}

#[test]
fn progress_counts_empty_is_all_zero() {
    let counts = super::progress_counts(&[]);
    assert_eq!(counts, super::ProgressCountsView::default());
    assert_eq!(counts.total, 0);
    assert_eq!(counts.remaining, 0);
}

#[test]
fn progress_counts_buckets_latest_outcome_per_file() {
    // Three distinct files: one committed, one blocked, one skipped. A file that
    // advanced across phases is counted by its latest (highest-ordinal) row.
    let file_phases = [
        file_phase_view("a.mkv", 0, "skipped"),
        file_phase_view("a.mkv", 1, "committed"), // latest wins for a.mkv
        file_phase_view("b.mkv", 0, "blocked"),
        file_phase_view("c.mkv", 0, "skipped"),
    ];
    let counts = super::progress_counts(&file_phases);
    assert_eq!(counts.total, 3);
    assert_eq!(counts.completed, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.skipped, 1);
    // Every file is terminal, so nothing is left outstanding.
    assert_eq!(counts.remaining, 0);
}

#[test]
fn progress_counts_skipped_is_not_remaining() {
    // A skipped-because-compliant file is finished, not outstanding work.
    let counts = super::progress_counts(&[file_phase_view("only.mkv", 0, "skipped")]);
    assert_eq!(counts.total, 1);
    assert_eq!(counts.skipped, 1);
    assert_eq!(counts.remaining, 0);
}

#[test]
fn compliance_execution_defaults_use_production_transcode_paths() {
    let workflow_defaults = WorkflowExecutorOptions::default();
    let compliance_defaults = super::ComplianceExecutionOptions::default();

    assert_eq!(
        compliance_defaults.transcode_staging_root,
        workflow_defaults.artifact_roots.transcode.staging_root
    );
    assert_eq!(
        compliance_defaults.transcode_target_dir,
        workflow_defaults.artifact_roots.transcode.target_dir
    );
}

#[test]
fn compliance_execution_defaults_use_production_remux_paths() {
    let workflow_defaults = WorkflowExecutorOptions::default();
    let compliance_defaults = super::ComplianceExecutionOptions::default();

    assert_eq!(
        compliance_defaults.remux_staging_root,
        workflow_defaults.artifact_roots.remux.staging_root
    );
    assert_eq!(
        compliance_defaults.remux_target_dir,
        workflow_defaults.artifact_roots.remux.target_dir
    );
}

#[test]
fn compliance_execution_defaults_use_production_audio_paths() {
    let workflow_defaults = WorkflowExecutorOptions::default();
    let compliance_defaults = super::ComplianceExecutionOptions::default();

    assert_eq!(
        compliance_defaults.audio_staging_root,
        workflow_defaults.artifact_roots.audio.staging_root
    );
    assert_eq!(
        compliance_defaults.audio_target_dir,
        workflow_defaults.artifact_roots.audio.target_dir
    );
}

#[test]
fn compliance_options_convert_paths_into_workflow_options_leaving_rest_default() {
    let options = super::ComplianceExecutionOptions {
        transcode_staging_root: PathBuf::from("/srv/transcode/staging"),
        transcode_target_dir: PathBuf::from("/srv/transcode/out"),
        remux_staging_root: PathBuf::from("/srv/remux/staging"),
        remux_target_dir: PathBuf::from("/srv/remux/out"),
        audio_staging_root: PathBuf::from("/srv/audio/staging"),
        audio_target_dir: PathBuf::from("/srv/audio/out"),
        backup_root: None,
        safety_policy_slug: None,
    };

    let converted = WorkflowExecutorOptions::from(options.clone());

    // Staging roots pass through unchanged.
    assert_eq!(
        converted.artifact_roots.transcode.staging_root,
        options.transcode_staging_root
    );
    assert_eq!(
        converted.artifact_roots.remux.staging_root,
        options.remux_staging_root
    );
    assert_eq!(
        converted.artifact_roots.audio.staging_root,
        options.audio_staging_root
    );
    // Commit target dirs route to per-operation working dirs, NOT the operator
    // output dirs (`*_target_dir`); post-run promotion moves finals out.
    assert_eq!(
        converted.artifact_roots.transcode.target_dir,
        super::committed_working_dir(&options.transcode_staging_root, "transcode")
    );
    assert_eq!(
        converted.artifact_roots.remux.target_dir,
        super::committed_working_dir(&options.remux_staging_root, "remux")
    );
    assert_eq!(
        converted.artifact_roots.audio.target_dir,
        super::committed_working_dir(&options.audio_staging_root, "audio")
    );
    assert_ne!(
        converted.artifact_roots.transcode.target_dir,
        options.transcode_target_dir
    );
    // Non-path fields stay at workflow defaults: the facade carries paths only.
    let workflow_defaults = WorkflowExecutorOptions::default();
    assert_eq!(
        converted.queue.max_attempts,
        workflow_defaults.queue.max_attempts
    );
    assert_eq!(
        converted.timing.lease_ttl,
        workflow_defaults.timing.lease_ttl
    );
}

#[test]
fn committed_source_dir_namespaces_per_source_under_the_working_dir() {
    use voom_core::FileVersionId;

    let working = super::committed_working_dir(&PathBuf::from("/srv/staging"), "remux");
    let a = super::committed_source_dir(&working, FileVersionId(7));
    let b = super::committed_source_dir(&working, FileVersionId(8));

    // Distinct sources get distinct commit dirs (no flat collision), and each
    // stays under the operation working dir so promotion's prefix match holds.
    assert_eq!(a, working.join("v7"));
    assert_ne!(a, b);
    assert!(a.starts_with(&working));
    assert!(b.starts_with(&working));
}

#[test]
fn apply_staging_root_sets_every_family_without_touching_target_dirs() {
    let mut options = super::ComplianceExecutionOptions::default();
    let defaults = super::ComplianceExecutionOptions::default();
    options.apply_staging_root(PathBuf::from("/srv/staging"));

    assert_eq!(
        options.transcode_staging_root,
        PathBuf::from("/srv/staging")
    );
    assert_eq!(options.remux_staging_root, PathBuf::from("/srv/staging"));
    assert_eq!(options.audio_staging_root, PathBuf::from("/srv/staging"));
    assert_eq!(options.transcode_target_dir, defaults.transcode_target_dir);
    assert_eq!(options.remux_target_dir, defaults.remux_target_dir);
    assert_eq!(options.audio_target_dir, defaults.audio_target_dir);
}

#[test]
fn apply_output_dir_sets_every_family_without_touching_staging_roots() {
    let mut options = super::ComplianceExecutionOptions::default();
    let defaults = super::ComplianceExecutionOptions::default();
    options.apply_output_dir(PathBuf::from("/srv/out"));

    assert_eq!(options.transcode_target_dir, PathBuf::from("/srv/out"));
    assert_eq!(options.remux_target_dir, PathBuf::from("/srv/out"));
    assert_eq!(options.audio_target_dir, PathBuf::from("/srv/out"));
    assert_eq!(
        options.transcode_staging_root,
        defaults.transcode_staging_root
    );
    assert_eq!(options.remux_staging_root, defaults.remux_staging_root);
    assert_eq!(options.audio_staging_root, defaults.audio_staging_root);
}

async fn seed_noncompliant(
    cp: &crate::ControlPlane,
) -> (
    voom_core::PolicyVersionId,
    voom_core::PolicyInputSetId,
    voom_core::PolicyDocumentId,
) {
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(
            load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap(),
        )
        .await
        .unwrap();
    (
        created_policy.version.id,
        input.id,
        created_policy.document.id,
    )
}

async fn seed_blocked(
    cp: &crate::ControlPlane,
) -> (voom_core::PolicyVersionId, voom_core::PolicyInputSetId) {
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let mut input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();
    input.media_snapshots[0].container = None;
    input.slug = "synthetic-blocked-container".to_owned();
    input.fixture_labels = vec!["synthetic_blocked_container".to_owned()];
    let input = cp.create_policy_input_set(input).await.unwrap();
    (created_policy.version.id, input.id)
}

async fn seed_compliant(
    cp: &crate::ControlPlane,
) -> (
    voom_core::PolicyVersionId,
    voom_core::PolicyInputSetId,
    voom_core::PolicyDocumentId,
) {
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    (
        created_policy.version.id,
        input.id,
        created_policy.document.id,
    )
}

#[tokio::test]
async fn compliance_report_is_read_only() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    let before = table_counts(&cp).await;

    let data = cp
        .generate_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.report.summary.status, voom_plan::ReportStatus::Mixed);
    assert_eq!(before, table_counts(&cp).await);
}

#[tokio::test]
async fn compliance_report_rejects_stale_policy_version() {
    let (cp, _tmp) = cp().await;
    let (stale_version_id, input_set_id, document_id) = seed_noncompliant(&cp).await;
    cp.add_policy_version(
        document_id,
        "policy \"container-metadata\" { phase normalize {} }",
    )
    .await
    .unwrap();

    let err = cp
        .generate_compliance_report(stale_version_id, input_set_id)
        .await
        .unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
}

#[tokio::test]
async fn compliance_apply_creates_planned_issue_for_noncompliant_check() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;

    let data = cp
        .apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.issues.created_count, 1);
    assert_eq!(data.issues.updated_count, 0);
    assert_eq!(data.issues.resolved_count, 0);
    assert_eq!(count(&cp, EventKind::IssueOpened).await, 1);
    let issue_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issues")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(issue_count, 1);
}

#[tokio::test]
async fn compliance_apply_creates_open_issue_for_blocked_insufficient_facts() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id) = seed_blocked(&cp).await;

    let data = cp
        .apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.issues.created_count, 1);
    let status: String = sqlx::query_scalar("SELECT status FROM issues")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(status, "open");
}

#[tokio::test]
async fn compliance_apply_is_idempotent_for_repeated_report() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    cp.apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    let second = cp
        .apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(second.issues.created_count, 0);
    assert_eq!(second.issues.updated_count, 0);
    assert_eq!(second.issues.resolved_count, 0);
    assert!(second.issues.skipped_count >= 1);
    assert_eq!(count(&cp, EventKind::IssueOpened).await, 1);
}

#[tokio::test]
async fn compliance_apply_resolves_matching_issue_after_compliance() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, document_id) = seed_compliant(&cp).await;
    let report = cp
        .generate_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();
    let check = report
        .report
        .checks
        .iter()
        .find(|check| check.compliance_kind == "container")
        .unwrap();
    let key = test_dedupe_key(document_id, input_set_id, check);
    sqlx::query(
        "INSERT INTO issues \
         (kind, severity, priority, priority_source, priority_reason, status, title, body, \
          created_at, updated_at, dedupe_key) \
         VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', 'seed', 'planned', \
                 'seed', 'seed', ?, ?, ?)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .bind(&key)
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let data = cp
        .apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.issues.resolved_count, 1);
    assert_eq!(count(&cp, EventKind::IssueResolved).await, 1);
    let status: String = sqlx::query_scalar("SELECT status FROM issues")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(status, "resolved");
}

fn test_dedupe_key(
    policy_document_id: voom_core::PolicyDocumentId,
    input_set_id: voom_core::PolicyInputSetId,
    check: &voom_plan::ComplianceCheck,
) -> String {
    let preimage = serde_json::json!({
        "target": check.target,
        "compliance_kind": check.compliance_kind,
        "operation_kind": check.operation_kind,
    });
    let canonical = voom_plan::hash::canonical_json(&preimage).unwrap();
    format!(
        "policy_noncompliant:v1:policy_document_id={}:input_set_id={}:check={}",
        policy_document_id.0,
        input_set_id.0,
        blake3::hash(canonical.as_bytes()).to_hex()
    )
}

#[tokio::test]
async fn compliance_apply_resolves_matching_issue_when_new_policy_no_longer_emits_check() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, document_id) = seed_noncompliant(&cp).await;
    cp.apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();
    let no_work_version = cp
        .add_policy_version(
            document_id,
            "policy \"container-metadata\" { phase normalize {} }",
        )
        .await
        .unwrap();

    let data = cp
        .apply_compliance_report(no_work_version.id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.issues.resolved_count, 1);
    let status: String = sqlx::query_scalar("SELECT status FROM issues")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(status, "resolved");
}

#[tokio::test]
async fn compliance_apply_does_not_create_issue_for_unsupported_operation() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;

    let data = cp
        .apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.issues.created_count, 1);
    assert!(data.issues.skipped_count >= 3);
    let issue_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issues")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(issue_count, 1);
}

#[tokio::test]
async fn compliance_tool_preflight_fails_before_issue_and_job_writes() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "requires-mkvtoolnix",
            "policy \"requires mkvtoolnix\" {\n  metadata {\n    \
             requires_tools: [mkvtoolnix]\n  }\n  phase normalize {\n    container mkv\n  }\n}\n",
        )
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_video(&cp).await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "requires-mkvtoolnix".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let err = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            super::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();

    assert_eq!(err.source.code(), "POLICY_EXECUTION_ERROR");
    assert!(err.source.to_string().contains("mkvtoolnix"));
    for table in ["issues", "jobs", "tickets", "leases"] {
        let query = format!("SELECT COUNT(*) FROM {table}");
        let rows: i64 = sqlx::query_scalar(&query)
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table} changed before preflight failed");
    }
}

#[tokio::test]
async fn compliance_execution_rejects_unknown_compiled_fields_without_partial_writes() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    cp.apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    sqlx::query("DROP TRIGGER policy_versions_are_immutable")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE policy_versions \
         SET compiled_json = json_set( \
             compiled_json, \
             '$.phases[0].operations[0].future_operation', \
             json('true') \
         ) \
         WHERE id = ?",
    )
    .bind(i64::try_from(policy_version_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let corrupted_json: String =
        sqlx::query_scalar("SELECT compiled_json FROM policy_versions WHERE id = ?")
            .bind(i64::try_from(policy_version_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let before = durable_policy_execution_rows(&cp).await;

    let error = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            policy_version_id,
            input_set_id,
            WorkerRuntimeRegistry::new(),
            super::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.source.code(), "PLAN_GENERATION_ERROR");
    assert!(
        error
            .source
            .to_string()
            .contains("unknown field `future_operation`")
    );
    assert!(error.partial.is_none());
    assert_eq!(before, durable_policy_execution_rows(&cp).await);
    let persisted_json: String =
        sqlx::query_scalar("SELECT compiled_json FROM policy_versions WHERE id = ?")
            .bind(i64::try_from(policy_version_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(persisted_json.as_bytes(), corrupted_json.as_bytes());
    for table in ["jobs", "tickets", "leases"] {
        assert_eq!(count_rows(&cp, table).await, 0, "{table} received a row");
    }
}

#[tokio::test]
async fn compliance_execute_options_reach_policy_remux_ticket_payload() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_video(&cp).await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-remux-roots".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    register_policy_remux_worker(&cp).await;
    let options = super::ComplianceExecutionOptions {
        remux_staging_root: PathBuf::from("/custom/remux/staging"),
        remux_target_dir: PathBuf::from("/custom/remux/output"),
        ..super::ComplianceExecutionOptions::default()
    };

    let err = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            created_policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            options,
        )
        .await
        .unwrap_err();

    assert_eq!(err.source.code(), "CONFIG_INVALID");
    let ticket_payload: String =
        sqlx::query_scalar("SELECT payload FROM tickets WHERE kind = ? ORDER BY id ASC LIMIT 1")
            .bind("synthetic.workflow.operation.remux")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let payload = serde_json::from_str(&ticket_payload).unwrap();
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", payload).unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["staging_root"],
        "/custom/remux/staging"
    );
    // Commits route to a per-operation working dir; promotion later moves the
    // terminal artifact to `/custom/remux/output`.
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/custom/remux/staging/.committed/remux"
    );
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        file_version_id.0
    );
}

#[tokio::test]
async fn compliance_execute_options_reach_policy_audio_ticket_payload() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/audio-transcode-extract.voom").unwrap();
    let created_policy = cp
        .create_policy_document("audio-transcode-extract", &source)
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_audio(&cp).await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-audio-roots".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    register_policy_audio_worker(&cp, OperationKind::TranscodeAudio).await;
    let options = super::ComplianceExecutionOptions {
        audio_staging_root: PathBuf::from("/custom/audio/staging"),
        audio_target_dir: PathBuf::from("/custom/audio/output"),
        ..super::ComplianceExecutionOptions::default()
    };

    let err = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            created_policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            options,
        )
        .await
        .unwrap_err();

    assert_eq!(err.source.code(), "CONFIG_INVALID");
    let ticket_payload: String =
        sqlx::query_scalar("SELECT payload FROM tickets WHERE kind = ? ORDER BY id ASC LIMIT 1")
            .bind("synthetic.workflow.operation.transcode_audio")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let payload = serde_json::from_str(&ticket_payload).unwrap();
    let workflow_payload = WorkflowTicketPayload::parse_ticket(
        "synthetic.workflow.operation.transcode_audio",
        payload,
    )
    .unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["staging_root"],
        "/custom/audio/staging"
    );
    // Commits route to a per-operation working dir; promotion later moves the
    // terminal artifact to `/custom/audio/output`.
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/custom/audio/staging/.committed/audio"
    );
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        file_version_id.0
    );
    assert_eq!(
        workflow_payload.rendered_payload["audio"]["type"],
        "transcode_audio"
    );
}

#[tokio::test]
async fn policy_runtime_registry_loads_transcode_video_workers() {
    let (cp, _tmp) = cp().await;
    let worker_id = register_policy_worker_with_extra(
        &cp,
        OperationKind::TranscodeVideo,
        "policy-test-transcode",
        serde_json::json!({
            "endpoint": "127.0.0.1:9",
            "secret": "policy-transcode-secret",
        }),
    )
    .await;

    let registry = cp.policy_runtime_registry().await.unwrap();

    let runtime = registry.get(worker_id).unwrap();
    assert_eq!(runtime.credentials.worker_id, worker_id);
}

#[tokio::test]
async fn policy_runtime_registry_loads_transcode_audio_workers() {
    let (cp, _tmp) = cp().await;
    let worker_id = register_policy_worker_with_extra(
        &cp,
        OperationKind::TranscodeAudio,
        "policy-test-transcode-audio",
        serde_json::json!({
            "endpoint": "127.0.0.1:9",
            "secret": "policy-transcode-audio-secret",
        }),
    )
    .await;

    let registry = cp.policy_runtime_registry().await.unwrap();

    let runtime = registry.get(worker_id).unwrap();
    assert_eq!(runtime.credentials.worker_id, worker_id);
}

#[tokio::test]
async fn policy_runtime_registry_loads_extract_audio_workers() {
    let (cp, _tmp) = cp().await;
    let worker_id = register_policy_worker_with_extra(
        &cp,
        OperationKind::ExtractAudio,
        "policy-test-extract-audio",
        serde_json::json!({
            "endpoint": "127.0.0.1:9",
            "secret": "policy-extract-audio-secret",
        }),
    )
    .await;

    let registry = cp.policy_runtime_registry().await.unwrap();

    let runtime = registry.get(worker_id).unwrap();
    assert_eq!(runtime.credentials.worker_id, worker_id);
}

#[tokio::test]
async fn live_policy_runtime_registry_drops_unreachable_endpoint() {
    let (cp, _tmp) = cp().await;
    // 127.0.0.1:1 is a closed privileged port: a connection is refused fast,
    // standing in for a stale endpoint left by a hard-killed run-local.
    let worker_id = register_policy_worker_with_extra(
        &cp,
        OperationKind::TranscodeVideo,
        "policy-test-dead-endpoint",
        serde_json::json!({
            "endpoint": "127.0.0.1:1",
            "secret": "policy-dead-secret",
        }),
    )
    .await;

    // The worker is registered, so the unfiltered registry includes it...
    let registered = cp.policy_runtime_registry().await.unwrap();
    assert!(
        registered.get(worker_id).is_ok(),
        "unfiltered registry must include the registered worker"
    );

    // ...but the liveness-filtered registry drops the unreachable endpoint.
    let live = cp.live_policy_runtime_registry().await.unwrap();
    assert!(
        live.get(worker_id).is_err(),
        "liveness check must drop the dead endpoint"
    );
}

#[tokio::test]
async fn execute_reports_actionable_error_when_no_live_worker_for_remux() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_video(&cp).await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-remux-dead-worker".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    register_policy_worker_with_extra(
        &cp,
        OperationKind::Remux,
        "policy-test-remux-dead",
        serde_json::json!({
            "endpoint": "127.0.0.1:1",
            "secret": "remux-dead-secret",
        }),
    )
    .await;

    let err = cp
        .execute_compliance_policy(created_policy.version.id, input.input_set_id)
        .await
        .unwrap_err();

    assert_eq!(err.source.code(), "CONFIG_INVALID");
    let message = err.source.to_string();
    assert!(
        message.contains("no live worker for operation 'remux'"),
        "message must name the missing operation, got: {message}"
    );
    assert!(
        message.contains("voom worker run-local --kind mkvtoolnix"),
        "message must suggest the fix, got: {message}"
    );
    // The check is pre-dispatch: no issues applied and no tickets created.
    let issue_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issues")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(
        issue_count, 0,
        "no issues must be committed before dispatch"
    );
    let ticket_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(
        ticket_count, 0,
        "no tickets must be created before dispatch"
    );
}

#[tokio::test]
async fn compliance_execute_verifies_existing_active_artifact_through_bundled_worker() {
    let (cp, _tmp) = cp().await;
    let media_dir = tempfile::tempdir().unwrap();
    let media_path = media_dir.path().join("movie.mkv");
    tokio::fs::write(&media_path, b"published policy verification")
        .await
        .unwrap();
    let policy = cp
        .create_policy_document(
            "verify-existing-artifact",
            "policy \"verify existing artifact\" { phase verify { verify artifact } }",
        )
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot_for_existing_file(&cp, &media_path).await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "verify-existing-artifact".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let data = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            super::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(data.file_phases.len(), 1);
    assert_eq!(data.file_phases[0].outcome, "verified");
    assert_eq!(
        data.file_phases[0].produced_file_version_id,
        Some(file_version_id.0)
    );
    assert!(data.file_phases[0].artifact_handle_id.is_some());
    assert!(data.file_phases[0].artifact_verification_id.is_some());
    assert_eq!(data.summary.progress.completed, 1);
    assert_eq!(data.artifact_verifications.len(), 1);
    assert_eq!(data.artifact_verifications[0].status, "succeeded");
    let report = cp
        .read_compliance_run_report(voom_core::JobId(data.summary.job_id))
        .await
        .unwrap();
    assert_eq!(report.artifact_verifications.len(), 1);
    assert_eq!(
        report.artifact_verifications[0].verification_id,
        data.artifact_verifications[0].verification_id
    );
    let evidence: (String, i64, i64) = sqlx::query_as(
        "SELECT status, workflow_ticket_id, workflow_lease_id \
         FROM artifact_verifications",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(evidence.0, "succeeded");
    assert!(evidence.1 > 0);
    assert!(evidence.2 > 0);
    assert_eq!(count(&cp, EventKind::ArtifactVerificationStarted).await, 1);
    assert_eq!(
        count(&cp, EventKind::ArtifactVerificationSucceeded).await,
        1
    );
}

#[tokio::test]
async fn failed_policy_verification_persists_evidence_and_gates_downstream_phase() {
    let (cp, _tmp) = cp().await;
    let media_dir = tempfile::tempdir().unwrap();
    let media_path = media_dir.path().join("movie.mkv");
    tokio::fs::write(&media_path, b"bytes that do not match durable provenance")
        .await
        .unwrap();
    let policy = cp
        .create_policy_document(
            "verify-failure-gates-downstream",
            "policy \"verify failure gates downstream\" { \
               phase verify { verify artifact } \
               phase downstream { depends_on: [verify] verify artifact } \
             }",
        )
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot_for_existing_file(&cp, &media_path).await;
    sqlx::query("UPDATE file_versions SET content_hash = 'blake3:wrong' WHERE id = ?")
        .bind(i64::try_from(file_version_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "verify-failure-gates-downstream".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let error = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            super::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.source.code(), "ARTIFACT_CHECKSUM_MISMATCH");
    let partial = error.partial.unwrap();
    assert_eq!(partial.summary.failure_count, 1);
    assert!(partial.file_phases.is_empty());
    assert_eq!(partial.artifact_verifications.len(), 1);
    assert_eq!(partial.artifact_verifications[0].status, "failed");
    assert_eq!(
        partial.artifact_verifications[0].error_code.as_deref(),
        Some("ARTIFACT_CHECKSUM_MISMATCH")
    );
    let stored = cp
        .read_compliance_run_report(voom_core::JobId(partial.summary.job_id))
        .await
        .unwrap();
    assert_eq!(stored.artifact_verifications.len(), 1);
    assert_eq!(stored.artifact_verifications[0].status, "failed");

    let states: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, state FROM tickets ORDER BY id")
            .fetch_all(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(
        states,
        vec![(
            "synthetic.workflow.operation.verify_artifact".to_owned(),
            "failed".to_owned()
        )],
        "the dependent phase must never create or dispatch a ticket"
    );
    let job_state: String = sqlx::query_scalar("SELECT state FROM jobs")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let lease_states: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT ticket_id, state, release_reason FROM leases ORDER BY id")
            .fetch_all(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(job_state, "failed");
    assert_eq!(
        lease_states,
        vec![(1, "released".to_owned(), Some("failed_terminal".to_owned()))]
    );
    assert_eq!(count(&cp, EventKind::ArtifactVerificationFailed).await, 1);
    assert_eq!(
        count(&cp, EventKind::ArtifactVerificationSucceeded).await,
        0
    );
}

#[tokio::test]
async fn resume_carries_verified_phase_without_duplicate_verification() {
    let (cp, _tmp) = cp().await;
    let media_dir = tempfile::tempdir().unwrap();
    let media_path = media_dir.path().join("movie.mkv");
    tokio::fs::write(&media_path, b"resume verified bytes")
        .await
        .unwrap();
    let policy = cp
        .create_policy_document(
            "resume-verified-phase",
            "policy \"resume verified phase\" { \
               phase verify { verify artifact } \
               phase normalize { depends_on: [verify] container mkv } \
             }",
        )
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot_for_existing_file(&cp, &media_path).await;
    sqlx::query(
        "UPDATE media_snapshots \
         SET payload = json_set(payload, '$.container.format_name', 'mp4') \
         WHERE id = ?",
    )
    .bind(i64::try_from(media_snapshot_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "resume-verified-phase".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let first = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            super::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();
    let first = first.partial.unwrap();
    assert_eq!(first.file_phases.len(), 1);
    assert_eq!(first.file_phases[0].outcome, "verified");

    let resumed = cp
        .resume_phase_barrier_with_runtimes(
            voom_core::JobId(first.summary.job_id),
            policy.version.id,
            input.input_set_id,
            super::ComplianceExecutionOptions::default(),
            WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap_err();
    let resumed = resumed
        .partial
        .unwrap_or_else(|| panic!("resumed second phase must be partial: {}", resumed.source));

    assert_eq!(resumed.file_phases.len(), 1);
    assert_eq!(
        resumed.file_phases[0].outcome.as_str(),
        "verified",
        "the replacement job must carry the read-only phase"
    );
    assert!(resumed.file_phases[0].ticket_ids.is_empty());
    let evidence_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_verifications")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(evidence_count, 1);
    assert_eq!(count(&cp, EventKind::ArtifactVerificationStarted).await, 1);
}

#[tokio::test]
async fn resume_adopts_successful_evidence_missing_its_phase_row() {
    let (cp, _tmp) = cp().await;
    let media_dir = tempfile::tempdir().unwrap();
    let media_path = media_dir.path().join("movie.mkv");
    tokio::fs::write(&media_path, b"crash-window verified bytes")
        .await
        .unwrap();
    let policy = cp
        .create_policy_document(
            "resume-verification-crash-window",
            "policy \"resume verification crash window\" { \
               phase verify { verify artifact } \
             }",
        )
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot_for_existing_file(&cp, &media_path).await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "resume-verification-crash-window".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    let completed = cp
        .execute_compliance_policy_with_runtime_registry_and_options_for_test(
            policy.version.id,
            input.input_set_id,
            WorkerRuntimeRegistry::new(),
            super::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap();
    let prior_job_id = voom_core::JobId(completed.summary.job_id);

    sqlx::query("DELETE FROM workflow_file_phase_summaries WHERE job_id = ?")
        .bind(i64::try_from(prior_job_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query("DELETE FROM workflow_phase_summaries WHERE job_id = ?")
        .bind(i64::try_from(prior_job_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query("DELETE FROM workflow_summaries WHERE job_id = ?")
        .bind(i64::try_from(prior_job_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET state = 'failed' WHERE id = ?")
        .bind(i64::try_from(prior_job_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let prior_ticket: (String, String, String) =
        sqlx::query_as("SELECT state, payload, result FROM tickets WHERE job_id = ?")
            .bind(i64::try_from(prior_job_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let prior_payload: serde_json::Value = serde_json::from_str(&prior_ticket.1).unwrap();
    assert_eq!(prior_ticket.0, "succeeded");
    assert_eq!(prior_payload["workflow_id"], "workflow-1-phase-0");
    assert_eq!(prior_payload["branch_id"], "root");

    Box::pin(assert_corrupted_verification_result_rejected(
        &cp,
        prior_job_id,
        policy.version.id,
        input.input_set_id,
        &prior_ticket.2,
    ))
    .await;

    let resumed = cp
        .resume_phase_barrier_with_runtimes(
            prior_job_id,
            policy.version.id,
            input.input_set_id,
            super::ComplianceExecutionOptions::default(),
            WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap();

    assert_eq!(resumed.file_phases.len(), 1);
    assert_eq!(resumed.file_phases[0].outcome.as_str(), "verified");
    assert!(resumed.file_phases[0].ticket_ids.is_empty());
    let evidence_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_verifications")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(evidence_count, 1);
    assert_eq!(count(&cp, EventKind::ArtifactVerificationStarted).await, 1);
}

async fn assert_corrupted_verification_result_rejected(
    cp: &crate::ControlPlane,
    prior_job_id: voom_core::JobId,
    policy_version_id: voom_core::PolicyVersionId,
    input_set_id: voom_core::PolicyInputSetId,
    original_result: &str,
) {
    sqlx::query(
        "UPDATE tickets SET result = json_set(result, '$.path', '/wrong') WHERE job_id = ?",
    )
    .bind(i64::try_from(prior_job_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let corrupted = cp
        .resume_phase_barrier_with_runtimes(
            prior_job_id,
            policy_version_id,
            input_set_id,
            super::ComplianceExecutionOptions::default(),
            WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        corrupted.source,
        voom_core::VoomError::Conflict(_)
    ));
    let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(
        job_count, 1,
        "corrupt evidence must fail before opening a job"
    );
    sqlx::query("UPDATE tickets SET result = ? WHERE job_id = ?")
        .bind(original_result)
        .bind(i64::try_from(prior_job_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

#[tokio::test]
async fn report_mutates_no_durable_work_or_issue_tables() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    let before = boundary_counts(&cp).await;

    cp.generate_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(before, boundary_counts(&cp).await);
}

#[tokio::test]
async fn apply_mutates_only_issues_and_issue_events() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    let before = boundary_counts(&cp).await;

    cp.apply_compliance_report(policy_version_id, input_set_id)
        .await
        .unwrap();

    let after = boundary_counts(&cp).await;
    assert!(after.count("issues") > before.count("issues"));
    assert!(after.count("events") > before.count("events"));
    assert_eq!(after.count("jobs"), before.count("jobs"));
    assert_eq!(after.count("tickets"), before.count("tickets"));
    assert_eq!(after.count("leases"), before.count("leases"));
    assert_eq!(
        after.count("artifact_handles"),
        before.count("artifact_handles")
    );
}

const REPORT_READ_ONLY_TABLES: &[&str] = &[
    "issues",
    "events",
    "jobs",
    "tickets",
    "leases",
    "workers",
    "worker_capabilities",
    "worker_grants",
    "artifact_handles",
    "artifact_locations",
    "artifact_lineage",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundaryCounts(Vec<(&'static str, i64)>);

impl BoundaryCounts {
    fn count(&self, table: &str) -> i64 {
        self.0
            .iter()
            .find_map(|(name, count)| (*name == table).then_some(*count))
            .unwrap()
    }
}

async fn boundary_counts(cp: &crate::ControlPlane) -> BoundaryCounts {
    BoundaryCounts(table_counts(cp).await)
}

async fn table_counts(cp: &crate::ControlPlane) -> Vec<(&'static str, i64)> {
    let mut counts = Vec::with_capacity(REPORT_READ_ONLY_TABLES.len());
    for table in REPORT_READ_ONLY_TABLES {
        counts.push((*table, count_rows(cp, table).await));
    }
    counts
}

async fn count_rows(cp: &crate::ControlPlane, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar::<_, i64>(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

const DURABLE_POLICY_EXECUTION_TABLES: &[&str] = &[
    "issues",
    "events",
    "policy_input_sets",
    "policy_input_set_fixture_labels",
    "policy_input_synthetic_targets",
    "policy_media_snapshot_inputs",
    "policy_identity_evidence_inputs",
    "policy_bundle_target_inputs",
    "policy_quality_profile_selections",
    "policy_issue_inputs",
];

async fn durable_policy_execution_rows(cp: &crate::ControlPlane) -> Vec<(&'static str, String)> {
    let mut snapshots = Vec::with_capacity(DURABLE_POLICY_EXECUTION_TABLES.len());
    for table in DURABLE_POLICY_EXECUTION_TABLES {
        snapshots.push((*table, ordered_table_json(cp, table).await));
    }
    snapshots
}

async fn ordered_table_json(cp: &crate::ControlPlane, table: &str) -> String {
    let table_identifier = quoted_identifier(table);
    let pragma = format!("PRAGMA table_info({table_identifier})");
    let columns = sqlx::query(&pragma)
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();
    assert!(!columns.is_empty(), "{table} has no columns");
    let object_fields = columns
        .iter()
        .map(|column| {
            let literal = column.replace('\'', "''");
            format!("'{literal}', {}", quoted_identifier(column))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let order = columns
        .iter()
        .map(|column| quoted_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT COALESCE(json_group_array(json(row_json)), '[]') \
         FROM ( \
             SELECT json_object({object_fields}) AS row_json \
             FROM {table_identifier} \
             ORDER BY {order} \
         )"
    );
    sqlx::query_scalar(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn scanned_snapshot_with_video(
    cp: &crate::ControlPlane,
) -> (voom_core::FileVersionId, voom_core::MediaSnapshotId) {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/remux-roots.mp4".to_owned(),
                content_hash: "hash-remux-roots".to_owned(),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({
                "format": "test",
                "container": { "format_name": "mp4" },
                "streams": [
                    {
                        "id": "stream-0",
                        "index": 0,
                        "kind": "video",
                        "codec_name": "h264"
                    }
                ]
            }),
            T0,
        )
        .await
        .unwrap();
    (file_version_id, snapshot.id)
}

async fn scanned_snapshot_for_existing_file(
    cp: &crate::ControlPlane,
    path: &std::path::Path,
) -> (voom_core::FileVersionId, voom_core::MediaSnapshotId) {
    let facts = crate::scan::hash::observe_candidate_file(path)
        .await
        .unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.display().to_string(),
                content_hash: facts.content_hash,
                size_bytes: facts.size_bytes,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({
                "format": "test",
                "container": { "format_name": "mkv" },
                "streams": [{
                    "id": "stream-0",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264"
                }]
            }),
            T0,
        )
        .await
        .unwrap();
    (file_version_id, snapshot.id)
}

async fn scanned_snapshot_with_audio(
    cp: &crate::ControlPlane,
) -> (voom_core::FileVersionId, voom_core::MediaSnapshotId) {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/audio-roots.mkv".to_owned(),
                content_hash: "hash-audio-roots".to_owned(),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({
                "format": "test",
                "container": { "format_name": "mkv" },
                "streams": [
                    {
                        "id": "stream-0",
                        "index": 0,
                        "kind": "video",
                        "codec_name": "h264"
                    },
                    {
                        "id": "audio-1",
                        "index": 1,
                        "kind": "audio",
                        "codec_name": "opus",
                        "language": "eng",
                        "title": "Main",
                        "channels": 2,
                        "disposition": {
                            "default": false,
                            "forced": false,
                            "commentary": false
                        }
                    },
                    {
                        "id": "audio-2",
                        "index": 2,
                        "kind": "audio",
                        "codec_name": "opus",
                        "language": "eng",
                        "title": "Commentary",
                        "channels": 2,
                        "disposition": {
                            "default": false,
                            "forced": false,
                            "commentary": true
                        }
                    }
                ]
            }),
            T0,
        )
        .await
        .unwrap();
    (file_version_id, snapshot.id)
}

async fn register_policy_remux_worker(cp: &crate::ControlPlane) -> voom_core::WorkerId {
    register_policy_worker_with_extra(
        cp,
        OperationKind::Remux,
        "policy-test-remux",
        serde_json::json!({}),
    )
    .await
}

async fn register_policy_worker_with_extra(
    cp: &crate::ControlPlane,
    operation: OperationKind,
    name: &str,
    extra: serde_json::Value,
) -> voom_core::WorkerId {
    let worker = cp
        .register_worker(NewWorker {
            name: name.to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    let operation_name = operation_name(operation);
    let operation = TicketOperation::new(operation_name).unwrap();
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra,
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: serde_json::json!({ operation_name: 1 }),
    })
    .await
    .unwrap();
    worker.id
}

async fn register_policy_audio_worker(
    cp: &crate::ControlPlane,
    operation: OperationKind,
) -> voom_core::WorkerId {
    register_policy_worker_with_extra(cp, operation, "policy-test-audio", serde_json::json!({}))
        .await
}

fn operation_name(operation: OperationKind) -> &'static str {
    operation.as_str()
}

#[tokio::test]
async fn unknown_named_profile_blocks_before_planning() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "transcode-unknown-profile",
            "policy \"transcode unknown profile\" { phase normalize { transcode video to hevc using profile \"nope\" } }",
        )
        .await
        .unwrap();
    let input_set_id = transcodable_input(&cp, "transcode-unknown-input").await;

    let err = cp
        .generate_compliance_report(policy.version.id, input_set_id)
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn known_named_profile_resolves_default_hevc_before_planning() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "transcode-default-hevc",
            "policy \"transcode default hevc\" { phase normalize { transcode video to hevc } }",
        )
        .await
        .unwrap();
    let input_set_id = transcodable_input(&cp, "transcode-default-input").await;

    let data = cp
        .generate_compliance_report(policy.version.id, input_set_id)
        .await
        .unwrap();

    let node = data
        .plan
        .nodes
        .iter()
        .find(|node| node.operation_kind == PlanOperationKind::TranscodeVideo)
        .unwrap();
    assert_eq!(node.status, voom_plan::NodeStatus::Planned);
    assert_eq!(node.operation_payload["profile"], "default-hevc");
    assert_eq!(
        node.operation_payload["resolved_profile"]["encoder"],
        "libx265"
    );
    assert_eq!(node.operation_payload["resolved_profile"]["crf"], 23);
}

#[tokio::test]
async fn read_compliance_run_report_unknown_job_is_not_found() {
    let (cp, _tmp) = cp().await;

    let err = cp
        .read_compliance_run_report(voom_core::JobId(999_999))
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            voom_core::VoomError::NotFound(ref message)
                if message.contains("no job with id 999999")
        ),
        "unknown job must be NotFound(no job with id), got {err:?}"
    );
}

#[tokio::test]
async fn compliance_audio_extract_outputs_preserve_ticket_and_descriptor_order() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(voom_store::repo::jobs::NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: TicketOperation::new("synthetic.workflow.operation.extract_audio").unwrap(),
            priority: 0,
            payload: serde_json::json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    let result = serde_json::json!({
        "result_file_location_id": 11,
        "outputs": [
            published_extract_output("output-a", "a-1", 1, 11, 1),
            published_extract_output("output-b", "a-2", 2, 12, 2)
        ]
    });
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ? WHERE id = ?",
    )
    .bind(serde_json::to_string(&result).unwrap())
    .bind("1970-01-01T00:00:00Z")
    .bind(i64::try_from(ticket.id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let outputs = cp.audio_extract_outputs_for_job(job.id).await.unwrap();

    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].output_id(), Some("output-a"));
    assert_eq!(outputs[1].output_id(), Some("output-b"));

    let legacy = cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: TicketOperation::new("synthetic.workflow.operation.extract_audio").unwrap(),
            priority: 0,
            payload: serde_json::json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    let legacy_result = historical_extract_result();
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ? WHERE id = ?",
    )
    .bind(serde_json::to_string(&legacy_result).unwrap())
    .bind("1970-01-01T00:00:00Z")
    .bind(i64::try_from(legacy.id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let outputs = cp.audio_extract_outputs_for_job(job.id).await.unwrap();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[2].result_file_location_id(), 26);
    assert!(outputs[2].is_legacy_singleton());
}

fn published_extract_output(
    output_id: &str,
    source_snapshot_stream_id: &str,
    source_provider_stream_index: u32,
    result_file_location_id: u64,
    seed: u64,
) -> serde_json::Value {
    serde_json::json!({
        "operation_output_id": 100 + seed,
        "output_id": output_id,
        "source_file_version_id": 200 + seed,
        "source_media_snapshot_id": 300 + seed,
        "source_snapshot_stream_id": source_snapshot_stream_id,
        "source_provider_stream_index": source_provider_stream_index,
        "role": "extract_audio_sidecar",
        "staged_artifact_handle_id": 400 + seed,
        "staged_artifact_location_id": 500 + seed,
        "verification_id": 600 + seed,
        "commit_record_id": 700 + seed,
        "result_file_version_id": 800 + seed,
        "result_file_location_id": result_file_location_id,
        "result_file_asset_id": 900 + seed,
        "result_media_snapshot_id": 1000 + seed,
        "bundle_member_id": 1100 + seed,
        "lineage_id": 1200 + seed,
        "staging_path": format!("/stage/{output_id}.ogg"),
        "target_path": format!("/target/{output_id}.ogg")
    })
}

#[derive(serde::Serialize)]
struct HistoricalExecuteExtractAudioReport {
    job_id: u64,
    ticket_id: u64,
    lease_id: u64,
    source_file_version_id: u64,
    source_file_location_id: u64,
    staged_artifact_handle_id: u64,
    staged_artifact_location_id: u64,
    verification_id: u64,
    commit_record_id: u64,
    result_file_version_id: u64,
    result_file_location_id: u64,
    staging_path: &'static str,
    target_path: &'static str,
    commit_recovery_required: Option<serde_json::Value>,
}

fn historical_extract_result() -> serde_json::Value {
    serde_json::to_value(HistoricalExecuteExtractAudioReport {
        job_id: 1,
        ticket_id: 2,
        lease_id: 3,
        source_file_version_id: 20,
        source_file_location_id: 21,
        staged_artifact_handle_id: 21,
        staged_artifact_location_id: 22,
        verification_id: 23,
        commit_record_id: 24,
        result_file_version_id: 25,
        result_file_location_id: 26,
        staging_path: "/stage/legacy.ogg",
        target_path: "/target/legacy.ogg",
        commit_recovery_required: Some(serde_json::json!({
            "recovery_reason": "historical recovery payload remains opaque"
        })),
    })
    .unwrap()
}

#[test]
fn compliance_audio_extract_outputs_reject_incomplete_published_members() {
    let result = serde_json::json!({
        "outputs": [{
            "output_id": "incomplete",
            "result_file_location_id": 1
        }]
    });

    let error = super::decode_compliance_extract_result(42, &result.to_string()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("audio extraction ticket 42 published output is malformed")
    );
}

#[test]
fn compliance_audio_extract_outputs_reject_unknown_published_fields() {
    let mut output = published_extract_output("output-a", "a-1", 1, 11, 1);
    output["unexpected"] = serde_json::json!(true);
    let result = serde_json::json!({ "outputs": [output] });

    let error = super::decode_compliance_extract_result(42, &result.to_string()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("audio extraction ticket 42 published output is malformed")
    );
}

#[test]
fn compliance_audio_extract_outputs_reject_unknown_legacy_fields() {
    let mut result = historical_extract_result();
    result["unexpected"] = serde_json::json!(true);

    let error = super::decode_compliance_extract_result(42, &result.to_string()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("audio extraction ticket 42 legacy result is malformed")
    );
}

#[tokio::test]
async fn compliance_audio_synthesis_companions_preserve_ticket_and_descriptor_order() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(voom_store::repo::jobs::NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let replacement = create_transcode_audio_ticket(&cp, job.id).await;
    let synthesis = create_transcode_audio_ticket(&cp, job.id).await;
    set_succeeded_ticket_result(
        &cp,
        replacement.id,
        &serde_json::json!({"result_file_location_id": 10}),
    )
    .await;
    set_succeeded_ticket_result(
        &cp,
        synthesis.id,
        &serde_json::json!({
            "synthesis_operation_id": "node-synthesis",
            "synthesis_operation_key": "synthesize:7:node-synthesis",
            "synthesized_companions": [
                published_synthesis_companion("companion-a", "a-1", 1, 3, 1),
                published_synthesis_companion("companion-b", "a-2", 2, 4, 2)
            ]
        }),
    )
    .await;

    let companions = cp.audio_synthesis_companions_for_job(job.id).await.unwrap();

    assert_eq!(companions.len(), 2);
    assert_eq!(companions[0].synthesis_operation_id, "node-synthesis");
    assert_eq!(
        companions[0].synthesis_operation_key,
        "synthesize:7:node-synthesis"
    );
    assert_eq!(companions[0].companion.companion_id, "companion-a");
    assert_eq!(companions[1].companion.companion_id, "companion-b");
    assert_eq!(companions[1].companion.result_provider_stream_index, 4);
}

#[test]
fn compliance_audio_synthesis_companions_reject_incomplete_or_unknown_fields() {
    let incomplete = serde_json::json!({
        "synthesis_operation_id": "node-synthesis",
        "synthesized_companions": [
            published_synthesis_companion("companion-a", "a-1", 1, 3, 1)
        ]
    });
    let error = super::decode_compliance_synthesis_result(42, &incomplete.to_string()).unwrap_err();
    assert!(error.to_string().contains(
        "audio synthesis ticket 42 must contain operation id, operation key, and non-empty"
    ));

    let mut companion = published_synthesis_companion("companion-a", "a-1", 1, 3, 1);
    companion["unexpected"] = serde_json::json!(true);
    let malformed = serde_json::json!({
        "synthesis_operation_id": "node-synthesis",
        "synthesis_operation_key": "synthesize:7:node-synthesis",
        "synthesized_companions": [companion]
    });
    let error = super::decode_compliance_synthesis_result(42, &malformed.to_string()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("audio synthesis ticket 42 companion is malformed")
    );
}

async fn create_transcode_audio_ticket(
    cp: &crate::ControlPlane,
    job_id: voom_core::JobId,
) -> voom_store::repo::tickets::Ticket {
    cp.create_ticket(NewTicket {
        job_id: Some(job_id),
        kind: TicketOperation::new("synthetic.workflow.operation.transcode_audio").unwrap(),
        priority: 0,
        payload: serde_json::json!({}),
        max_attempts: 1,
        created_at: T0,
    })
    .await
    .unwrap()
}

async fn set_succeeded_ticket_result(
    cp: &crate::ControlPlane,
    ticket_id: voom_core::TicketId,
    result: &serde_json::Value,
) {
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ? WHERE id = ?",
    )
    .bind(serde_json::to_string(result).unwrap())
    .bind("1970-01-01T00:00:00Z")
    .bind(i64::try_from(ticket_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

fn published_synthesis_companion(
    companion_id: &str,
    source_snapshot_stream_id: &str,
    source_provider_stream_index: u32,
    result_provider_stream_index: u32,
    seed: u64,
) -> serde_json::Value {
    serde_json::json!({
        "ordinal": seed - 1,
        "companion_id": companion_id,
        "source_file_version_id": 100 + seed,
        "source_media_snapshot_id": 200 + seed,
        "source_snapshot_stream_id": source_snapshot_stream_id,
        "source_provider_stream_index": source_provider_stream_index,
        "result_file_version_id": 300 + seed,
        "result_file_location_id": 400 + seed,
        "result_media_snapshot_id": 500 + seed,
        "result_snapshot_stream_id": companion_id,
        "result_provider_stream_index": result_provider_stream_index,
        "artifact_handle_id": 600 + seed,
        "artifact_location_id": 700 + seed,
        "lineage_id": 800 + seed,
        "location": format!("/target/{companion_id}.mkv"),
        "codec": "aac",
        "channels": 2,
        "language": "eng",
        "title": "Main",
        "disposition_default": true,
        "disposition_forced": false,
        "disposition_commentary": false
    })
}

#[tokio::test]
async fn read_compliance_run_report_in_flight_job_has_no_summary() {
    let (cp, _tmp) = cp().await;
    // A job that opened but never finalized a workflow summary row: the read must
    // distinguish "still running / not a workflow job" from an unknown job id.
    let job = cp
        .open_job(voom_store::repo::jobs::NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();

    let err = cp.read_compliance_run_report(job.id).await.unwrap_err();

    assert!(
        matches!(
            err,
            voom_core::VoomError::NotFound(ref message)
                if message.contains("no completed workflow summary")
        ),
        "in-flight job must be NotFound(no completed workflow summary), got {err:?}"
    );
}

#[tokio::test]
async fn read_compliance_run_report_zero_phase_job_is_ok_and_empty() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    // The compliant-baseline input set targets synthetic variants, so the
    // coordinator's active *file* set is empty: a job opens with a summary row
    // but records zero phase rows.
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    let outcome = cp
        .run_phase_barrier(
            created.version.id,
            input.id,
            crate::cases::policy::compliance::ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap();

    let view = cp.read_compliance_run_report(outcome.job_id).await.unwrap();

    assert_eq!(view.summary.job_id, outcome.job_id.0);
    assert!(view.phases.is_empty(), "no file targets => no phase rows");
    assert!(view.file_phases.is_empty());
    assert_eq!(view.latest_phase_index, None);
}

#[tokio::test]
async fn read_compliance_run_report_orders_phases_and_points_at_latest() {
    use voom_store::repo::workflow_summaries::{
        NewPhaseSummary, NewWorkflowSummary, PhaseOutcome, PhaseReport,
    };

    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(voom_store::repo::jobs::NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.workflow_summaries()
        .insert_summary(
            NewWorkflowSummary {
                job_id: job.id,
                branch_count: 1,
                ticket_count: 2,
                dispatch_count: 2,
                retry_count: 0,
                failure_count: 0,
                peak_active_workflow_leases: 1,
                elapsed: std::time::Duration::from_millis(1),
                per_operation: serde_json::json!({}),
            },
            T0,
        )
        .await
        .unwrap();
    // Insert ordinal 1 before ordinal 0 to prove the read returns them ascending
    // regardless of write order.
    for (ordinal, name) in [(1u32, "audio"), (0u32, "remux")] {
        cp.workflow_summaries()
            .upsert_phase_summary(
                NewPhaseSummary {
                    job_id: job.id,
                    phase_ordinal: ordinal,
                    phase_name: name.to_owned(),
                    outcome: PhaseOutcome::Completed,
                    report: Some(PhaseReport {
                        report_id: format!("report_{name}"),
                        report: serde_json::json!({ "report_id": format!("report_{name}") }),
                    }),
                },
                T0,
            )
            .await
            .unwrap();
    }

    let view = cp.read_compliance_run_report(job.id).await.unwrap();

    assert_eq!(view.phases.len(), 2);
    assert_eq!(view.phases[0].phase_ordinal, 0);
    assert_eq!(view.phases[0].phase_name, "remux");
    assert_eq!(view.phases[1].phase_ordinal, 1);
    assert_eq!(view.phases[1].phase_name, "audio");
    assert_eq!(view.latest_phase_index, Some(1));
    assert_eq!(
        view.phases[view.latest_phase_index.unwrap()]
            .report_id
            .as_deref(),
        Some("report_audio"),
        "latest index points at the highest-ordinal phase's report"
    );
}

fn backup_evidence_plan(nodes: Vec<voom_plan::PlanNode>) -> voom_plan::ExecutionPlan {
    voom_plan::ExecutionPlan {
        schema_version: 1,
        plan_id: "plan_backup_evidence".to_owned(),
        plan_hash: "blake3:plan".to_owned(),
        policy: voom_plan::PolicyIdentity {
            slug: "backup".to_owned(),
            source_hash: "abc".to_owned(),
            document_id: Some(voom_core::PolicyDocumentId(1)),
            version_id: Some(voom_core::PolicyVersionId(2)),
        },
        input: voom_plan::InputIdentity {
            slug: Some("synthetic".to_owned()),
            source_label: None,
            input_set_id: Some(voom_core::PolicyInputSetId(3)),
            fixture_labels: vec!["synthetic".to_owned()],
        },
        generated_at: None,
        summary: voom_plan::PlanSummary::default(),
        nodes,
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: voom_plan::PlanProvenance::default(),
    }
}

fn file_version_target_node(file_version_id: u64) -> voom_plan::PlanNode {
    voom_plan::PlanNode {
        node_id: "target".to_owned(),
        phase_name: "normalize".to_owned(),
        ordinal: 0,
        target: voom_plan::TargetRef::FileVersion {
            id: voom_core::FileVersionId(file_version_id),
        },
        operation_kind: voom_plan::PlanOperationKind::Remux,
        operation_payload: serde_json::json!({}),
        observed_state: None,
        status: voom_plan::NodeStatus::Planned,
        status_reason: String::new(),
        capability_hints: voom_plan::CapabilityHints::default(),
        scheduling_hints: voom_plan::SchedulingHints::default(),
        resource_estimates: voom_plan::ResourceEstimates::default(),
        artifact_expectations: voom_plan::ArtifactExpectations::default(),
        safety_hints: voom_plan::SafetyHints::default(),
    }
}

fn synthetic_node() -> voom_plan::PlanNode {
    voom_plan::PlanNode {
        target: voom_plan::TargetRef::Synthetic {
            key: "movie-a".to_owned(),
            kind: voom_policy::TargetKind::MediaWork,
        },
        ..file_version_target_node(0)
    }
}

#[test]
fn plan_file_version_targets_collects_file_version_targets_only() {
    let plan = backup_evidence_plan(vec![
        file_version_target_node(11),
        synthetic_node(),
        file_version_target_node(22),
    ]);

    let ids: Vec<u64> = super::plan_file_version_targets(&plan)
        .into_iter()
        .map(|id| id.0)
        .collect();

    assert_eq!(ids, vec![11, 22]);
}

#[tokio::test]
async fn backup_evidence_for_plan_surfaces_seeded_backups() {
    let (cp, _tmp) = cp().await;
    let pool = cp.pool_for_test();
    let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let file_version_id = voom_core::FileVersionId(
        u64::try_from(
            sqlx::query(
                "INSERT INTO file_versions \
                 (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
                 VALUES (?, 'blake3:source', 3, 'external_observed', ?)",
            )
            .bind(file_asset_id)
            .bind("1970-01-01T00:00:00Z")
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid(),
        )
        .unwrap(),
    );
    let job_id = voom_core::JobId(
        u64::try_from(
            sqlx::query(
                "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
                 VALUES ('backup-test', 'open', 0, ?, ?)",
            )
            .bind("1970-01-01T00:00:00Z")
            .bind("1970-01-01T00:00:00Z")
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid(),
        )
        .unwrap(),
    );
    let ticket_id = voom_core::TicketId(
        u64::try_from(
            sqlx::query(
                "INSERT INTO tickets \
                 (job_id, kind, state, priority, payload, attempt, max_attempts, \
                  next_eligible_at, created_at, state_changed_at) \
                 VALUES (?, 'backup-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
            )
            .bind(i64::try_from(job_id.0).unwrap())
            .bind("1970-01-01T00:00:00Z")
            .bind("1970-01-01T00:00:00Z")
            .bind("1970-01-01T00:00:00Z")
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid(),
        )
        .unwrap(),
    );

    let backup = cp
        .backups
        .insert_pending(
            voom_store::repo::backups::NewBackup {
                source_file_version_id: file_version_id,
                job_id,
                ticket_id,
                provider: "voom-backup-worker".to_owned(),
                destination_path: format!("/backups/v{}/movie.mkv", file_version_id.0),
            },
            T0,
        )
        .await
        .unwrap();
    cp.backups
        .mark_verified(backup.id, 3, "blake3:source", T0)
        .await
        .unwrap();

    let plan = backup_evidence_plan(vec![file_version_target_node(file_version_id.0)]);
    let evidence = cp.backup_evidence_for_plan(&plan).await.unwrap();

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].source_file_version_id, file_version_id.0);
    assert_eq!(evidence[0].status, "verified");
    assert_eq!(evidence[0].checksum.as_deref(), Some("blake3:source"));
}
