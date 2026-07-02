#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;

mod issue_envelope {
    use super::*;

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
    }

    async fn fixture() -> Fixture {
        let tmp = NamedTempFile::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        Fixture { _tmp: tmp, url }
    }

    /// Insert one issue row directly. Issues have no `create` command — they are
    /// opened by the compliance and terminal-failure paths — so tests seed the
    /// table to exercise the read/transition surface in isolation.
    async fn seed_issue(url: &str, kind: &str, severity: &str, priority: &str, status: &str) {
        let pool = voom_store::connect(url).await.unwrap();
        sqlx::query(
            "INSERT INTO issues \
             (kind, severity, priority, priority_source, priority_reason, status, \
              title, body, created_at, updated_at) \
             VALUES (?, ?, ?, 'system', 'seed reason', ?, \
                     'Seed issue title', 'Seed issue body', \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(kind)
        .bind(severity)
        .bind(priority)
        .bind(status)
        .execute(&pool)
        .await
        .unwrap();
    }

    fn cli(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "issue"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    /// Replace clock-driven fields with placeholders so snapshots are stable.
    /// `created_at` is seeded to a fixed value and left intact; the transition
    /// timestamps (`updated_at`, `resolved_at`, `suppressed_until`) are stamped.
    fn redact(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
        if let Some(issues) = json["data"].get_mut("issues").and_then(Value::as_array_mut) {
            for issue in issues {
                stamp(issue);
            }
        } else {
            stamp(&mut json["data"]);
        }
    }

    fn stamp(issue: &mut Value) {
        for field in ["updated_at", "resolved_at", "suppressed_until"] {
            if issue.get(field).is_some_and(|value| !value.is_null()) {
                issue[field] = Value::String("[ts]".to_owned());
            }
        }
    }

    #[tokio::test]
    async fn list_outputs_records() {
        let fixture = fixture().await;
        seed_issue(
            &fixture.url,
            "policy_noncompliant",
            "medium",
            "normal",
            "open",
        )
        .await;
        seed_issue(
            &fixture.url,
            "terminal_failure",
            "high",
            "urgent",
            "planned",
        )
        .await;

        let output = cli(&fixture.url).arg("list").output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "issue");
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("list_outputs_records", json);
    }

    #[tokio::test]
    async fn list_paginates_with_limit_and_after_id() {
        let fixture = fixture().await;
        seed_issue(
            &fixture.url,
            "policy_noncompliant",
            "medium",
            "normal",
            "open",
        )
        .await;
        seed_issue(
            &fixture.url,
            "terminal_failure",
            "high",
            "urgent",
            "planned",
        )
        .await;
        seed_issue(&fixture.url, "policy_noncompliant", "low", "low", "open").await;

        let first = cli(&fixture.url)
            .args(["list", "--limit", "2"])
            .output()
            .unwrap();
        assert_eq!(first.status.code(), Some(0));
        let mut json = envelope(first.stdout);
        assert_eq!(json["data"]["next_cursor"], 2);
        redact(&mut json);
        insta::assert_json_snapshot!("list_first_page", json);

        let second = cli(&fixture.url)
            .args(["list", "--limit", "2", "--after-id", "2"])
            .output()
            .unwrap();
        assert_eq!(second.status.code(), Some(0));
        let mut json = envelope(second.stdout);
        let ids: Vec<u64> = json["data"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3]);
        redact(&mut json);
        insta::assert_json_snapshot!("list_second_page", json);
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let fixture = fixture().await;
        seed_issue(
            &fixture.url,
            "policy_noncompliant",
            "medium",
            "normal",
            "open",
        )
        .await;
        seed_issue(
            &fixture.url,
            "terminal_failure",
            "high",
            "urgent",
            "planned",
        )
        .await;

        let output = cli(&fixture.url)
            .args(["list", "--status", "planned"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        redact(&mut json);
        insta::assert_json_snapshot!("list_filters_by_status", json);
    }

    #[tokio::test]
    async fn show_outputs_record() {
        let fixture = fixture().await;
        seed_issue(&fixture.url, "terminal_failure", "high", "high", "open").await;

        let output = cli(&fixture.url)
            .args(["show", "--issue-id", "1"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("show_outputs_record", json);
    }

    #[tokio::test]
    async fn show_unknown_id_is_not_found() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args(["show", "--issue-id", "999"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact(&mut json);
        insta::assert_json_snapshot!("show_unknown_id_is_not_found", json);
    }

    #[tokio::test]
    async fn resolve_transitions_to_resolved() {
        let fixture = fixture().await;
        seed_issue(&fixture.url, "terminal_failure", "high", "high", "open").await;

        let output = cli(&fixture.url)
            .args(["resolve", "--issue-id", "1"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["status"], "resolved");
        redact(&mut json);
        insta::assert_json_snapshot!("resolve_transitions_to_resolved", json);
    }

    #[tokio::test]
    async fn suppress_sets_horizon() {
        let fixture = fixture().await;
        seed_issue(
            &fixture.url,
            "policy_noncompliant",
            "medium",
            "normal",
            "open",
        )
        .await;

        let output = cli(&fixture.url)
            .args(["suppress", "--issue-id", "1", "--days", "7"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["status"], "suppressed");
        redact(&mut json);
        insta::assert_json_snapshot!("suppress_sets_horizon", json);
    }

    #[tokio::test]
    async fn accept_transitions_to_accepted() {
        let fixture = fixture().await;
        seed_issue(
            &fixture.url,
            "policy_noncompliant",
            "medium",
            "normal",
            "open",
        )
        .await;

        let output = cli(&fixture.url)
            .args(["accept", "--issue-id", "1"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["status"], "accepted");
        redact(&mut json);
        insta::assert_json_snapshot!("accept_transitions_to_accepted", json);
    }

    #[tokio::test]
    async fn update_overrides_priority() {
        let fixture = fixture().await;
        seed_issue(&fixture.url, "terminal_failure", "high", "normal", "open").await;

        let output = cli(&fixture.url)
            .args([
                "update",
                "--issue-id",
                "1",
                "--priority",
                "urgent",
                "--priority-reason",
                "operator escalation",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["priority"], "urgent");
        assert_eq!(json["data"]["priority_source"], "user");
        redact(&mut json);
        insta::assert_json_snapshot!("update_overrides_priority", json);
    }

    #[tokio::test]
    async fn update_unknown_id_is_not_found() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args(["update", "--issue-id", "42", "--priority", "low"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact(&mut json);
        insta::assert_json_snapshot!("update_unknown_id_is_not_found", json);
    }

    #[tokio::test]
    async fn suppress_rejects_out_of_range_days() {
        let fixture = fixture().await;
        seed_issue(
            &fixture.url,
            "policy_noncompliant",
            "medium",
            "normal",
            "open",
        )
        .await;

        // Zero and an over-cap value both fail clap range validation before
        // dispatch, so they route through the BAD_ARGS envelope (exit 1) rather
        // than reaching the panicking date arithmetic in the control plane.
        for days in ["0", "4294967295"] {
            let output = cli(&fixture.url)
                .args(["suppress", "--issue-id", "1", "--days", days])
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(1),
                "days={days} must be rejected"
            );
            let json = envelope(output.stdout);
            assert_eq!(json["error"]["code"], "BAD_ARGS", "days={days}");
        }
    }
}
