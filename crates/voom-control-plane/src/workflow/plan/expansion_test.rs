use std::sync::Arc;

use serde_json::Value;
use time::OffsetDateTime;
use voom_core::OperationKind;
use voom_core::{JobId, SystemClock, TicketId};
use voom_store::repo::jobs::NewJob;
use voom_store::repo::tickets::{NewTicket, Ticket, TicketRepo};

use crate::ControlPlane;
use crate::workflow::execution::timing::EffectiveTiming;
use crate::workflow::plan::binding::{
    BranchContext, branch_context_with_probe_codec, render_default_payload,
    render_default_payload_with_fan_out,
};
use crate::workflow::plan::expansion::{
    ExpansionContext, expand_backup_completion, expand_probe_completion, expand_quality_completion,
    expand_scanner_completion, expand_transform_completion,
};
use crate::workflow::plan::model::WorkflowPlan;
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn scanner_completion_creates_only_probe_hash_identity() {
    let fixture = WorkflowExpansionFixture::new().await;
    let scanner = fixture
        .seed_succeeded_ticket(
            "scan",
            "root",
            OperationKind::ScanLibrary,
            scanner_result_with_three_files(),
        )
        .await;

    let first = expand_scanner_completion(&fixture.ctx(), &scanner)
        .await
        .unwrap();
    let second = expand_scanner_completion(&fixture.ctx(), &scanner)
        .await
        .unwrap();

    assert_eq!(first.len(), 9);
    assert!(second.is_empty());
    assert!(first.iter().all(|ticket| {
        let payload = parse_workflow_payload(ticket);
        ["probe", "hash", "identity"].contains(&payload.node_id.as_str())
    }));
    let probe_000 = fixture.ticket("probe", "file-000").await;
    let probe_001 = fixture.ticket("probe", "file-001").await;
    let probe_002 = fixture.ticket("probe", "file-002").await;
    assert_eq!(probe_000.rendered_payload["codec"], "h265");
    assert_eq!(probe_001.rendered_payload["codec"], "h264");
    assert_eq!(probe_002.rendered_payload["codec"], "h265");
    assert_eq!(
        probe_000.source_file.as_ref().unwrap()["size_bytes"],
        serde_json::json!(4_200_000_000_u64)
    );
    assert!((25..=35).contains(&probe_001.timing.duration_ms));
    assert_ne!(probe_000.timing, probe_001.timing);
    fixture.assert_no_ticket("quality", "file-000").await;
    fixture.assert_ticket_count(10).await;
    fixture.assert_dependency_count(9).await;
}

#[tokio::test]
async fn probe_completion_creates_quality_after_probe_output_provides_codec() {
    let fixture = WorkflowExpansionFixture::new().await;
    let probe = fixture
        .seed_succeeded_ticket(
            "probe",
            "file-001",
            OperationKind::ProbeFile,
            serde_json::json!({"codec": "h264"}),
        )
        .await;

    let first = expand_probe_completion(&fixture.ctx(), "file-001", &probe)
        .await
        .unwrap();
    let second = expand_probe_completion(&fixture.ctx(), "file-001", &probe)
        .await
        .unwrap();

    assert_eq!(first.len(), 1);
    assert!(second.is_empty());
    let quality = parse_workflow_payload(&first[0]);
    assert_eq!(quality.node_id, "quality");
    assert_eq!(quality.branch_id, "file-001");
    assert_eq!(quality.rendered_payload["codec"], "h264");
    fixture.assert_ticket_count(2).await;
    fixture.assert_dependency_count(1).await;
}

#[tokio::test]
async fn probe_completion_requires_codec_result() {
    let fixture = WorkflowExpansionFixture::new().await;
    let probe = fixture
        .seed_succeeded_ticket(
            "probe",
            "file-001",
            OperationKind::ProbeFile,
            serde_json::json!({"hash": "sha256:missing-codec"}),
        )
        .await;

    let err = expand_probe_completion(&fixture.ctx(), "file-001", &probe)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("result field `codec`"));
    fixture.assert_no_ticket("quality", "file-001").await;
}

#[tokio::test]
async fn quality_completion_creates_exactly_selected_transform_based_on_needs_transcode() {
    let fixture = WorkflowExpansionFixture::new().await;
    let needs_transcode = fixture
        .seed_succeeded_ticket(
            "quality",
            "file-000",
            OperationKind::ScoreQuality,
            serde_json::json!({"needs_transcode": true}),
        )
        .await;
    let does_not_need_transcode = fixture
        .seed_succeeded_ticket(
            "quality",
            "file-001",
            OperationKind::ScoreQuality,
            serde_json::json!({"needs_transcode": false}),
        )
        .await;

    let transcode_first = expand_quality_completion(&fixture.ctx(), "file-000", &needs_transcode)
        .await
        .unwrap();
    let transcode_second = expand_quality_completion(&fixture.ctx(), "file-000", &needs_transcode)
        .await
        .unwrap();
    let remux_first =
        expand_quality_completion(&fixture.ctx(), "file-001", &does_not_need_transcode)
            .await
            .unwrap();
    let remux_second =
        expand_quality_completion(&fixture.ctx(), "file-001", &does_not_need_transcode)
            .await
            .unwrap();

    assert_eq!(node_ids(&transcode_first), vec!["transcode"]);
    assert!(transcode_second.is_empty());
    assert_eq!(node_ids(&remux_first), vec!["remux"]);
    assert!(remux_second.is_empty());
    fixture.assert_no_ticket("remux", "file-000").await;
    fixture.assert_no_ticket("transcode", "file-001").await;
    fixture.assert_ticket_count(4).await;
    fixture.assert_dependency_count(2).await;
}

#[tokio::test]
async fn transform_completion_creates_downstream_work_but_not_verify() {
    let fixture = WorkflowExpansionFixture::new().await;
    let transform = fixture
        .seed_succeeded_ticket(
            "transcode",
            "file-001",
            OperationKind::TranscodeVideo,
            serde_json::json!({"output_path": "/staging/file-001.h265.mkv"}),
        )
        .await;

    let first = expand_transform_completion(&fixture.ctx(), "file-001", &transform)
        .await
        .unwrap();
    let second = expand_transform_completion(&fixture.ctx(), "file-001", &transform)
        .await
        .unwrap();

    assert_eq!(
        node_ids(&first),
        vec!["backup", "external-sync", "issue", "use-lease"]
    );
    assert!(second.is_empty());
    for ticket in &first {
        let payload = parse_workflow_payload(ticket);
        assert_eq!(
            payload.rendered_payload["path"],
            "/staging/file-001.h265.mkv"
        );
    }
    let issue = fixture.ticket("issue", "file-001").await;
    assert_eq!(issue.rendered_payload["reason"], "quality_regression");
    let external_sync = fixture.ticket("external-sync", "file-001").await;
    assert_eq!(external_sync.rendered_payload["system"], "plex");
    assert_eq!(external_sync.rendered_payload["action"], "refresh");
    let use_lease = fixture.ticket("use-lease", "file-001").await;
    assert_eq!(use_lease.rendered_payload["holder"], "manual");
    assert_eq!(use_lease.rendered_payload["reason"], "playback");
    fixture.assert_no_ticket("verify", "file-001").await;
    fixture.assert_ticket_count(5).await;
    fixture.assert_dependency_count(4).await;
}

#[tokio::test]
async fn backup_completion_creates_verify_after_local_backup_id_exists() {
    let fixture = WorkflowExpansionFixture::new().await;
    let backup = fixture
        .seed_succeeded_ticket(
            "backup",
            "file-001",
            OperationKind::BackUpFile,
            serde_json::json!({"local_backup_id": "backup-local-001"}),
        )
        .await;

    let first = expand_backup_completion(&fixture.ctx(), "file-001", &backup)
        .await
        .unwrap();
    let second = expand_backup_completion(&fixture.ctx(), "file-001", &backup)
        .await
        .unwrap();

    assert_eq!(node_ids(&first), vec!["verify"]);
    assert!(second.is_empty());
    let verify = fixture.ticket("verify", "file-001").await;
    assert_eq!(verify.rendered_payload["path"], "backup-local-001");
    fixture.assert_ticket_count(2).await;
    fixture.assert_dependency_count(1).await;
}

#[tokio::test]
async fn expansion_promotes_existing_pending_ticket_after_restart() {
    let fixture = WorkflowExpansionFixture::new().await;
    let probe = fixture
        .seed_succeeded_ticket(
            "probe",
            "file-001",
            OperationKind::ProbeFile,
            serde_json::json!({"codec": "h264"}),
        )
        .await;
    let quality = fixture
        .seed_pending_ticket("quality", "file-001", OperationKind::ScoreQuality)
        .await;
    fixture.add_dependency(quality.id, probe.id).await;

    let created = expand_probe_completion(&fixture.ctx(), "file-001", &probe)
        .await
        .unwrap();

    assert!(created.is_empty());
    let quality = fixture.find_ticket("quality", "file-001").await.unwrap();
    assert_eq!(quality.state, voom_store::repo::tickets::TicketState::Ready);
    fixture.assert_ticket_count(2).await;
    fixture.assert_dependency_count(1).await;
}

#[tokio::test]
async fn expansion_rejects_pre_existing_duplicate_workflow_tickets() {
    let fixture = WorkflowExpansionFixture::new().await;
    let probe = fixture
        .seed_succeeded_ticket(
            "probe",
            "file-001",
            OperationKind::ProbeFile,
            serde_json::json!({"codec": "h264"}),
        )
        .await;
    fixture
        .seed_pending_ticket("quality", "file-001", OperationKind::ScoreQuality)
        .await;
    fixture
        .seed_pending_ticket("quality", "file-001", OperationKind::ScoreQuality)
        .await;

    let err = expand_probe_completion(&fixture.ctx(), "file-001", &probe)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("duplicate workflow tickets"));
}

#[tokio::test]
async fn scanner_completion_dedupes_duplicate_file_outputs() {
    let fixture = WorkflowExpansionFixture::new().await;
    let scanner = fixture
        .seed_succeeded_ticket(
            "scan",
            "root",
            OperationKind::ScanLibrary,
            serde_json::json!({
                "files": [
                    "/library/file-000.mkv",
                    "/library/file-000.mkv",
                    {"path": "/library/file-001.mkv"}
                ]
            }),
        )
        .await;

    let created = expand_scanner_completion(&fixture.ctx(), &scanner)
        .await
        .unwrap();

    assert_eq!(created.len(), 6);
    fixture.assert_ticket_count(7).await;
    fixture.assert_dependency_count(6).await;
}

#[tokio::test]
async fn scanner_completion_rejects_branch_id_path_collisions() {
    let fixture = WorkflowExpansionFixture::new().await;
    let scanner = fixture
        .seed_succeeded_ticket(
            "scan",
            "root",
            OperationKind::ScanLibrary,
            serde_json::json!({
                "files": [
                    "/library/file-000.mkv",
                    "/other/file-000.mp4"
                ]
            }),
        )
        .await;

    let err = expand_scanner_completion(&fixture.ctx(), &scanner)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("both derive branch id"));
}

struct WorkflowExpansionFixture {
    cp: ControlPlane,
    _tmp: tempfile::NamedTempFile,
    plan: WorkflowPlan,
    workflow_id: String,
    plan_id: String,
    job_id: JobId,
    now: OffsetDateTime,
}

impl WorkflowExpansionFixture {
    async fn new() -> Self {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        let _ = voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        let cp = ControlPlane::open_with_pool(pool, Arc::new(SystemClock))
            .await
            .unwrap();
        let now = T0;
        let job = cp
            .open_job(NewJob {
                kind: "synthetic.workflow".to_owned(),
                priority: 0,
                created_at: now,
            })
            .await
            .unwrap();
        let plan = WorkflowPlan::default_ci();
        Self {
            cp,
            _tmp: tmp,
            plan_id: plan.id.clone(),
            plan,
            workflow_id: "workflow-test".to_owned(),
            job_id: job.id,
            now,
        }
    }

    fn ctx(&self) -> ExpansionContext<'_> {
        ExpansionContext::new(
            &self.cp,
            &self.plan,
            &self.workflow_id,
            &self.plan_id,
            self.job_id,
            self.now,
        )
    }

    async fn seed_succeeded_ticket(
        &self,
        node_id: &str,
        branch_id: &str,
        operation: OperationKind,
        result: Value,
    ) -> Ticket {
        let branch = branch_for_seed(branch_id, operation);
        let rendered_payload = if operation == OperationKind::ScanLibrary {
            render_default_payload_with_fan_out(operation, &branch, timing(), 3).unwrap()
        } else {
            render_default_payload(operation, &branch, timing()).unwrap()
        };
        let payload = WorkflowTicketPayload {
            workflow_id: self.workflow_id.clone(),
            plan_id: self.plan_id.clone(),
            node_id: node_id.to_owned(),
            branch_id: branch_id.to_owned(),
            operation,
            rendered_payload,
            timing: timing(),
            source_file: Some(serde_json::json!({
                "path": format!("/library/{branch_id}.mkv"),
                "size_bytes": 4_200_000_000_u64,
            })),
        }
        .to_ticket_payload()
        .unwrap();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(self.job_id),
                kind: ticket_kind(operation),
                priority: 0,
                payload,
                max_attempts: 1,
                created_at: self.now,
            })
            .await
            .unwrap();
        set_ticket_succeeded(&self.cp, ticket.id, result, self.now).await;
        self.cp.tickets().get(ticket.id).await.unwrap().unwrap()
    }

    async fn seed_pending_ticket(
        &self,
        node_id: &str,
        branch_id: &str,
        operation: OperationKind,
    ) -> Ticket {
        let branch = branch_for_seed(branch_id, operation);
        let rendered_payload = render_default_payload(operation, &branch, timing()).unwrap();
        let payload = WorkflowTicketPayload {
            workflow_id: self.workflow_id.clone(),
            plan_id: self.plan_id.clone(),
            node_id: node_id.to_owned(),
            branch_id: branch_id.to_owned(),
            operation,
            rendered_payload,
            timing: timing(),
            source_file: Some(serde_json::json!({
                "path": format!("/library/{branch_id}.mkv"),
                "size_bytes": 4_200_000_000_u64,
            })),
        }
        .to_ticket_payload()
        .unwrap();
        self.cp
            .create_ticket(NewTicket {
                job_id: Some(self.job_id),
                kind: ticket_kind(operation),
                priority: 0,
                payload,
                max_attempts: 1,
                created_at: self.now,
            })
            .await
            .unwrap()
    }

    async fn add_dependency(&self, ticket_id: TicketId, depends_on: TicketId) {
        self.cp
            .tickets()
            .add_dependency(ticket_id, depends_on)
            .await
            .unwrap();
    }

    async fn find_ticket(&self, node_id: &str, branch_id: &str) -> Option<Ticket> {
        find_ticket(&self.cp, self.job_id, node_id, branch_id).await
    }

    async fn ticket(&self, node_id: &str, branch_id: &str) -> WorkflowTicketPayload {
        let ticket = find_ticket(&self.cp, self.job_id, node_id, branch_id)
            .await
            .unwrap_or_else(|| panic!("missing ticket {node_id}/{branch_id}"));
        parse_workflow_payload(&ticket)
    }

    async fn assert_no_ticket(&self, node_id: &str, branch_id: &str) {
        assert!(
            find_ticket(&self.cp, self.job_id, node_id, branch_id)
                .await
                .is_none(),
            "unexpected ticket {node_id}/{branch_id}"
        );
    }

    async fn assert_ticket_count(&self, expected: i64) {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ?")
            .bind(i64::try_from(self.job_id.0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap();
        assert_eq!(count, expected);
    }

    async fn assert_dependency_count(&self, expected: i64) {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ticket_dependencies td \
             JOIN tickets t ON t.id = td.ticket_id \
             WHERE t.job_id = ?",
        )
        .bind(i64::try_from(self.job_id.0).unwrap())
        .fetch_one(&self.cp.pool)
        .await
        .unwrap();
        assert_eq!(count, expected);
    }
}

async fn set_ticket_succeeded(
    cp: &ControlPlane,
    ticket_id: TicketId,
    result: Value,
    now: OffsetDateTime,
) {
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ?, \
         epoch = epoch + 1 WHERE id = ?",
    )
    .bind(serde_json::to_string(&result).unwrap())
    .bind(format_time(now))
    .bind(i64::try_from(ticket_id.0).unwrap())
    .execute(&cp.pool)
    .await
    .unwrap();
}

async fn find_ticket(
    cp: &ControlPlane,
    job_id: JobId,
    node_id: &str,
    branch_id: &str,
) -> Option<Ticket> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM tickets \
         WHERE job_id = ? \
           AND json_extract(payload, '$.node_id') = ? \
           AND json_extract(payload, '$.branch_id') = ? \
         LIMIT 1",
    )
    .bind(i64::try_from(job_id.0).unwrap())
    .bind(node_id)
    .bind(branch_id)
    .fetch_optional(&cp.pool)
    .await
    .unwrap();
    let (id,) = row?;
    cp.tickets()
        .get(TicketId(u64::try_from(id).unwrap()))
        .await
        .unwrap()
}

fn parse_workflow_payload(ticket: &Ticket) -> WorkflowTicketPayload {
    WorkflowTicketPayload::parse_ticket(&ticket.kind, ticket.payload.clone()).unwrap()
}

fn node_ids(tickets: &[Ticket]) -> Vec<String> {
    tickets
        .iter()
        .map(parse_workflow_payload)
        .map(|payload| payload.node_id)
        .collect()
}

fn scanner_result_with_three_files() -> Value {
    serde_json::json!({
        "files": [
            {"path": "/library/file-000.mkv", "size_bytes": 4_200_000_000_u64},
            {"path": "/library/file-001.mkv", "size_bytes": 4_200_000_001_u64},
            {"path": "/library/file-002.mkv", "size_bytes": 4_200_000_002_u64}
        ]
    })
}

fn branch_for_seed(branch_id: &str, operation: OperationKind) -> BranchContext {
    if operation == OperationKind::ScoreQuality {
        branch_context_with_probe_codec(branch_id, "h264")
    } else {
        BranchContext {
            branch_id: branch_id.to_owned(),
            path: format!("/library/{branch_id}.mkv"),
            probe_codec: Some("h264".to_owned()),
            source_file: None,
        }
    }
}

fn ticket_kind(operation: OperationKind) -> String {
    format!(
        "synthetic.workflow.operation.{}",
        serde_json::to_value(operation).unwrap().as_str().unwrap()
    )
}

fn timing() -> EffectiveTiming {
    EffectiveTiming::for_test(25, 10)
}

fn format_time(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap()
}
