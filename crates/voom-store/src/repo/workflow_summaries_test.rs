use serde_json::json;
use std::time::Duration;
use time::OffsetDateTime;
use voom_core::{
    ArtifactHandleId, FileLocationId, FileVersionId, JobId, MediaSnapshotId, TicketId,
};

use super::*;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const JOB: JobId = JobId(1);

async fn repo() -> (SqliteWorkflowSummaryRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    seed_refs(&pool).await;
    (SqliteWorkflowSummaryRepo::new(pool), tmp)
}

/// Seed the FK targets a committed per-`(file, phase)` row links: one job, one
/// `file_version` chain (asset → version → location → snapshot), one artifact
/// handle. All use id = 1.
async fn seed_refs(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) \
         VALUES (1, 'workflow', 'open', 0, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO file_assets (id, created_at) VALUES (1, '1970-01-01T00:00:00Z')")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO file_versions \
         (id, file_asset_id, content_hash, size_bytes, produced_by, created_at) \
         VALUES (1, 1, 'hash-1', 100, 'ingest', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO file_locations (id, file_version_id, kind, value, observed_at) \
         VALUES (1, 1, 'local_path', '/media/1.mkv', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO media_snapshots (id, file_version_id, probed_at, payload) \
         VALUES (1, 1, '1970-01-01T00:00:00Z', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO artifact_handles \
         (id, privacy_class, durability_class, allowed_access_modes, mutability, created_at) \
         VALUES (1, 'internal', 'durable', '[]', 'immutable', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn sample_summary() -> NewWorkflowSummary {
    NewWorkflowSummary {
        job_id: JOB,
        branch_count: 2,
        ticket_count: 6,
        dispatch_count: 9,
        retry_count: 1,
        failure_count: 0,
        peak_active_workflow_leases: 3,
        elapsed: Duration::from_nanos(1_500_000_001),
        per_operation: json!({ "transcode_video": { "success_count": 1 } }),
    }
}

fn committed_file_phase(branch_id: &str) -> NewFilePhaseSummary {
    NewFilePhaseSummary {
        job_id: JOB,
        phase_ordinal: 0,
        branch_id: branch_id.to_owned(),
        ticket_ids: vec![TicketId(1), TicketId(2)],
        produced_file_version_id: Some(FileVersionId(1)),
        produced_file_location_id: Some(FileLocationId(1)),
        artifact_handle_id: Some(ArtifactHandleId(1)),
        reprobe_snapshot_id: Some(MediaSnapshotId(1)),
        outcome: FilePhaseOutcome::Committed,
    }
}

#[tokio::test]
async fn summary_round_trips() {
    let (repo, _tmp) = repo().await;

    let inserted = repo.insert_summary(sample_summary(), T0).await.unwrap();
    let got = repo.get_summary(JOB).await.unwrap().unwrap();

    assert_eq!(got, inserted);
    assert_eq!(got.elapsed, Duration::from_nanos(1_500_000_001));
    assert_eq!(got.dispatch_count, 9);
    assert_eq!(
        got.per_operation,
        json!({ "transcode_video": { "success_count": 1 } })
    );
    assert_eq!(got.created_at, T0);
}

#[tokio::test]
async fn get_summary_is_none_for_unknown_job() {
    let (repo, _tmp) = repo().await;
    assert!(repo.get_summary(JOB).await.unwrap().is_none());
}

#[tokio::test]
async fn phase_summary_links_report() {
    let (repo, _tmp) = repo().await;

    let input = NewPhaseSummary {
        job_id: JOB,
        phase_ordinal: 0,
        phase_name: "transcode".to_owned(),
        report: Some(PhaseReport {
            report_id: "rep-abc".to_owned(),
            report: json!({ "schema_version": 1 }),
        }),
        outcome: PhaseOutcome::Completed,
    };
    let inserted = repo.upsert_phase_summary(input, T0).await.unwrap();

    let got = repo.get_phase_summary(JOB, 0).await.unwrap().unwrap();
    assert_eq!(got, inserted);
    let report = got.report.as_ref().unwrap();
    assert_eq!(report.report_id, "rep-abc");
    assert_eq!(report.report, json!({ "schema_version": 1 }));
    assert_eq!(got.outcome, PhaseOutcome::Completed);

    let listed = repo.phases_for_job(JOB).await.unwrap();
    assert_eq!(listed, vec![got]);
}

#[tokio::test]
async fn phase_summary_skipped_has_no_report() {
    let (repo, _tmp) = repo().await;

    let input = NewPhaseSummary {
        job_id: JOB,
        phase_ordinal: 1,
        phase_name: "remux".to_owned(),
        report: None,
        outcome: PhaseOutcome::Skipped,
    };
    repo.upsert_phase_summary(input, T0).await.unwrap();

    let got = repo.get_phase_summary(JOB, 1).await.unwrap().unwrap();
    assert!(got.report.is_none());
    assert_eq!(got.outcome, PhaseOutcome::Skipped);
}

#[tokio::test]
async fn phases_for_job_are_ordered_by_ordinal() {
    let (repo, _tmp) = repo().await;

    for ordinal in [2_u32, 0, 1] {
        let input = NewPhaseSummary {
            job_id: JOB,
            phase_ordinal: ordinal,
            phase_name: format!("phase-{ordinal}"),
            report: None,
            outcome: PhaseOutcome::Skipped,
        };
        repo.upsert_phase_summary(input, T0).await.unwrap();
    }

    let ordinals: Vec<u32> = repo
        .phases_for_job(JOB)
        .await
        .unwrap()
        .iter()
        .map(|p| p.phase_ordinal)
        .collect();
    assert_eq!(ordinals, vec![0, 1, 2]);
}

#[tokio::test]
async fn file_phase_summary_links_artifacts() {
    let (repo, _tmp) = repo().await;

    let inserted = repo
        .upsert_file_phase_summary(committed_file_phase("a"), T0)
        .await
        .unwrap();

    let got = repo
        .get_file_phase_summary(JOB, 0, "a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, inserted);
    assert_eq!(got.ticket_ids, vec![TicketId(1), TicketId(2)]);
    assert_eq!(got.produced_file_version_id, Some(FileVersionId(1)));
    assert_eq!(got.produced_file_location_id, Some(FileLocationId(1)));
    assert_eq!(got.artifact_handle_id, Some(ArtifactHandleId(1)));
    assert_eq!(got.reprobe_snapshot_id, Some(MediaSnapshotId(1)));
    assert_eq!(got.outcome, FilePhaseOutcome::Committed);
}

#[tokio::test]
async fn committed_requires_produced_lineage() {
    let (repo, _tmp) = repo().await;

    let mut input = committed_file_phase("a");
    input.produced_file_version_id = None;

    let err = repo.upsert_file_phase_summary(input, T0).await.unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database(_)),
        "expected a Database CHECK violation, got {err:?}"
    );
}

#[tokio::test]
async fn committed_requires_reprobe_snapshot() {
    let (repo, _tmp) = repo().await;

    // The re-probe arm of the committed CHECK: a committed row written without
    // a re-probe snapshot violates the "written only after re-probe" invariant.
    let mut input = committed_file_phase("a");
    input.reprobe_snapshot_id = None;

    let err = repo.upsert_file_phase_summary(input, T0).await.unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database(_)),
        "expected a Database CHECK violation, got {err:?}"
    );
}

#[tokio::test]
async fn produced_ids_must_reference_real_rows() {
    let (repo, _tmp) = repo().await;

    let mut bad_version = committed_file_phase("a");
    bad_version.produced_file_version_id = Some(FileVersionId(9999));
    let err = repo
        .upsert_file_phase_summary(bad_version, T0)
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database(_)),
        "expected a foreign-key violation, got {err:?}"
    );

    let mut bad_snapshot = committed_file_phase("b");
    bad_snapshot.reprobe_snapshot_id = Some(MediaSnapshotId(9999));
    let err = repo
        .upsert_file_phase_summary(bad_snapshot, T0)
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database(_)),
        "expected a foreign-key violation, got {err:?}"
    );
}

#[tokio::test]
async fn half_committed_barrier_records_only_advanced_files() {
    let (repo, _tmp) = repo().await;

    // Branch "a" advanced; branch "b" failed and writes no row.
    repo.upsert_file_phase_summary(committed_file_phase("a"), T0)
        .await
        .unwrap();

    let rows = repo.file_phases_for_job(JOB).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].branch_id, "a");
}

#[tokio::test]
async fn file_phase_upsert_is_first_write_wins() {
    let (repo, _tmp) = repo().await;

    let first = repo
        .upsert_file_phase_summary(committed_file_phase("a"), T0)
        .await
        .unwrap();

    // A second write for the same (job, phase, branch) with a different shape
    // is a no-op and returns the already-stored row.
    let second_input = NewFilePhaseSummary {
        ticket_ids: vec![TicketId(99)],
        outcome: FilePhaseOutcome::Blocked,
        produced_file_version_id: None,
        produced_file_location_id: None,
        artifact_handle_id: None,
        reprobe_snapshot_id: None,
        ..committed_file_phase("a")
    };
    let second = repo
        .upsert_file_phase_summary(second_input, T0)
        .await
        .unwrap();

    assert_eq!(second, first);
    let rows = repo.file_phases_for_job(JOB).await.unwrap();
    assert_eq!(rows, vec![first]);
}

#[tokio::test]
async fn phase_upsert_is_first_write_wins() {
    let (repo, _tmp) = repo().await;

    let first_input = NewPhaseSummary {
        job_id: JOB,
        phase_ordinal: 0,
        phase_name: "transcode".to_owned(),
        report: Some(PhaseReport {
            report_id: "rep-1".to_owned(),
            report: json!({ "v": 1 }),
        }),
        outcome: PhaseOutcome::Completed,
    };
    let first = repo.upsert_phase_summary(first_input, T0).await.unwrap();

    let second_input = NewPhaseSummary {
        job_id: JOB,
        phase_ordinal: 0,
        phase_name: "transcode".to_owned(),
        report: None,
        outcome: PhaseOutcome::Blocked,
    };
    let second = repo.upsert_phase_summary(second_input, T0).await.unwrap();

    assert_eq!(second, first);
    assert_eq!(repo.phases_for_job(JOB).await.unwrap(), vec![first]);
}
