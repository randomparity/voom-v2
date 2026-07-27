#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::{Command, Output};

use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::OffsetDateTime;
use voom_control_plane::ControlPlane;
use voom_core::{JobId, TicketId, TicketOperation};
use voom_store::repo::jobs::{JobState, NewJob};
use voom_store::repo::tickets::NewTicket;

const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

struct Fixture {
    _tmp: NamedTempFile,
    url: String,
    pool: SqlitePool,
}

#[derive(Debug, PartialEq, Eq)]
struct DurableState {
    job_state: Option<String>,
    ticket: Option<(String, i64, i64)>,
    job_count: i64,
    ticket_count: i64,
    lease_count: i64,
    event_count: i64,
    cancelled_event_count: i64,
}

async fn fixture() -> Fixture {
    let tmp = NamedTempFile::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    Fixture {
        _tmp: tmp,
        url,
        pool,
    }
}

async fn open_job_with_ready_ticket(fixture: &Fixture) -> (JobId, TicketId) {
    let cp = ControlPlane::open(&fixture.url).await.unwrap();
    let job = cp
        .open_job(NewJob {
            kind: "policy_execute".to_owned(),
            priority: 0,
            created_at: NOW,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: TicketOperation::new("noop").unwrap(),
            priority: 0,
            payload: serde_json::json!({}),
            max_attempts: 2,
            created_at: NOW,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, NOW).await.unwrap();
    (job.id, ticket.id)
}

fn job_cli(url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args(["--database-url", url, "job"]);
    command
}

fn envelope(output: &Output) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}

async fn durable_state(
    fixture: &Fixture,
    job_id: Option<JobId>,
    ticket_id: Option<TicketId>,
) -> DurableState {
    let job_state = match job_id {
        Some(id) => sqlx::query_scalar("SELECT state FROM jobs WHERE id = ?")
            .bind(i64::try_from(id.0).unwrap())
            .fetch_optional(&fixture.pool)
            .await
            .unwrap(),
        None => None,
    };
    let ticket = match ticket_id {
        Some(id) => sqlx::query_as("SELECT state, attempt, epoch FROM tickets WHERE id = ?")
            .bind(i64::try_from(id.0).unwrap())
            .fetch_optional(&fixture.pool)
            .await
            .unwrap(),
        None => None,
    };
    DurableState {
        job_state,
        ticket,
        job_count: count_rows(&fixture.pool, "jobs").await,
        ticket_count: count_rows(&fixture.pool, "tickets").await,
        lease_count: count_rows(&fixture.pool, "leases").await,
        event_count: count_rows(&fixture.pool, "events").await,
        cancelled_event_count: sqlx::query_scalar(
            "SELECT COUNT(*) FROM events WHERE kind = 'job.cancelled'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
    }
}

async fn count_rows(pool: &SqlitePool, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar(&query).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn cancel_open_job_emits_one_envelope_and_persists_audited_state() {
    let fixture = fixture().await;
    let (job_id, ticket_id) = open_job_with_ready_ticket(&fixture).await;
    let before = durable_state(&fixture, Some(job_id), Some(ticket_id)).await;

    let output = job_cli(&fixture.url)
        .args([
            "cancel",
            "--job-id",
            &job_id.0.to_string(),
            "--reason",
            "operator requested stop",
        ])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = envelope(&output);
    assert_eq!(json["command"], "job");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["job"]["id"], job_id.0);
    assert_eq!(json["data"]["job"]["state"], "cancelled");
    assert_eq!(json["data"]["job"]["epoch"], 1);
    let after = durable_state(&fixture, Some(job_id), Some(ticket_id)).await;
    assert_eq!(after.job_state.as_deref(), Some("cancelled"));
    assert_eq!(after.ticket, before.ticket);
    assert_eq!(after.job_count, before.job_count);
    assert_eq!(after.ticket_count, before.ticket_count);
    assert_eq!(after.lease_count, before.lease_count);
    assert_eq!(after.event_count, before.event_count + 1);
    assert_eq!(
        after.cancelled_event_count,
        before.cancelled_event_count + 1
    );
    let event: (i64, i64, String) = sqlx::query_as(
        "SELECT subject_id, json_extract(payload, '$.job_id'), \
                json_extract(payload, '$.reason') FROM events \
         WHERE kind = 'job.cancelled' ORDER BY event_id DESC LIMIT 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(event.0, i64::try_from(job_id.0).unwrap());
    assert_eq!(event.1, i64::try_from(job_id.0).unwrap());
    assert_eq!(event.2, "operator requested stop");
}

#[tokio::test]
async fn cancel_missing_job_is_not_found_without_durable_changes() {
    let fixture = fixture().await;
    let before = durable_state(&fixture, None, None).await;

    let output = job_cli(&fixture.url)
        .args([
            "cancel",
            "--job-id",
            "999",
            "--reason",
            "operator requested stop",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(envelope(&output)["error"]["code"], "NOT_FOUND");
    assert_eq!(durable_state(&fixture, None, None).await, before);
}

#[tokio::test]
async fn cancel_terminal_jobs_is_conflict_without_durable_changes() {
    for terminal in [JobState::Succeeded, JobState::Failed, JobState::Cancelled] {
        assert_terminal_rejected(terminal).await;
    }
}

async fn assert_terminal_rejected(terminal: JobState) {
    let fixture = fixture().await;
    let (job_id, ticket_id) = open_job_with_ready_ticket(&fixture).await;
    let cp = ControlPlane::open(&fixture.url).await.unwrap();
    match terminal {
        JobState::Succeeded => {
            cp.succeed_job(job_id, NOW).await.unwrap();
        }
        JobState::Failed => {
            cp.fail_job(job_id, "fixture failure".to_owned(), NOW)
                .await
                .unwrap();
        }
        JobState::Cancelled => {
            cp.cancel_job(job_id, "fixture cancellation".to_owned(), NOW)
                .await
                .unwrap();
        }
        JobState::Open => panic!("terminal fixture must not be open"),
    }
    let before = durable_state(&fixture, Some(job_id), Some(ticket_id)).await;

    let output = job_cli(&fixture.url)
        .args([
            "cancel",
            "--job-id",
            &job_id.0.to_string(),
            "--reason",
            "second cancellation",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(envelope(&output)["error"]["code"], "CONFLICT");
    assert_eq!(
        durable_state(&fixture, Some(job_id), Some(ticket_id)).await,
        before
    );
}

#[tokio::test]
async fn cancel_blank_reason_is_config_invalid_without_durable_changes() {
    let fixture = fixture().await;
    let (job_id, ticket_id) = open_job_with_ready_ticket(&fixture).await;
    let before = durable_state(&fixture, Some(job_id), Some(ticket_id)).await;

    let output = job_cli(&fixture.url)
        .args([
            "cancel",
            "--job-id",
            &job_id.0.to_string(),
            "--reason",
            "   ",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(envelope(&output)["error"]["code"], "CONFIG_INVALID");
    assert_eq!(
        durable_state(&fixture, Some(job_id), Some(ticket_id)).await,
        before
    );
}

#[tokio::test]
async fn cancel_requires_job_id_and_reason_as_bad_args() {
    let fixture = fixture().await;
    let before = durable_state(&fixture, None, None).await;

    for args in [
        vec!["cancel", "--reason", "operator requested stop"],
        vec!["cancel", "--job-id", "1"],
    ] {
        let output = job_cli(&fixture.url).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(envelope(&output)["error"]["code"], "BAD_ARGS");
    }

    assert_eq!(durable_state(&fixture, None, None).await, before);
}
