use super::{ready_chaos, run_real_owner_process};
use crate::support::chaos_librarian::ChaosLibrarian;
use crate::support::observed_state::export_observed_state;
use serde_json::Value;
use std::path::Path;

pub async fn run() {
    let chaos = ready_chaos();
    let work = crate::support::chaos_librarian::retained_tempdir().unwrap();
    let directory = work.path().canonicalize().unwrap().join("corpus");
    let generated = helper(&chaos, "generate", &directory, None).await;
    assert_eq!(generated["seed"], 248);
    assert_eq!(generated["cells"].as_array().unwrap().len(), 15);
    assert_eq!(generated["network_consumer_phase"], "recovered-only");
    for (name, count, failed) in [
        ("adversarial", 18, 2),
        ("replay", 18, 2),
        ("corrupt-tags", 2, 1),
        ("network", 8, 0),
        ("ass", 1, 0),
    ] {
        let run = directory.join(name);
        let scan = directory.join(format!("scan-{name}"));
        std::fs::create_dir(&scan).unwrap();
        let evidence = run_real_owner_process(
            &chaos,
            &run.join("library"),
            &scan,
            "real_owner_scan.py",
            &[
                "--existing-fixtures",
                "--media-extension",
                "mp4",
                "--media-extension",
                "mkv",
                "--media-extension",
                "mp3",
            ],
        )
        .await
        .unwrap();
        assert_eq!(evidence["scan"]["status"], "succeeded", "{name}");
        assert_eq!(evidence["scan"]["observation_count"], count, "{name}");
        assert_eq!(evidence["completion"]["failed_content_count"], failed);
        assert_eq!(evidence["completion"]["degraded_evidence_count"], 0);
        let url = format!("sqlite://{}", scan.join("stack/voom.db").display());
        assert_durable(&url, &evidence, count, failed).await;
        let observed = run.join("observed-state.json");
        export_observed_state(&url, &run, &observed, env!("CARGO_PKG_VERSION"))
            .await
            .unwrap();
        let compared = helper(&chaos, "compare", &directory, Some(name)).await;
        assert_eq!(compared["semantic_assets"], count - failed);
        assert_eq!(compared["failed_assets"].as_array().unwrap().len(), failed);
    }
}

async fn assert_durable(url: &str, evidence: &Value, count: usize, failed: usize) {
    let pool = voom_store::connect(url).await.unwrap();
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT provider_relative_locator, evidence_json FROM scan_observations \
         ORDER BY provider_relative_locator",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), count);
    assert_eq!(
        rows.iter().filter(|(_, value)| value.is_none()).count(),
        failed
    );
    for (locator, persisted) in rows {
        let calls: Vec<_> = evidence["probe_invocations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| {
                call["args"].as_array().unwrap().iter().any(|arg| {
                    arg.as_str()
                        .is_some_and(|arg| Path::new(arg).ends_with(Path::new(&locator)))
                })
            })
            .collect();
        assert_eq!(calls.len(), 1, "one actual worker probe per {locator}");
        assert_eq!(calls[0]["exit_code"] == 0, persisted.is_some(), "{locator}");
    }
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM media_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(usize::try_from(snapshots).unwrap(), count - failed);
    pool.close().await;
}

async fn helper(chaos: &ChaosLibrarian, mode: &str, directory: &Path, name: Option<&str>) -> Value {
    let mut command = tokio::process::Command::new("uv");
    command
        .current_dir(&chaos.submodule_dir)
        .args(["run", "python"])
        .arg(
            chaos
                .workspace_root
                .join("crates/voom-cli/tests/support/fixed_seed_corpus.py"),
        )
        .arg(mode)
        .arg(directory);
    if let Some(name) = name {
        command.arg(name);
    }
    command.kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_mins(15), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "corpus {mode}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
