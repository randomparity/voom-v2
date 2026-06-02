#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use time::OffsetDateTime;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome, SchedulerReasonCode,
    SchedulerRequestSource, SqliteSchedulerDecisionRepo,
};

mod scheduler_envelope {
    use super::*;

    #[tokio::test]
    async fn scheduler_decisions_list_outputs_envelope() {
        let fixture = fixture().await;

        let output = scheduler_command(&fixture.url)
            .args(["decisions", "list", "--worker-id", "2"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "scheduler");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("scheduler_decisions_list_outputs_envelope", json);
    }

    #[tokio::test]
    async fn scheduler_decisions_show_outputs_full_explanation() {
        let fixture = fixture().await;

        let output = scheduler_command(&fixture.url)
            .args([
                "decisions",
                "show",
                "--decision-id",
                fixture.decision_id.to_string().as_str(),
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "scheduler");
        assert_eq!(json["status"], "ok");
        assert!(json["data"]["decision"].get("explanation_json").is_some());
        redact_local(&mut json);
        insta::assert_json_snapshot!("scheduler_decisions_show_outputs_full_explanation", json);
    }

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
        decision_id: u64,
    }

    async fn fixture() -> Fixture {
        let tmp = NamedTempFile::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        seed_refs(&pool).await;
        let repo = SqliteSchedulerDecisionRepo::new(pool);
        let decision = repo
            .create(NewSchedulerDecision {
                decision_kind: SchedulerDecisionKind::LeaseAcquire,
                request_source: SchedulerRequestSource::RemoteAcquire,
                idempotency_key: Some("idem".to_owned()),
                request_node_id: Some(NodeId(1)),
                request_worker_id: Some(WorkerId(2)),
                ticket_id: Some(TicketId(3)),
                selected_worker_id: Some(WorkerId(2)),
                selected_node_id: Some(NodeId(1)),
                selected_lease_id: None,
                outcome: SchedulerDecisionOutcome::Selected,
                reason_code: SchedulerReasonCode::Selected,
                summary: "selected worker 2 for ticket 3".to_owned(),
                candidate_count: 1,
                selected_score: Some(1700),
                suppression_key: None,
                explanation: json!({"scoring_version":1,"candidates":[]}),
                now: OffsetDateTime::UNIX_EPOCH,
            })
            .await
            .unwrap();

        Fixture {
            _tmp: tmp,
            url,
            decision_id: decision.id,
        }
    }

    fn scheduler_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "scheduler"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn redact_local(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    }

    async fn seed_refs(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "INSERT INTO nodes \
             (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
              auth_token_hash, auth_token_hint, metadata) \
             VALUES (1, 'node-1', 'remote', 'active', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
             VALUES (2, 'worker-2', 'remote', 'active', 1, \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tickets \
             (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
              next_eligible_at, created_at, state_changed_at) \
             VALUES (3, NULL, 'probe_file', 'ready', 0, '{}', 0, 3, \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
    }
}
