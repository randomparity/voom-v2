use std::time::Duration;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;
use voom_core::{FileVersionId, JobId};
use voom_policy::{FixtureName, TargetRef, load_fixture, load_policy_fixture};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, MediaSnapshot, NewFileVersion,
    ProducedBy,
};
use voom_store::repo::jobs::NewJob;
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, NewWorkflowSummary, WorkflowSummaryRepo,
};

use crate::cases::compliance::ComplianceExecutionOptions;
use crate::cases::cp;

use super::{active_version_with_snapshot, project_media_snapshot_input};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn job_state(cp: &crate::ControlPlane, job_id: JobId) -> String {
    sqlx::query_scalar("SELECT state FROM jobs WHERE id = ?")
        .bind(i64::try_from(job_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap()
}

fn reprobe_payload(video_codec: &str) -> Value {
    json!({
        "format": "sprint16-v1",
        "probe": { "provider": "ffprobe", "provider_version": "7.0" },
        "container": { "format_name": "mp4" },
        "streams": [
            {
                "id": "stream-0",
                "index": 0,
                "kind": "video",
                "codec_name": video_codec,
                "pixel_format": "yuv420p",
                "width": 1920,
                "height": 1080
            },
            {
                "id": "stream-1",
                "index": 1,
                "kind": "audio",
                "codec_name": "aac",
                "language": "eng"
            }
        ]
    })
}

/// Seed a fresh file asset + first version with a recorded snapshot, mirroring
/// the scan path. Returns the new version id.
async fn seed_version(
    cp: &crate::ControlPlane,
    path: &str,
    hash: &str,
    payload: Value,
) -> FileVersionId {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.to_owned(),
                content_hash: hash.to_owned(),
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
        panic!("expected new file asset");
    };
    cp.record_media_snapshot(file_version_id, None, payload, T0)
        .await
        .unwrap();
    file_version_id
}

async fn latest_snapshot(cp: &crate::ControlPlane, version: FileVersionId) -> MediaSnapshot {
    cp.identity()
        .list_media_snapshots_by_version(version)
        .await
        .unwrap()
        .into_iter()
        .max_by_key(|snapshot| snapshot.id.0)
        .unwrap()
}

#[tokio::test]
async fn project_media_snapshot_input_round_trips_committed_facts() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(&cp, "/srv/a.mp4", "hash-a", reprobe_payload("h264")).await;
    let snapshot = latest_snapshot(&cp, version).await;

    let input = project_media_snapshot_input(7, &snapshot);

    assert_eq!(input.ordinal, 7);
    assert_eq!(input.target, TargetRef::FileVersion { id: version });
    assert_eq!(input.container.as_deref(), Some("mp4"));
    assert_eq!(input.video_codec.as_deref(), Some("h264"));
    assert_eq!(input.width, Some(1920));
    assert_eq!(input.height, Some(1080));
    assert_eq!(input.existing_media_snapshot_id, Some(snapshot.id));
    assert_eq!(input.hdr, None);
    assert_eq!(input.bitrate, None);
    assert_eq!(input.duration_millis, None);
    // stream_summary forwards the streams verbatim for the planner's per-stream readers.
    assert_eq!(input.stream_summary["video_stream_count"], json!(1));
    assert_eq!(input.stream_summary["streams"][0]["codec_name"], "h264");
    assert_eq!(input.stream_summary["streams"][1]["kind"], "audio");
}

#[tokio::test]
async fn active_version_with_snapshot_picks_latest_committed_tip() {
    let (cp, _tmp) = cp().await;
    let v1 = seed_version(&cp, "/srv/b.mkv", "hash-b1", reprobe_payload("hevc")).await;
    let asset_id = cp
        .identity()
        .get_file_version(v1)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let v2 = cp
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: "hash-b2".to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Transcode,
            produced_from_version_id: Some(v1),
            created_at: T0,
        })
        .await
        .unwrap();
    let v2_snapshot = cp
        .record_media_snapshot(v2.id, None, reprobe_payload("h264"), T0)
        .await
        .unwrap();

    let (tip, snapshot) = active_version_with_snapshot(cp.identity(), asset_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(tip.id, v2.id);
    assert_eq!(snapshot.id, v2_snapshot.id);
    assert_eq!(snapshot.payload["streams"][0]["codec_name"], "h264");
}

#[tokio::test]
async fn active_version_with_snapshot_skips_retired_tip() {
    let (cp, _tmp) = cp().await;
    let v1 = seed_version(&cp, "/srv/c.mkv", "hash-c1", reprobe_payload("hevc")).await;
    let v1_snapshot = latest_snapshot(&cp, v1).await;
    let asset_id = cp
        .identity()
        .get_file_version(v1)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let v2 = cp
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: "hash-c2".to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Transcode,
            produced_from_version_id: Some(v1),
            created_at: T0,
        })
        .await
        .unwrap();
    cp.record_media_snapshot(v2.id, None, reprobe_payload("h264"), T0)
        .await
        .unwrap();
    let retired_at = T0.format(&Iso8601::DEFAULT).unwrap();
    sqlx::query("UPDATE file_versions SET retired_at = ? WHERE id = ?")
        .bind(&retired_at)
        .bind(i64::try_from(v2.id.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();

    let (tip, snapshot) = active_version_with_snapshot(cp.identity(), asset_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(tip.id, v1);
    assert_eq!(snapshot.id, v1_snapshot.id);
}

fn payload_without_container() -> Value {
    json!({
        "format": "sprint16-v1",
        "probe": { "provider": "ffprobe", "provider_version": "7.0" },
        "streams": [
            { "id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264" }
        ]
    })
}

fn file_draft(slug: &str, snapshots: &[MediaSnapshot]) -> voom_policy::PolicyInputSetDraft {
    voom_policy::PolicyInputSetDraft {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        schema_version: 1,
        source_kind: voom_policy::PolicyInputSourceKind::Imported,
        created_at: T0,
        description: None,
        fixture_labels: vec![slug.replace('-', "_")],
        synthetic_targets: Vec::new(),
        media_snapshots: snapshots
            .iter()
            .enumerate()
            .map(|(index, snapshot)| {
                project_media_snapshot_input(u32::try_from(index + 1).unwrap(), snapshot)
            })
            .collect(),
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

#[tokio::test]
async fn run_phase_barrier_rejects_colliding_branch_ids_before_opening_job() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    // Two files under different directories share the stem `movie`.
    let v1 = seed_version(
        &cp,
        "/lib/a/movie.mkv",
        "hash-collide-1",
        reprobe_payload("h264"),
    )
    .await;
    let v2 = seed_version(
        &cp,
        "/lib/b/movie.mkv",
        "hash-collide-2",
        reprobe_payload("hevc"),
    )
    .await;
    let s1 = latest_snapshot(&cp, v1).await;
    let s2 = latest_snapshot(&cp, v2).await;
    let input = cp
        .create_policy_input_set(file_draft("collide", &[s1, s2]))
        .await
        .unwrap();

    let err = cp
        .run_phase_barrier(
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();

    assert_eq!(err.source.code(), "CONFIG_INVALID");
    assert!(err.source.to_string().contains("movie"));
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "no job should open when branch ids collide");
}

#[tokio::test]
async fn run_phase_barrier_drops_unplannable_file_as_blocked() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let version = seed_version(
        &cp,
        "/lib/blocked/movie.mkv",
        "hash-blocked",
        payload_without_container(),
    )
    .await;
    let snapshot = latest_snapshot(&cp, version).await;
    let input = cp
        .create_policy_input_set(file_draft("blocked-file", &[snapshot]))
        .await
        .unwrap();

    let outcome = cp
        .run_phase_barrier(
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(job_state(&cp, outcome.job_id).await, "succeeded");
    assert!(
        outcome
            .file_phases
            .iter()
            .any(|row| row.outcome == FilePhaseOutcome::Blocked),
        "expected a blocked file-phase row, got {:?}",
        outcome.file_phases
    );
    assert!(
        outcome
            .file_phases
            .iter()
            .all(|row| row.outcome != FilePhaseOutcome::Committed),
        "no file should commit when the only file is blocked"
    );
    let tickets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ?")
        .bind(i64::try_from(outcome.job_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(tickets, 0, "a blocked phase dispatches no tickets");

    // Issue #164 / ADR-0008: even an all-blocked phase (nothing committed) must
    // still record a report, and that report must carry the blocked file's
    // diagnostic — the per-(file, phase) row has no diagnostic field, so the
    // report is the only durable record of *why* the file blocked. Recording
    // `None` here (the rejected survivors-only design) would lose it.
    let phase = outcome.phases.first().unwrap();
    assert!(
        phase.report.is_some(),
        "an all-blocked phase must still record a report (ADR-0008), got None"
    );
    let report = phase.report.as_ref().unwrap();
    assert!(
        !report.report["diagnostics"].as_array().unwrap().is_empty(),
        "blocked phase report must carry the planner diagnostic, got {:?}",
        report.report["diagnostics"]
    );
}

#[tokio::test]
async fn run_phase_barrier_with_no_file_targets_succeeds_with_zero_phase_summary() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    // The compliant-baseline fixture's snapshot targets are synthetic, so the
    // coordinator's active *file* set is empty: no FileVersion to advance.
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();

    let outcome = cp
        .run_phase_barrier(
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(job_state(&cp, outcome.job_id).await, "succeeded");
    assert_eq!(outcome.summary.branch_count, 0);
    assert_eq!(outcome.summary.ticket_count, 0);
    assert!(outcome.phases.is_empty());
    assert!(outcome.file_phases.is_empty());
    assert!(
        cp.workflow_summaries()
            .phases_for_job(outcome.job_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        cp.workflow_summaries()
            .get_summary(outcome.job_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn control_plane_persists_workflow_summary_over_shared_pool() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();

    cp.workflow_summaries()
        .insert_summary(
            NewWorkflowSummary {
                job_id: job.id,
                branch_count: 2,
                ticket_count: 3,
                dispatch_count: 3,
                retry_count: 0,
                failure_count: 0,
                peak_active_workflow_leases: 1,
                elapsed: Duration::from_millis(5),
                per_operation: json!({ "transcode_video": 1 }),
            },
            T0,
        )
        .await
        .unwrap();

    let summary = cp
        .workflow_summaries()
        .get_summary(job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.job_id, job.id);
    assert_eq!(summary.branch_count, 2);
    assert_eq!(summary.ticket_count, 3);
    assert_eq!(summary.per_operation, json!({ "transcode_video": 1 }));
}

#[tokio::test]
async fn active_version_with_snapshot_returns_none_for_unknown_asset() {
    let (cp, _tmp) = cp().await;

    let result = active_version_with_snapshot(cp.identity(), voom_core::FileAssetId(9_999))
        .await
        .unwrap();

    assert!(result.is_none());
}

/// Build a single-phase compiled policy whose phase carries the given `on_error`
/// strategy. `CompiledPolicy::minimal_for_test` is `#[cfg(test)]`-private to
/// `voom-policy`, so this builds it from public fields instead.
fn policy_with_on_error(strategy: Option<voom_policy::ErrorStrategy>) -> voom_policy::CompiledPolicy {
    voom_policy::CompiledPolicy {
        policy_name: "guarded".to_owned(),
        slug: "guarded".to_owned(),
        source_hash: "src-hash-onerr".to_owned(),
        schema_version: 2,
        metadata: std::collections::BTreeMap::new(),
        config: std::collections::BTreeMap::new(),
        phases: vec![voom_policy::CompiledPhase {
            name: "normalize".to_owned(),
            depends_on: Vec::new(),
            run_if: None,
            skip_if: None,
            on_error: strategy,
            operations: Vec::new(),
        }],
        phase_order: vec!["normalize".to_owned()],
        warnings: Vec::new(),
        provenance: voom_policy::PolicyProvenance::default(),
    }
}

#[test]
fn reject_unhandled_on_error_rejects_continue() {
    let err = super::reject_unhandled_on_error(&policy_with_on_error(Some(
        voom_policy::ErrorStrategy::Continue,
    )))
    .unwrap_err();
    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(err.to_string().contains("normalize"), "names the phase: {err}");
    assert!(err.to_string().contains("continue"), "names the strategy: {err}");
}

#[test]
fn reject_unhandled_on_error_rejects_skip() {
    let err = super::reject_unhandled_on_error(&policy_with_on_error(Some(
        voom_policy::ErrorStrategy::Skip,
    )))
    .unwrap_err();
    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(err.to_string().contains("normalize"));
    assert!(err.to_string().contains("skip"));
}

#[test]
fn reject_unhandled_on_error_allows_abort_and_unset() {
    assert!(
        super::reject_unhandled_on_error(&policy_with_on_error(Some(
            voom_policy::ErrorStrategy::Abort
        )))
        .is_ok()
    );
    assert!(super::reject_unhandled_on_error(&policy_with_on_error(None)).is_ok());
}
