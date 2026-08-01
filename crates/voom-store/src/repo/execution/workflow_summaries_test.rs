use serde_json::json;
use std::time::Duration;
use time::OffsetDateTime;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{
    ArtifactHandleId, FileLocationId, FileVersionId, JobId, MediaSnapshotId, TicketId,
};

use super::*;
use crate::repo::workflow_progress::{
    FileAdmissionTier, FileProgressState, NewFilePhaseEntry, NewFileProgress,
    SqliteWorkflowProgressRepo,
};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const JOB: JobId = JobId(1);

async fn repo() -> (SqliteWorkflowSummaryRepo, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    seed_refs(&pool).await;
    (SqliteWorkflowSummaryRepo::new(pool), tmp)
}

async fn progress_repos() -> (
    SqliteWorkflowProgressRepo,
    SqliteWorkflowSummaryRepo,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    seed_refs(&pool).await;
    (
        SqliteWorkflowProgressRepo::new(pool.clone()),
        SqliteWorkflowSummaryRepo::new(pool),
        tmp,
    )
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
        artifact_verification_id: None,
        reprobe_snapshot_id: Some(MediaSnapshotId(1)),
        outcome: FilePhaseOutcome::Committed,
    }
}

fn verified_file_phase(branch_id: &str) -> NewFilePhaseSummary {
    NewFilePhaseSummary {
        job_id: JOB,
        phase_ordinal: 0,
        branch_id: branch_id.to_owned(),
        ticket_ids: vec![TicketId(1)],
        produced_file_version_id: Some(FileVersionId(1)),
        produced_file_location_id: Some(FileLocationId(1)),
        artifact_handle_id: Some(ArtifactHandleId(1)),
        artifact_verification_id: Some(ArtifactVerificationId(1)),
        reprobe_snapshot_id: Some(MediaSnapshotId(1)),
        outcome: FilePhaseOutcome::Verified,
    }
}

fn file_run_start(branch_id: &str, version_id: u64, phase_ordinal: u32) -> NewFileRunStart {
    NewFileRunStart {
        branch_id: branch_id.to_owned(),
        starting_file_version_id: FileVersionId(version_id),
        starting_phase_ordinal: phase_ordinal,
    }
}

fn file_run_history(
    branch_id: &str,
    phase_ordinal: u32,
    outcome: FilePhaseOutcome,
) -> NewFileRunHistory {
    NewFileRunHistory {
        branch_id: branch_id.to_owned(),
        phase_ordinal,
        outcome,
    }
}

fn file_progress(branch_id: &str, input_ordinal: u32) -> NewFileProgress {
    NewFileProgress {
        branch_id: branch_id.to_owned(),
        input_ordinal,
        admission_tier: FileAdmissionTier::Pending,
        next_phase_ordinal: 0,
    }
}

#[tokio::test]
async fn file_window_admission_is_bounded_and_refills_after_terminal() {
    let (repo, summaries, _tmp) = progress_repos().await;
    summaries
        .insert_file_run_starts(
            JOB,
            vec![
                file_run_start("alpha", 1, 0),
                file_run_start("beta", 1, 0),
                file_run_start("gamma", 1, 0),
            ],
        )
        .await
        .unwrap();
    repo.insert_file_window(
        JOB,
        2,
        vec![
            file_progress("alpha", 0),
            file_progress("beta", 1),
            file_progress("gamma", 2),
        ],
        T0,
    )
    .await
    .unwrap();

    assert_eq!(
        repo.admit_next_file(JOB, T0)
            .await
            .unwrap()
            .unwrap()
            .branch_id,
        "alpha"
    );
    assert_eq!(
        repo.admit_next_file(JOB, T0)
            .await
            .unwrap()
            .unwrap()
            .branch_id,
        "beta"
    );
    assert!(repo.admit_next_file(JOB, T0).await.unwrap().is_none());

    repo.begin_file_terminalization(JOB, "alpha").await.unwrap();
    assert!(repo.admit_next_file(JOB, T0).await.unwrap().is_none());
    repo.mark_file_terminal(JOB, "alpha", T0).await.unwrap();
    assert_eq!(
        repo.admit_next_file(JOB, T0)
            .await
            .unwrap()
            .unwrap()
            .branch_id,
        "gamma"
    );
    let rows = repo.file_progress_for_job(JOB).await.unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.state == FileProgressState::Active)
            .count(),
        2
    );
}

#[tokio::test]
async fn interrupted_resume_files_are_admitted_before_untouched_inputs() {
    let (repo, summaries, _tmp) = progress_repos().await;
    summaries
        .insert_file_run_starts(
            JOB,
            vec![
                file_run_start("untouched", 1, 0),
                file_run_start("interrupted", 1, 0),
            ],
        )
        .await
        .unwrap();
    repo.insert_file_window(
        JOB,
        1,
        vec![
            NewFileProgress {
                branch_id: "untouched".to_owned(),
                input_ordinal: 0,
                admission_tier: FileAdmissionTier::Pending,
                next_phase_ordinal: 0,
            },
            NewFileProgress {
                branch_id: "interrupted".to_owned(),
                input_ordinal: 4,
                admission_tier: FileAdmissionTier::Interrupted,
                next_phase_ordinal: 1,
            },
        ],
        T0,
    )
    .await
    .unwrap();

    let admitted = repo.admit_next_file(JOB, T0).await.unwrap().unwrap();

    assert_eq!(admitted.branch_id, "interrupted");
    assert_eq!(admitted.input_ordinal, 4);
}

#[tokio::test]
async fn cancelled_job_cannot_admit_a_pending_file() {
    let (repo, summaries, _tmp) = progress_repos().await;
    summaries
        .insert_file_run_starts(JOB, vec![file_run_start("alpha", 1, 0)])
        .await
        .unwrap();
    repo.insert_file_window(JOB, 1, vec![file_progress("alpha", 0)], T0)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET state = 'cancelled' WHERE id = ?")
        .bind(i64::try_from(JOB.0).unwrap())
        .execute(&summaries.pool)
        .await
        .unwrap();

    let error = repo.admit_next_file(JOB, T0).await.unwrap_err();

    assert!(matches!(error, voom_core::VoomError::UserCancellation(_)));
    assert_eq!(
        repo.file_progress(JOB, "alpha")
            .await
            .unwrap()
            .unwrap()
            .state,
        FileProgressState::Pending
    );
}

#[tokio::test]
async fn phase_entry_is_durable_and_replay_must_match() {
    let (repo, summaries, _tmp) = progress_repos().await;
    summaries
        .insert_file_run_starts(JOB, vec![file_run_start("alpha", 1, 0)])
        .await
        .unwrap();
    let input = NewFilePhaseEntry {
        job_id: JOB,
        phase_ordinal: 0,
        branch_id: "alpha".to_owned(),
        media_snapshot_id: MediaSnapshotId(1),
        gate_admitted: true,
    };

    let first = repo
        .upsert_file_phase_entry(input.clone(), T0)
        .await
        .unwrap();
    let replay = repo.upsert_file_phase_entry(input, T0).await.unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        repo.file_phase_entries_for_job(JOB).await.unwrap(),
        vec![first]
    );

    let error = repo
        .upsert_file_phase_entry(
            NewFilePhaseEntry {
                job_id: JOB,
                phase_ordinal: 0,
                branch_id: "alpha".to_owned(),
                media_snapshot_id: MediaSnapshotId(1),
                gate_admitted: false,
            },
            T0,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, voom_core::VoomError::Conflict(_)));
}

#[tokio::test]
async fn concurrent_file_admission_never_exceeds_durable_capacity() {
    let (repo, summaries, _tmp) = progress_repos().await;
    let branches = ["alpha", "beta", "gamma", "delta"];
    summaries
        .insert_file_run_starts(
            JOB,
            branches
                .iter()
                .map(|branch| file_run_start(branch, 1, 0))
                .collect(),
        )
        .await
        .unwrap();
    repo.insert_file_window(
        JOB,
        2,
        branches
            .iter()
            .enumerate()
            .map(|(ordinal, branch)| NewFileProgress {
                branch_id: (*branch).to_owned(),
                input_ordinal: u32::try_from(ordinal).unwrap(),
                admission_tier: FileAdmissionTier::Pending,
                next_phase_ordinal: 0,
            })
            .collect(),
        T0,
    )
    .await
    .unwrap();
    let attempts = (0..8)
        .map(|_| {
            let repo = repo.clone();
            tokio::spawn(async move { repo.admit_next_file(JOB, T0).await.unwrap() })
        })
        .collect::<Vec<_>>();
    let mut admitted = Vec::new();
    for attempt in attempts {
        if let Some(row) = attempt.await.unwrap() {
            admitted.push(row.branch_id);
        }
    }

    admitted.sort();
    assert_eq!(admitted, vec!["alpha", "beta"]);
    let rows = repo.file_progress_for_job(JOB).await.unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.state == FileProgressState::Active)
            .count(),
        2
    );
}

#[tokio::test]
async fn file_progress_cursor_advances_once_from_expected_phase() {
    let (repo, summaries, _tmp) = progress_repos().await;
    summaries
        .insert_file_run_starts(JOB, vec![file_run_start("alpha", 1, 0)])
        .await
        .unwrap();
    repo.insert_file_window(JOB, 1, vec![file_progress("alpha", 0)], T0)
        .await
        .unwrap();
    repo.admit_next_file(JOB, T0).await.unwrap();

    assert!(
        repo.advance_file_progress(JOB, "alpha", 0, 1)
            .await
            .unwrap()
    );
    assert!(
        !repo
            .advance_file_progress(JOB, "alpha", 0, 1)
            .await
            .unwrap()
    );
    assert_eq!(
        repo.file_progress_for_job(JOB).await.unwrap()[0].next_phase_ordinal,
        1
    );
}

#[tokio::test]
async fn file_phase_and_cursor_checkpoint_commit_atomically_and_replay() {
    let (repo, summaries, _tmp) = progress_repos().await;
    summaries
        .insert_file_run_starts(JOB, vec![file_run_start("alpha", 1, 0)])
        .await
        .unwrap();
    repo.insert_file_window(JOB, 1, vec![file_progress("alpha", 0)], T0)
        .await
        .unwrap();
    repo.admit_next_file(JOB, T0).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_cursor_advance BEFORE UPDATE OF next_phase_ordinal \
         ON workflow_file_progress BEGIN SELECT RAISE(ABORT, 'forced cursor failure'); END",
    )
    .execute(&summaries.pool)
    .await
    .unwrap();

    let error = repo
        .upsert_file_phase_summary_and_advance(committed_file_phase("alpha"), 0, 1, T0)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert!(
        summaries
            .get_file_phase_summary(JOB, 0, "alpha")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.file_progress(JOB, "alpha")
            .await
            .unwrap()
            .unwrap()
            .next_phase_ordinal,
        0
    );
    sqlx::query("DROP TRIGGER fail_cursor_advance")
        .execute(&summaries.pool)
        .await
        .unwrap();

    let first = repo
        .upsert_file_phase_summary_and_advance(committed_file_phase("alpha"), 0, 1, T0)
        .await
        .unwrap();
    let replayed = repo
        .upsert_file_phase_summary_and_advance(committed_file_phase("alpha"), 0, 1, T0)
        .await
        .unwrap();

    assert_eq!(first, replayed);
    assert_eq!(
        repo.file_progress(JOB, "alpha")
            .await
            .unwrap()
            .unwrap()
            .next_phase_ordinal,
        1
    );
}

#[tokio::test]
async fn file_run_starts_insert_atomically_and_list_by_branch() {
    let (repo, _tmp) = repo().await;

    let inserted = repo
        .insert_file_run_starts(
            JOB,
            vec![file_run_start("zeta", 1, 2), file_run_start("alpha", 1, 0)],
        )
        .await
        .unwrap();
    assert_eq!(
        inserted
            .iter()
            .map(|row| row.branch_id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "zeta"]
    );
    assert_eq!(inserted[1].starting_phase_ordinal, 2);

    let listed = repo.file_run_starts_for_job(JOB).await.unwrap();
    assert_eq!(listed, inserted);
}

#[tokio::test]
async fn file_run_start_batch_rolls_back_on_invalid_member() {
    let (repo, _tmp) = repo().await;

    let err = repo
        .insert_file_run_starts(
            JOB,
            vec![
                file_run_start("valid", 1, 0),
                file_run_start("missing-version", 999, 0),
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert_eq!(
        repo.file_run_starts_for_job(JOB).await.unwrap(),
        Vec::<FileRunStart>::new()
    );
}

#[tokio::test]
async fn file_run_history_inserts_atomically_and_lists_by_branch_then_phase() {
    let (repo, _tmp) = repo().await;
    repo.insert_file_run_starts(
        JOB,
        vec![file_run_start("zeta", 1, 2), file_run_start("alpha", 1, 2)],
    )
    .await
    .unwrap();

    let inserted = repo
        .insert_file_run_history(
            JOB,
            vec![
                file_run_history("zeta", 1, FilePhaseOutcome::Skipped),
                file_run_history("alpha", 1, FilePhaseOutcome::Committed),
                file_run_history("alpha", 0, FilePhaseOutcome::Skipped),
            ],
        )
        .await
        .unwrap();

    assert_eq!(
        inserted
            .iter()
            .map(|row| (row.branch_id.as_str(), row.phase_ordinal, row.outcome))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", 0, FilePhaseOutcome::Skipped),
            ("alpha", 1, FilePhaseOutcome::Committed),
            ("zeta", 1, FilePhaseOutcome::Skipped),
        ]
    );
    assert_eq!(repo.file_run_history_for_job(JOB).await.unwrap(), inserted);
}

#[tokio::test]
async fn file_run_history_accepts_blocked_terminal_outcome() {
    let (repo, _tmp) = repo().await;
    repo.insert_file_run_starts(JOB, vec![file_run_start("alpha", 1, 2)])
        .await
        .unwrap();

    let inserted = repo
        .insert_file_run_history(
            JOB,
            vec![
                file_run_history("alpha", 0, FilePhaseOutcome::Committed),
                file_run_history("alpha", 1, FilePhaseOutcome::Blocked),
            ],
        )
        .await
        .unwrap();

    assert_eq!(repo.file_run_history_for_job(JOB).await.unwrap(), inserted);
    assert_eq!(inserted[1].outcome, FilePhaseOutcome::Blocked);
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
    assert_eq!(got.artifact_verification_id, None);
    assert_eq!(got.reprobe_snapshot_id, Some(MediaSnapshotId(1)));
    assert_eq!(got.outcome, FilePhaseOutcome::Committed);
}

#[tokio::test]
async fn verified_file_phase_links_unchanged_file_and_exact_evidence() {
    let (repo, _tmp) = repo().await;
    seed_verification(&repo.pool).await;

    let inserted = repo
        .upsert_file_phase_summary(verified_file_phase("verified"), T0)
        .await
        .unwrap();

    assert_eq!(inserted.outcome, FilePhaseOutcome::Verified);
    assert_eq!(
        inserted.artifact_verification_id,
        Some(ArtifactVerificationId(1))
    );
    assert_eq!(inserted.produced_file_version_id, Some(FileVersionId(1)));
}

#[tokio::test]
async fn verified_file_phase_requires_exact_evidence() {
    let (repo, _tmp) = repo().await;
    let mut input = verified_file_phase("missing-evidence");
    input.artifact_verification_id = None;

    let error = repo.upsert_file_phase_summary(input, T0).await.unwrap_err();

    assert!(matches!(error, voom_core::VoomError::Database { .. }));
}

async fn seed_verification(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO workers \
         (id, name, kind, status, registered_at, last_seen_at) \
         VALUES (1, 'verify', 'local', 'registered', \
         '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artifact_locations \
         (id, artifact_handle_id, kind, value, observed_at) \
         VALUES (1, 1, 'local_path', '/media/1.mkv', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artifact_verifications \
         (id, artifact_handle_id, artifact_location_id, path, worker_id, status, \
          expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
          report, started_at, finished_at) \
         VALUES (1, 1, 1, '/media/1.mkv', 1, 'succeeded', 100, 'hash-1', 100, \
         'hash-1', '{}', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn committed_requires_produced_lineage() {
    let (repo, _tmp) = repo().await;

    let mut input = committed_file_phase("a");
    input.produced_file_version_id = None;

    let err = repo.upsert_file_phase_summary(input, T0).await.unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database { .. }),
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
        matches!(err, voom_core::VoomError::Database { .. }),
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
        matches!(err, voom_core::VoomError::Database { .. }),
        "expected a foreign-key violation, got {err:?}"
    );

    let mut bad_snapshot = committed_file_phase("b");
    bad_snapshot.reprobe_snapshot_id = Some(MediaSnapshotId(9999));
    let err = repo
        .upsert_file_phase_summary(bad_snapshot, T0)
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Database { .. }),
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
        artifact_verification_id: None,
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
