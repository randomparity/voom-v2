use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use voom_events::EventKind;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, OperationKind, OperationRequest, OperationResponse,
    ProgressFrame, ProtocolError, WorkerCredentials,
};

use crate::cases::policy_inputs::PolicyInputFromScanInput;
use crate::cases::{count, cp};
use crate::workflow::{
    WorkerRuntimeRegistry, executor::WorkflowExecutorOptions, ticket_payload::WorkflowTicketPayload,
};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[test]
fn compliance_execution_defaults_use_production_transcode_paths() {
    let workflow_defaults = WorkflowExecutorOptions::default();
    let compliance_defaults = super::ComplianceExecutionOptions::default();

    assert_eq!(
        compliance_defaults.transcode_staging_root,
        workflow_defaults.transcode_staging_root
    );
    assert_eq!(
        compliance_defaults.transcode_target_dir,
        workflow_defaults.transcode_target_dir
    );
}

#[test]
fn compliance_execution_defaults_use_production_remux_paths() {
    let workflow_defaults = WorkflowExecutorOptions::default();
    let compliance_defaults = super::ComplianceExecutionOptions::default();

    assert_eq!(
        compliance_defaults.remux_staging_root,
        workflow_defaults.remux_staging_root
    );
    assert_eq!(
        compliance_defaults.remux_target_dir,
        workflow_defaults.remux_target_dir
    );
}

#[test]
fn compliance_execution_defaults_use_production_audio_paths() {
    let workflow_defaults = WorkflowExecutorOptions::default();
    let compliance_defaults = super::ComplianceExecutionOptions::default();

    assert_eq!(
        compliance_defaults.audio_staging_root,
        workflow_defaults.audio_staging_root
    );
    assert_eq!(
        compliance_defaults.audio_target_dir,
        workflow_defaults.audio_target_dir
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
    };

    let converted = WorkflowExecutorOptions::from(options.clone());

    assert_eq!(
        converted.transcode_staging_root,
        options.transcode_staging_root
    );
    assert_eq!(converted.transcode_target_dir, options.transcode_target_dir);
    assert_eq!(converted.remux_staging_root, options.remux_staging_root);
    assert_eq!(converted.remux_target_dir, options.remux_target_dir);
    assert_eq!(converted.audio_staging_root, options.audio_staging_root);
    assert_eq!(converted.audio_target_dir, options.audio_target_dir);
    // Non-path fields stay at workflow defaults: the facade carries paths only.
    let workflow_defaults = WorkflowExecutorOptions::default();
    assert_eq!(converted.max_attempts, workflow_defaults.max_attempts);
    assert_eq!(converted.lease_ttl, workflow_defaults.lease_ttl);
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

#[test]
fn compliance_ticket_operation_is_derived_from_workflow_ticket_kind() {
    assert_eq!(
        super::operation_from_ticket_kind("synthetic.workflow.operation.remux").unwrap(),
        "remux"
    );
    assert_eq!(
        super::operation_from_ticket_kind("synthetic.workflow.operation.transcode_video").unwrap(),
        "transcode_video"
    );
    assert!(super::operation_from_ticket_kind("synthetic.workflow.operation.unknown").is_err());
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
async fn compliance_execute_no_executable_work_creates_no_job() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_compliant(&cp).await;

    let data = cp
        .execute_compliance_policy(policy_version_id, input_set_id)
        .await
        .unwrap();

    assert_eq!(data.execution.submitted_node_count, 0);
    assert_eq!(data.execution.job_id, None);
    assert_eq!(count_rows(&cp, "jobs").await, 0);
}

#[tokio::test]
async fn compliance_execute_reports_issues_applied_when_workflow_submission_fails() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    register_policy_remux_worker(&cp).await;

    let err = cp
        .execute_compliance_policy_without_runtime_for_test(policy_version_id, input_set_id)
        .await
        .unwrap_err();

    let partial = err.partial.unwrap();
    assert_eq!(err.source.code(), "CONFIG_INVALID");
    assert_eq!(partial.issues.created_count, 1);
    assert_eq!(partial.execution.submitted_node_count, 1);
    assert_eq!(partial.execution.failure_count, 0);
    assert_eq!(count_rows(&cp, "issues").await, 1);
}

#[tokio::test]
async fn compliance_execute_reports_issues_applied_when_runtime_registry_fails() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    register_policy_remux_worker_with_extra(
        &cp,
        serde_json::json!({
            "endpoint": "not-a-socket",
            "secret": "policy-test-secret",
        }),
    )
    .await;

    let err = cp
        .execute_compliance_policy(policy_version_id, input_set_id)
        .await
        .unwrap_err();

    let partial = err.partial.unwrap();
    assert_eq!(err.source.code(), "CONFIG_INVALID");
    assert_eq!(partial.issues.created_count, 1);
    assert_eq!(partial.execution.submitted_node_count, 1);
    assert_eq!(partial.execution.job_id, None);
    assert_eq!(count_rows(&cp, "issues").await, 1);
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
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/custom/remux/output"
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
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/custom/audio/output"
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

#[tokio::test]
async fn execute_mutates_issues_issue_events_and_workflow_tables_only() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    let worker_id = register_policy_remux_worker(&cp).await;
    let runtimes = success_runtime_registry(worker_id);
    let before = boundary_counts(&cp).await;

    let err = cp
        .execute_compliance_policy_with_runtime_registry_for_test(
            policy_version_id,
            input_set_id,
            runtimes,
        )
        .await
        .unwrap_err();

    assert_synthetic_policy_remux_error(&err.source);
    let partial = err.partial.unwrap();
    assert_eq!(partial.issues.created_count, 1);
    assert_eq!(partial.execution.submitted_node_count, 1);
    assert_eq!(partial.tickets.len(), 0);

    let after = boundary_counts(&cp).await;
    assert!(after.count("issues") > before.count("issues"));
    assert!(after.count("events") > before.count("events"));
    assert!(after.count("jobs") > before.count("jobs"));
    assert_eq!(after.count("tickets"), before.count("tickets"));
    assert_eq!(after.count("leases"), before.count("leases"));
    assert_eq!(after.count("workers"), before.count("workers"));
    assert_eq!(
        after.count("worker_capabilities"),
        before.count("worker_capabilities")
    );
    assert_eq!(after.count("worker_grants"), before.count("worker_grants"));
    assert_eq!(
        after.count("artifact_handles"),
        before.count("artifact_handles")
    );
    assert_eq!(
        after.count("artifact_locations"),
        before.count("artifact_locations")
    );
    assert_eq!(
        after.count("artifact_lineage"),
        before.count("artifact_lineage")
    );
}

#[tokio::test]
async fn compliance_ticket_reporting_uses_kind_without_parsing_payload() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    let worker_id = register_policy_remux_worker(&cp).await;
    let runtimes = success_runtime_registry(worker_id);

    let err = cp
        .execute_compliance_policy_with_runtime_registry_for_test(
            policy_version_id,
            input_set_id,
            runtimes,
        )
        .await
        .unwrap_err();

    assert_synthetic_policy_remux_error(&err.source);
    let partial = err.partial.unwrap();
    let job_id = partial.execution.job_id.unwrap();
    assert_eq!(partial.tickets.len(), 0);

    let tickets = cp.compliance_executed_tickets(job_id).await.unwrap();
    assert_eq!(tickets.len(), 0);
}

#[tokio::test]
async fn compliance_execute_uses_production_workflow_lease_defaults() {
    let (cp, _tmp) = cp().await;
    let (policy_version_id, input_set_id, _document_id) = seed_noncompliant(&cp).await;
    let worker_id = register_policy_remux_worker(&cp).await;
    let runtimes = success_runtime_registry(worker_id);

    let err = cp
        .execute_compliance_policy_with_runtime_registry_for_test(
            policy_version_id,
            input_set_id,
            runtimes,
        )
        .await
        .unwrap_err();

    assert_synthetic_policy_remux_error(&err.source);
    assert_eq!(err.partial.unwrap().tickets.len(), 0);
    assert_eq!(count_rows(&cp, "leases").await, 0);
}

fn assert_synthetic_policy_remux_error(source: &voom_core::VoomError) {
    assert_eq!(source.code(), "CONFIG_INVALID");
    assert!(
        source
            .to_string()
            .contains("remux requires file_version or file_location target")
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

async fn register_policy_remux_worker_with_extra(
    cp: &crate::ControlPlane,
    extra: serde_json::Value,
) -> voom_core::WorkerId {
    register_policy_worker_with_extra(cp, OperationKind::Remux, "policy-test-remux", extra).await
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
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation_name.to_owned(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra,
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation_name.to_owned()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: serde_json::json!({ operation_name: 1 }),
    })
    .await
    .unwrap();
    worker.id
}

fn success_runtime_registry(worker_id: voom_core::WorkerId) -> WorkerRuntimeRegistry {
    WorkerRuntimeRegistry::new().with_in_process_runtime(
        worker_id,
        Arc::new(SuccessClient),
        WorkerCredentials {
            worker_id,
            worker_epoch: 0,
            secret: SecretString::from("policy-test-secret"),
        },
    )
}

async fn register_policy_audio_worker(
    cp: &crate::ControlPlane,
    operation: OperationKind,
) -> voom_core::WorkerId {
    register_policy_worker_with_extra(cp, operation, "policy-test-audio", serde_json::json!({}))
        .await
}

fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::Remux => "remux",
        OperationKind::TranscodeVideo => "transcode_video",
        OperationKind::TranscodeAudio => "transcode_audio",
        OperationKind::ExtractAudio => "extract_audio",
        _ => unreachable!("compliance tests only seed remux/transcode"),
    }
}

#[derive(Debug)]
struct SuccessClient;

#[async_trait::async_trait]
impl ClientHandle for SuccessClient {
    async fn handshake(
        &self,
        _offered: u32,
    ) -> Result<voom_worker_protocol::HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        let response = OperationResponse {
            lease_id: request.lease_id,
            accepted_at: chrono::Utc::now(),
        };
        let frame = ProgressFrame::Result {
            lease_id: request.lease_id,
            seq: 0,
            emitted_at: chrono::Utc::now(),
            payload: serde_json::json!({"status": "ok"}),
        };
        let body = serde_json::to_vec(&frame).map_err(|_| ProtocolError::InternalServerError)?;
        let (mut writer, reader) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let _ = writer.write_all(&body).await;
            let _ = writer.write_all(b"\n").await;
        });
        Ok(DispatchStream {
            response,
            frames: voom_worker_protocol::NdjsonReader::new(
                Pin::from(Box::new(reader) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
                request.lease_id,
            ),
        })
    }
}
