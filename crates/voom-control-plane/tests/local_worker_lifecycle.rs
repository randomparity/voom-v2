#![expect(
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

//! Lifecycle coverage for `ControlPlane::start_local_worker`: it must register a
//! node-less worker, spawn the bundled `voom-ffmpeg-worker`, record its live
//! endpoint+secret so registry discovery can find it, and retire the row on
//! `shutdown_and_retire`. Uses REAL time (a real `SQLite` pool is present; never
//! `tokio::time::pause`).

use sqlx::Row;
use tempfile::NamedTempFile;
use voom_control_plane::{ControlPlane, LocalWorkerKind};
use voom_store::repo::workers::WorkerStatus;
use voom_test_support::worker::cargo_build_package;

#[tokio::test]
async fn start_local_worker_registers_endpoint_then_retires_on_shutdown() {
    // The production sibling-binary resolution looks for `voom-ffmpeg-worker`
    // next to the running test binary; build it into the active profile dir.
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let running = cp
        .start_local_worker(LocalWorkerKind::Ffmpeg)
        .await
        .unwrap();
    let worker_id = running.handle().worker_id;
    assert_eq!(running.handle().endpoint.ip().to_string(), "127.0.0.1");
    assert_ne!(running.handle().endpoint.port(), 0);

    let live = live_worker_named(&cp, "local-ffmpeg").await;
    assert!(
        live.is_some(),
        "a live local-ffmpeg worker must exist after start_local_worker"
    );
    let live_id = live.unwrap();
    assert_eq!(live_id, worker_id.0);

    let endpoint = recorded_endpoint(&url, worker_id.0).await;
    assert!(
        !endpoint.is_empty(),
        "the worker capability must record a non-empty endpoint"
    );
    let secret = recorded_secret(&url, worker_id.0).await;
    assert!(
        !secret.is_empty(),
        "the worker capability must record a non-empty secret"
    );

    running.shutdown_and_retire(&cp).await.unwrap();

    assert!(
        live_worker_named(&cp, "local-ffmpeg").await.is_none(),
        "the worker must not be in the live set after shutdown_and_retire"
    );
    let inspection = cp.get_worker_inspection(worker_id).await.unwrap().unwrap();
    assert_eq!(inspection.worker.status, WorkerStatus::Retired);
}

#[tokio::test]
async fn start_local_worker_self_heals_a_stale_same_name_worker() {
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let first = cp
        .start_local_worker(LocalWorkerKind::Ffmpeg)
        .await
        .unwrap();
    let first_id = first.handle().worker_id;
    // Simulate a hard kill that left a stale registered row: drop the running
    // worker without retiring it. Drop kills the child but leaves the DB row.
    drop(first);

    let second = cp
        .start_local_worker(LocalWorkerKind::Ffmpeg)
        .await
        .unwrap();
    let second_id = second.handle().worker_id;
    assert_ne!(first_id.0, second_id.0);

    // The prior same-name worker was self-healed (retired), so only the new one
    // is live.
    let live = live_worker_named(&cp, "local-ffmpeg").await;
    assert_eq!(live, Some(second_id.0));
    let first_inspection = cp.get_worker_inspection(first_id).await.unwrap().unwrap();
    assert_eq!(first_inspection.worker.status, WorkerStatus::Retired);

    second.shutdown_and_retire(&cp).await.unwrap();
}

async fn live_worker_named(cp: &ControlPlane, base: &str) -> Option<u64> {
    let prefix = format!("{base}-");
    let inspections = cp.list_worker_inspections(None, 1000).await.unwrap();
    inspections
        .into_iter()
        .filter(|inspection| {
            inspection.worker.name == base || inspection.worker.name.starts_with(&prefix)
        })
        .find(|inspection| {
            matches!(
                inspection.worker.status,
                WorkerStatus::Registered | WorkerStatus::Active
            )
        })
        .map(|inspection| inspection.worker.id.0)
}

async fn recorded_endpoint(url: &str, worker_id: u64) -> String {
    capability_extra_field(url, worker_id, "endpoint").await
}

async fn recorded_secret(url: &str, worker_id: u64) -> String {
    capability_extra_field(url, worker_id, "secret").await
}

async fn capability_extra_field(url: &str, worker_id: u64, field: &str) -> String {
    let pool = voom_store::connect(url).await.unwrap();
    let extra: String = sqlx::query(
        "SELECT extra FROM worker_capabilities WHERE worker_id = ? ORDER BY id ASC LIMIT 1",
    )
    .bind(i64::try_from(worker_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap()
    .get("extra");
    let parsed: serde_json::Value = serde_json::from_str(&extra).unwrap();
    parsed[field].as_str().unwrap_or_default().to_owned()
}
