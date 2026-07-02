#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Inspection surfaces added by ADR 0031: `voom event|job|ticket` and
//! `voom scheduler leases`, plus the keyset `--after-id` / `next_cursor`
//! pagination convention. Goldens pin the newest-first (id DESC) ordering and
//! the end-of-stream cursor semantics.

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use time::{Duration, OffsetDateTime};
use voom_events::{Event, EventEnvelope, SubjectType, payload::SchemaInitializedPayload};
use voom_store::repo::events::{EventRepo, SqliteEventRepo};
use voom_store::repo::jobs::{NewJob, SqliteJobRepo};

mod inspection_envelope {
    use super::*;

    // ---- jobs -------------------------------------------------------------

    #[tokio::test]
    async fn job_list_is_newest_first_and_show_reads_one() {
        let fx = fixture().await;
        seed_jobs(&fx.pool, 3).await;

        let mut json = envelope(voom(&fx.url).args(["job", "list"]).output().unwrap().stdout);
        assert_eq!(json["command"], "job");
        let ids: Vec<u64> = json["data"]["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|job| job["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 2, 1], "jobs list newest first (id DESC)");
        // A short page (3 rows < default limit) is the end of the stream.
        assert!(json.get("next_cursor").is_none());
        redact_local(&mut json);
        insta::assert_json_snapshot!("job_list_newest_first", json);

        let show = envelope(
            voom(&fx.url)
                .args(["job", "show", "--job-id", "2"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(show["data"]["job"]["id"], 2);
    }

    #[tokio::test]
    async fn job_list_keyset_pages_deterministically() {
        let fx = fixture().await;
        seed_jobs(&fx.pool, 3).await;

        // Page 1: limit 2 fills the page, so a cursor is handed back.
        let page1 = envelope(
            voom(&fx.url)
                .args(["job", "list", "--limit", "2"])
                .output()
                .unwrap()
                .stdout,
        );
        let ids1: Vec<u64> = job_ids(&page1);
        assert_eq!(ids1, vec![3, 2]);
        assert_eq!(page1["next_cursor"].as_u64(), Some(2));

        // Page 2: continue after id 2 → the remaining older row, no cursor.
        let page2 = envelope(
            voom(&fx.url)
                .args(["job", "list", "--limit", "2", "--after-id", "2"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(job_ids(&page2), vec![1]);
        assert!(page2.get("next_cursor").is_none());
    }

    #[tokio::test]
    async fn job_show_unknown_id_is_not_found() {
        let fx = fixture().await;
        let output = voom(&fx.url)
            .args(["job", "show", "--job-id", "999"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let json = envelope(output.stdout);
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "NOT_FOUND");
    }

    // ---- tickets ----------------------------------------------------------

    #[tokio::test]
    async fn ticket_list_newest_first_and_state_filter() {
        let fx = fixture().await;
        seed_tickets(&fx.pool, &["ready", "leased", "ready"]).await;

        let mut json = envelope(
            voom(&fx.url)
                .args(["ticket", "list"])
                .output()
                .unwrap()
                .stdout,
        );
        let ids: Vec<u64> = json["data"]["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 2, 1]);
        redact_local(&mut json);
        insta::assert_json_snapshot!("ticket_list_newest_first", json);

        let ready = envelope(
            voom(&fx.url)
                .args(["ticket", "list", "--state", "ready"])
                .output()
                .unwrap()
                .stdout,
        );
        let ready_ids: Vec<u64> = ready["data"]["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ready_ids, vec![3, 1], "state filter composes with order");

        let show = envelope(
            voom(&fx.url)
                .args(["ticket", "show", "--ticket-id", "2"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(show["data"]["ticket"]["id"], 2);
        assert_eq!(show["data"]["ticket"]["state"], "leased");
        assert!(show["data"]["ticket"].get("payload").is_some());
    }

    // ---- events -----------------------------------------------------------

    #[tokio::test]
    async fn event_list_filters_by_time_window() {
        let fx = fixture().await;
        // `voom init` already appended a `schema.initialized` event (id 1) with
        // a real wall-clock timestamp; the seeds land at the 1970 epoch as ids
        // 2..=4. Bounding the window with `--until` excludes the init event so
        // the assertion is deterministic.
        seed_events(&fx.pool, 3).await;

        let all = envelope(
            voom(&fx.url)
                .args(["event", "list"])
                .output()
                .unwrap()
                .stdout,
        );
        let ids: Vec<u64> = event_ids(&all);
        // Newest first: every id strictly greater than the next.
        assert!(
            ids.windows(2).all(|w| w[0] > w[1]),
            "events id DESC: {ids:?}"
        );
        assert!(ids.contains(&2) && ids.contains(&3) && ids.contains(&4));

        // Window [epoch+15s, epoch+25s] captures only the third seed (epoch+20s).
        let since = rfc3339(15);
        let until = rfc3339(25);
        let mut windowed = envelope(
            voom(&fx.url)
                .args(["event", "list", "--since", &since, "--until", &until])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(event_ids(&windowed), vec![4]);
        assert_eq!(windowed["data"]["events"][0]["kind"], "schema.initialized");
        redact_local(&mut windowed);
        insta::assert_json_snapshot!("event_list_time_window", windowed);
    }

    #[tokio::test]
    async fn event_list_rejects_bad_timestamp_as_bad_args() {
        let fx = fixture().await;
        let output = voom(&fx.url)
            .args(["event", "list", "--since", "not-a-time"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "BAD_ARGS");
    }

    #[tokio::test]
    async fn event_show_reads_one_by_id() {
        let fx = fixture().await;
        seed_events(&fx.pool, 2).await;
        let json = envelope(
            voom(&fx.url)
                .args(["event", "show", "--event-id", "2"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(json["data"]["event"]["id"], 2);
        assert_eq!(json["data"]["event"]["kind"], "schema.initialized");
        assert!(json["data"]["event"].get("payload").is_some());
    }

    // ---- scheduler leases -------------------------------------------------

    #[tokio::test]
    async fn scheduler_leases_list_and_show() {
        let fx = fixture().await;
        seed_leases(&fx.pool, 2).await;

        let mut json = envelope(
            voom(&fx.url)
                .args(["scheduler", "leases", "list"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(json["command"], "scheduler");
        let ids: Vec<u64> = json["data"]["leases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![2, 1], "leases newest first");
        redact_local(&mut json);
        insta::assert_json_snapshot!("scheduler_leases_list", json);

        let held = envelope(
            voom(&fx.url)
                .args(["scheduler", "leases", "list", "--state", "held"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(held["data"]["leases"].as_array().unwrap().len(), 2);

        let show = envelope(
            voom(&fx.url)
                .args(["scheduler", "leases", "show", "--lease-id", "1"])
                .output()
                .unwrap()
                .stdout,
        );
        assert_eq!(show["data"]["lease"]["id"], 1);
    }

    // ---- fixture ----------------------------------------------------------

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
        pool: sqlx::SqlitePool,
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

    fn voom(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url]);
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

    fn job_ids(json: &Value) -> Vec<u64> {
        json["data"]["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|job| job["id"].as_u64().unwrap())
            .collect()
    }

    fn event_ids(json: &Value) -> Vec<u64> {
        json["data"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_u64().unwrap())
            .collect()
    }

    fn rfc3339(offset_secs: i64) -> String {
        (OffsetDateTime::UNIX_EPOCH + Duration::seconds(offset_secs))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    async fn seed_jobs(pool: &sqlx::SqlitePool, count: u32) {
        let repo = SqliteJobRepo::new(pool.clone());
        for i in 0..count {
            repo.create(NewJob {
                kind: "probe_file".to_owned(),
                priority: i64::from(i),
                created_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(i64::from(i)),
            })
            .await
            .unwrap();
        }
    }

    async fn seed_tickets(pool: &sqlx::SqlitePool, states: &[&str]) {
        for (i, state) in states.iter().enumerate() {
            let id = i64::try_from(i).unwrap() + 1;
            sqlx::query(
                "INSERT INTO tickets \
                 (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
                  next_eligible_at, created_at, state_changed_at) \
                 VALUES (?, NULL, 'probe_file', ?, 0, '{}', 0, 3, \
                         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                         '1970-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(*state)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn seed_events(pool: &sqlx::SqlitePool, count: u32) {
        let repo = SqliteEventRepo::new(pool.clone());
        for i in 0..count {
            let at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(i64::from(i) * 10);
            let mut tx = pool.begin().await.unwrap();
            repo.append_in_tx(
                &mut tx,
                EventEnvelope {
                    occurred_at: at,
                    subject_type: SubjectType::System,
                    subject_id: None,
                    trace_id: None,
                    payload: Event::SchemaInitialized(SchemaInitializedPayload {
                        migrations_applied: 1,
                        schema_init_at: at,
                    }),
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
    }

    async fn seed_leases(pool: &sqlx::SqlitePool, count: u32) {
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
             VALUES (1, 'worker-1', 'remote', 'active', 1, \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        for i in 1..=count {
            sqlx::query(
                "INSERT INTO tickets \
                 (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
                  next_eligible_at, created_at, state_changed_at) \
                 VALUES (?, NULL, 'probe_file', 'leased', 0, '{}', 0, 3, \
                         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                         '1970-01-01T00:00:00Z')",
            )
            .bind(i64::from(i))
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO leases \
                 (id, ticket_id, worker_id, state, acquired_at, expires_at, \
                  last_heartbeat_at, ttl_seconds, release_reason, released_at, epoch) \
                 VALUES (?, ?, 1, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:05:00Z', \
                         '1970-01-01T00:00:00Z', 300, NULL, NULL, 0)",
            )
            .bind(i64::from(i))
            .bind(i64::from(i))
            .execute(pool)
            .await
            .unwrap();
        }
    }
}
