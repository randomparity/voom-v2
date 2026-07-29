use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;
use voom_core::{FileLocationId, FileVersionId, JobId, TicketOperation};
use voom_policy::{FixtureName, TargetRef, load_fixture, load_policy_fixture};
use voom_store::repo::identity::NewFileLocation;
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, MediaSnapshot, NewFileVersion,
    ProducedBy,
};
use voom_store::repo::jobs::NewJob;
use voom_store::repo::tickets::{NewTicket, TicketState};
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, NewFilePhaseEntry, NewFilePhaseSummary, NewFileProgress, NewFileRunHistory,
    NewFileRunStart, NewWorkflowSummary, PhaseOutcome,
};

use crate::cases::cp;
use crate::cases::policy::compliance::ComplianceExecutionOptions;

use super::PhaseFile;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn run_prepared_fresh_after_phase_plan<F, Fut>(
    cp: &crate::ControlPlane,
    inputs: super::PhaseBarrierRunInputs,
    options: ComplianceExecutionOptions,
    runtimes: crate::workflow::WorkerRuntimeRegistry,
    after_phase_plan: F,
) -> Result<super::CoordinatorOutcome, super::CoordinatorError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<(), voom_core::VoomError>>,
{
    let starts = super::run_starts_for_files(&inputs.files);
    let limit = options.file_window_limit()?;
    let (job, _) = cp
        .open_sliding_file_job(&starts, Vec::new(), Vec::new(), &inputs.files, limit)
        .await?;
    cp.workflow_summaries()
        .admit_next_file(job.id, cp.clock().now())
        .await?
        .ok_or_else(|| voom_core::VoomError::Internal("test file was not admitted".to_owned()))?;
    let super::PhaseBarrierRunInputs {
        policy,
        context,
        base_draft,
        files,
    } = inputs;
    let result = super::PhaseLoop::new(
        cp,
        super::PhaseLoopInputs {
            job_id: job.id,
            policy,
            context,
            base_draft,
            files,
            seed_file_phases: Vec::new(),
            options,
            runtimes,
        },
    )
    .run_file_pipeline_after_phase_plan(after_phase_plan)
    .await;
    let result = match result {
        Ok(_) => panic!("fault-injected pipeline unexpectedly succeeded"),
        Err(failure) => {
            let summary = cp
                .workflow_summaries()
                .insert_summary(super::zero_phase_summary(job.id), cp.clock().now())
                .await?;
            Err(super::CoordinatorError {
                source: failure.source,
                partial: Some(super::CoordinatorOutcome {
                    job_id: job.id,
                    summary,
                    phases: Vec::new(),
                    file_phases: Vec::new(),
                }),
            })
        }
    };
    cp.finish_phase_barrier_job(job.id, result).await
}

async fn run_prepared_resume_after_phase_plan<F, Fut>(
    cp: &crate::ControlPlane,
    inputs: super::PreparedResumeRunInputs,
    options: ComplianceExecutionOptions,
    runtimes: crate::workflow::WorkerRuntimeRegistry,
    after_phase_plan: F,
) -> Result<super::CoordinatorOutcome, super::CoordinatorError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<(), voom_core::VoomError>>,
{
    let super::PreparedResumeRunInputs {
        policy,
        context,
        base_draft,
        preparation:
            super::ResumePreparation {
                files,
                run_starts,
                history,
                seeds,
                max_in_flight_files: _,
            },
    } = inputs;
    let limit = options.file_window_limit()?;
    let (job, seed_file_phases) = cp
        .open_sliding_file_job(&run_starts, history, seeds, &files, limit)
        .await?;
    cp.workflow_summaries()
        .admit_next_file(job.id, cp.clock().now())
        .await?
        .ok_or_else(|| voom_core::VoomError::Internal("test file was not admitted".to_owned()))?;
    let result = super::PhaseLoop::new(
        cp,
        super::PhaseLoopInputs {
            job_id: job.id,
            policy,
            context,
            base_draft,
            files,
            seed_file_phases: seed_file_phases.clone(),
            options,
            runtimes,
        },
    )
    .run_file_pipeline_after_phase_plan(after_phase_plan)
    .await;
    let result = match result {
        Ok(_) => panic!("fault-injected resume pipeline unexpectedly succeeded"),
        Err(failure) => {
            let summary = cp
                .workflow_summaries()
                .insert_summary(super::zero_phase_summary(job.id), cp.clock().now())
                .await?;
            Err(super::CoordinatorError {
                source: failure.source,
                partial: Some(super::CoordinatorOutcome {
                    job_id: job.id,
                    summary,
                    phases: Vec::new(),
                    file_phases: seed_file_phases,
                }),
            })
        }
    };
    cp.finish_phase_barrier_job(job.id, result).await
}

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
    let (cp, _tmp) = crate::cases::cp().await;
    let version = seed_version(&cp, "/srv/a.mp4", "hash-a", reprobe_payload("h264")).await;
    let snapshot = latest_snapshot(&cp, version).await;

    let input = crate::media_snapshot::planning_input(7, &snapshot);

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

#[test]
fn project_media_snapshot_input_preserves_missing_stream_inventory() {
    let snapshot = MediaSnapshot {
        id: voom_core::MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: T0,
        payload: json!({}),
    };

    let input = crate::media_snapshot::planning_input(1, &snapshot);

    assert!(input.stream_summary.get("streams").is_none());
    assert_eq!(input.stream_summary["video_stream_count"], 0);
}

#[test]
fn regenerated_phase_report_canonicalizes_probe_container() {
    let snapshot = MediaSnapshot {
        id: voom_core::MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: T0,
        payload: json!({
            "container": {"format_name": "matroska,webm"},
            "streams": [
                {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"}
            ]
        }),
    };
    let policy = voom_policy::compile_policy(
        "policy \"canonical container\" { phase normalize { container mkv } }",
    )
    .unwrap()
    .policy;
    let report = super::regenerate_phase_report(
        &policy,
        &voom_plan::PlanningContext::default(),
        &file_draft(
            "canonical-container-report",
            std::slice::from_ref(&snapshot),
        ),
        "normalize",
        &[(1, snapshot)],
        &[true],
    )
    .unwrap();
    let check = &report.report["checks"][0];

    assert_eq!(check["check_status"], "compliant");
    assert_eq!(check["observed_state"]["container"], "mkv");
}

#[test]
fn regenerated_phase_report_blocks_malformed_probe_container() {
    let snapshot = MediaSnapshot {
        id: voom_core::MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: T0,
        payload: json!({
            "container": {"format_name": 42},
            "streams": [
                {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"}
            ]
        }),
    };
    let policy = voom_policy::compile_policy(
        "policy \"malformed container\" { phase normalize { container mkv } }",
    )
    .unwrap()
    .policy;
    let report = super::regenerate_phase_report(
        &policy,
        &voom_plan::PlanningContext::default(),
        &file_draft(
            "malformed-container-report",
            std::slice::from_ref(&snapshot),
        ),
        "normalize",
        &[(1, snapshot)],
        &[true],
    )
    .unwrap();
    let check = &report.report["checks"][0];

    assert_eq!(check["check_status"], "blocked");
    assert_eq!(check["reason"], "snapshot container is unknown");
    assert_eq!(check["execution_eligibility"], "blocked");
}

#[tokio::test]
async fn classify_phase_keeps_later_planned_operation_after_earlier_no_op() {
    let (cp, _tmp) = crate::cases::cp().await;
    let payload = json!({
        "format": "sprint16-v1",
        "probe": { "provider": "ffprobe", "provider_version": "7.0" },
        "container": { "format_name": "mkv" },
        "streams": [
            {
                "id": "stream-0",
                "index": 0,
                "kind": "video",
                "codec_name": "hevc"
            },
            {
                "id": "stream-1",
                "index": 1,
                "kind": "audio",
                "codec_name": "eac3",
                "channels": 6,
                "language": "eng"
            }
        ]
    });
    let version = seed_version(&cp, "/srv/multi-operation.mkv", "multi-operation", payload).await;
    let file = phase_file(&cp, version, "multi-operation").await;
    let policy = voom_policy::compile_policy(
        "policy \"multi operation\" { phase audio { \
         transcode audio to eac3 where language in [\"eng\"] \
         synthesize audio from channels >= 6 { codec aac channels 2 } \
         } }",
    )
    .unwrap()
    .policy;
    let plan = voom_plan::plan_phase(
        voom_plan::PlanningRequest {
            policy,
            input: file_draft("multi-operation", std::slice::from_ref(&file.snapshot)),
            context: voom_plan::PlanningContext::default(),
        },
        "audio",
    )
    .unwrap();

    let dispositions = super::classify_phase(std::slice::from_ref(&file), &plan).unwrap();

    let super::Disposition::Planned { node_ids } = &dispositions[0] else {
        panic!("a later planned operation must make the file planned");
    };
    assert_eq!(node_ids.len(), 1);
    assert_eq!(
        plan.nodes
            .iter()
            .find(|node| node.node_id == node_ids[0])
            .unwrap()
            .status,
        voom_plan::NodeStatus::Planned
    );
}

#[tokio::test]
async fn phase_dispatch_rejects_multiple_same_file_mutations() {
    let (cp, _tmp) = crate::cases::cp().await;
    let payload = json!({
        "format": "sprint16-v1",
        "probe": { "provider": "ffprobe", "provider_version": "7.0" },
        "container": { "format_name": "mkv" },
        "streams": [
            {
                "id": "stream-0",
                "index": 0,
                "kind": "video",
                "codec_name": "hevc"
            },
            {
                "id": "stream-1",
                "index": 1,
                "kind": "audio",
                "codec_name": "ac3",
                "channels": 6,
                "language": "eng"
            }
        ]
    });
    let version = seed_version(
        &cp,
        "/srv/multiple-mutations.mkv",
        "multiple-mutations",
        payload,
    )
    .await;
    let file = phase_file(&cp, version, "multiple-mutations").await;
    let policy = voom_policy::compile_policy(
        "policy \"multiple mutations\" { phase audio { \
         transcode audio to eac3 where language in [\"eng\"] \
         synthesize audio from channels >= 6 { codec aac channels 2 } \
         } }",
    )
    .unwrap()
    .policy;
    let plan = voom_plan::plan_phase(
        voom_plan::PlanningRequest {
            policy,
            input: file_draft("multiple-mutations", std::slice::from_ref(&file.snapshot)),
            context: voom_plan::PlanningContext::default(),
        },
        "audio",
    )
    .unwrap();

    let error = super::classify_phase(std::slice::from_ref(&file), &plan).unwrap_err();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(error.to_string().contains("split same-file mutations"));
    assert!(error.to_string().contains("multiple-mutations"));
}

#[tokio::test]
async fn active_version_with_snapshot_picks_latest_committed_tip() {
    let (cp, _tmp) = crate::cases::cp().await;
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

    let (tip, snapshot) = cp
        .identity()
        .get_active_version_with_snapshot(asset_id)
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

    let (tip, snapshot) = cp
        .identity()
        .get_active_version_with_snapshot(asset_id)
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
                crate::media_snapshot::planning_input(u32::try_from(index + 1).unwrap(), snapshot)
            })
            .collect(),
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

#[tokio::test]
async fn selected_branch_ids_disambiguate_duplicate_basenames() {
    let (cp, _tmp) = cp().await;
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

    let branch_ids = cp.selected_branch_ids(&[v1, v2]).await.unwrap();

    assert_eq!(
        branch_ids,
        vec![
            (v1, "a/movie.mkv".to_owned()),
            (v2, "b/movie.mkv".to_owned())
        ]
    );
}

#[tokio::test]
async fn selected_branch_ids_reject_duplicate_selected_versions() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/a/movie.mkv",
        "hash-duplicate-active",
        reprobe_payload("h264"),
    )
    .await;

    let err = cp
        .selected_branch_ids(&[version, version])
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
    assert!(err.to_string().contains("appears more than once"));
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
async fn fresh_run_records_retained_active_version_at_phase_zero() {
    let (cp, _tmp) = cp().await;
    let created = cp
        .create_policy_document(
            "empty-phase",
            "policy \"empty phase\" {\n  phase inspect {}\n}\n",
        )
        .await
        .unwrap();
    let selected = seed_version(
        &cp,
        "/lib/fresh/movie.mkv",
        "hash-fresh-0",
        reprobe_payload("h264"),
    )
    .await;
    let selected_snapshot = latest_snapshot(&cp, selected).await;
    let active = advance_chain_tip(&cp, selected, "hash-fresh-1", reprobe_payload("hevc")).await;
    let input = cp
        .create_policy_input_set(file_draft("fresh-active", &[selected_snapshot]))
        .await
        .unwrap();
    let runtimes = crate::workflow::WorkerRuntimeRegistry::new();
    let (initial_plan, prepared) = cp
        .prepare_phase_barrier_run_inputs(created.version.id, input.id, &runtimes)
        .await
        .unwrap();
    assert_eq!(prepared.files[0].version_id, active);
    assert_eq!(
        prepared.files[0].branch_id, "movie",
        "branch identity stays anchored to the selected source path"
    );
    assert!(initial_plan.nodes.iter().all(|node| {
        let TargetRef::FileVersion { id } = node.target else {
            return false;
        };
        id == active
    }));

    let outcome = Box::pin(cp.run_prepared_phase_barrier(
        prepared,
        ComplianceExecutionOptions::default(),
        runtimes,
    ))
    .await
    .unwrap();

    let starts = cp
        .workflow_summaries()
        .file_run_starts_for_job(outcome.job_id)
        .await
        .unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].branch_id, "movie");
    assert_eq!(starts[0].starting_file_version_id, active);
    assert_eq!(starts[0].starting_phase_ordinal, 0);
}

#[tokio::test]
async fn superseded_prepared_fresh_pipeline_rejects_dispatch() {
    let (cp, _tmp) = cp().await;
    let selected = seed_version(
        &cp,
        "/lib/superseded/movie.mkv",
        "hash-superseded-v1",
        reprobe_payload("h264"),
    )
    .await;
    let file = phase_file(&cp, selected, "movie").await;
    let prepared = super::PhaseBarrierRunInputs {
        policy: transcode_hevc_policy(),
        context: voom_plan::PlanningContext::default(),
        base_draft: file_draft("superseded-fresh", std::slice::from_ref(&file.snapshot)),
        files: vec![file],
    };
    let promotion_cp = cp.clone();
    let error = run_prepared_fresh_after_phase_plan(
        &cp,
        prepared,
        ComplianceExecutionOptions::default(),
        crate::workflow::WorkerRuntimeRegistry::new(),
        move |phase_ordinal| {
            let cp = promotion_cp.clone();
            async move {
                assert_eq!(phase_ordinal, 0);
                advance_chain_tip(&cp, selected, "hash-superseded-v2", reprobe_payload("hevc"))
                    .await;
                Ok(())
            }
        },
    )
    .await
    .unwrap_err();
    let current = active_version_id(&cp, selected).await;

    assert_eq!(
        error.source.code(),
        "STALE_IDENTITY_EVIDENCE",
        "{:?}",
        error.source
    );
    assert!(error.source.to_string().contains(&selected.to_string()));
    assert!(error.source.to_string().contains(&current.to_string()));
    let job_id = latest_job_id(&cp).await;
    assert_eq!(error.partial.as_ref().unwrap().job_id, job_id);
    let report = cp.read_compliance_run_report(job_id).await.unwrap();
    assert!(report.phases.is_empty());
    assert!(report.file_phases.is_empty());
    assert_eq!(job_state(&cp, job_id).await, "failed");
    assert_job_opened_then_failed_with_stale_reason(&cp, job_id).await;
    assert_eq!(job_ticket_count(&cp, job_id).await, 0);
    assert_eq!(job_lease_count(&cp, job_id).await, 0);
    assert_eq!(ticket_and_lease_event_count(&cp).await, 0);
    assert_eq!(workflow_effect_counts(&cp, job_id).await, (1, 0, 0));
    assert_eq!(artifact_count(&cp).await, 0);
    let starts = cp
        .workflow_summaries()
        .file_run_starts_for_job(job_id)
        .await
        .unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].starting_file_version_id, selected);
    assert_active_version(&cp, selected, current).await;
}

#[tokio::test]
async fn superseded_prepared_resume_rejects_dispatch_without_mutating_prior_work() {
    let (cp, _tmp) = cp().await;
    let selected = seed_version(
        &cp,
        "/lib/superseded/resume.mkv",
        "hash-superseded-resume-v1",
        reprobe_payload("h264"),
    )
    .await;
    let file = phase_file(&cp, selected, "resume").await;
    let prior_job_id = open_workflow_job(&cp).await;
    record_run_start(&cp, prior_job_id, "resume", selected, 0).await;
    let prior_ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(prior_job_id),
            kind: TicketOperation::new("historical.resume").unwrap(),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(prior_ticket.id, T0)
        .await
        .unwrap();
    cp.fail_job(prior_job_id, "prior failure".to_owned(), T0)
        .await
        .unwrap();
    let inputs = super::PhaseBarrierRunInputs {
        policy: transcode_hevc_policy(),
        context: voom_plan::PlanningContext::default(),
        base_draft: file_draft("superseded-resume", std::slice::from_ref(&file.snapshot)),
        files: vec![file],
    };
    let prepared = cp
        .prepare_resume_phase_barrier_run_inputs(prior_job_id, inputs)
        .await
        .unwrap();
    let prior_event_count = ticket_and_lease_event_count(&cp).await;
    let promotion_cp = cp.clone();
    let error = run_prepared_resume_after_phase_plan(
        &cp,
        prepared,
        ComplianceExecutionOptions::default(),
        crate::workflow::WorkerRuntimeRegistry::new(),
        move |phase_ordinal| {
            let cp = promotion_cp.clone();
            async move {
                assert_eq!(phase_ordinal, 0);
                advance_chain_tip(
                    &cp,
                    selected,
                    "hash-superseded-resume-v2",
                    reprobe_payload("hevc"),
                )
                .await;
                Ok(())
            }
        },
    )
    .await
    .unwrap_err();
    let current = active_version_id(&cp, selected).await;

    assert_eq!(error.source.code(), "STALE_IDENTITY_EVIDENCE");
    assert!(error.source.to_string().contains(&selected.to_string()));
    assert!(error.source.to_string().contains(&current.to_string()));
    let job_id = latest_job_id(&cp).await;
    assert_eq!(error.partial.as_ref().unwrap().job_id, job_id);
    let report = cp.read_compliance_run_report(job_id).await.unwrap();
    assert!(report.phases.is_empty());
    assert!(report.file_phases.is_empty());
    assert_ne!(job_id, prior_job_id);
    assert_eq!(job_state(&cp, job_id).await, "failed");
    assert_job_opened_then_failed_with_stale_reason(&cp, job_id).await;
    assert_eq!(job_state(&cp, prior_job_id).await, "failed");
    assert_eq!(job_ticket_count(&cp, job_id).await, 0);
    assert_eq!(job_ticket_count(&cp, prior_job_id).await, 1);
    assert_eq!(job_lease_count(&cp, job_id).await, 0);
    assert_eq!(ticket_and_lease_event_count(&cp).await, prior_event_count);
    assert_eq!(
        cp.tickets()
            .get(prior_ticket.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );
    assert_eq!(workflow_effect_counts(&cp, job_id).await, (1, 0, 0));
    assert_eq!(artifact_count(&cp).await, 0);
    let starts = cp
        .workflow_summaries()
        .file_run_starts_for_job(job_id)
        .await
        .unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].starting_file_version_id, selected);
    assert_active_version(&cp, selected, current).await;
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

    let result = cp
        .identity()
        .get_active_version_with_snapshot(voom_core::FileAssetId(9_999))
        .await
        .unwrap();

    assert!(result.is_none());
}

/// Build a single-phase compiled policy whose phase carries the given `on_error`
/// strategy. `CompiledPolicy::minimal_for_test` is `#[cfg(test)]`-private to
/// `voom-policy`, so this builds it from public fields instead.
fn policy_with_on_error(
    strategy: Option<voom_policy::ErrorStrategy>,
) -> voom_policy::CompiledPolicy {
    voom_policy::CompiledPolicy {
        policy_name: "guarded".to_owned(),
        slug: "guarded".to_owned(),
        source_hash: "src-hash-onerr".to_owned(),
        schema_version: 2,
        metadata: std::collections::BTreeMap::new(),
        config: voom_policy::CompiledConfig::default(),
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

fn transcode_hevc_policy() -> voom_policy::CompiledPolicy {
    let source = load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap();
    let mut policy = voom_policy::compile_policy(&source).unwrap().policy;
    let voom_policy::CompiledOperation::TranscodeVideo(operation) =
        &mut policy.phases[0].operations[0]
    else {
        panic!("fixture must compile to transcode video");
    };
    operation.resolved_profile = Some(voom_core::TranscodeVideoProfile::default_hevc());
    policy
}

fn policy_with_run_if(trigger: voom_policy::RunIfTrigger) -> voom_policy::CompiledPolicy {
    let mut policy = policy_with_on_error(None);
    policy.phases.insert(
        0,
        voom_policy::CompiledPhase {
            name: "inspect".to_owned(),
            depends_on: Vec::new(),
            run_if: None,
            skip_if: None,
            on_error: None,
            operations: Vec::new(),
        },
    );
    policy.phases[1].run_if = Some(voom_policy::CompiledRunIf {
        trigger,
        phase: "inspect".to_owned(),
    });
    policy.phase_order = vec!["inspect".to_owned(), "normalize".to_owned()];
    policy
}

#[test]
fn reject_unpublished_on_error_rejects_skip() {
    let err = super::reject_unpublished_on_error(&policy_with_on_error(Some(
        voom_policy::ErrorStrategy::Skip,
    )))
    .unwrap_err();
    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(err.to_string().contains("normalize"));
    assert!(err.to_string().contains("skip"));
}

#[test]
fn reject_unpublished_on_error_allows_published_strategies_and_unset() {
    assert!(
        super::reject_unpublished_on_error(&policy_with_on_error(Some(
            voom_policy::ErrorStrategy::Continue
        )))
        .is_ok()
    );
    assert!(
        super::reject_unpublished_on_error(&policy_with_on_error(Some(
            voom_policy::ErrorStrategy::Abort
        )))
        .is_ok()
    );
    assert!(super::reject_unpublished_on_error(&policy_with_on_error(None)).is_ok());
}

#[test]
fn continued_disposition_blocks_failed_nodes_and_preserves_successful_nodes() {
    let failed = super::continued_disposition(
        &super::Disposition::Planned {
            node_ids: vec!["failed".to_owned()],
        },
        &[TicketState::Failed],
    )
    .unwrap();
    let succeeded = super::continued_disposition(
        &super::Disposition::Planned {
            node_ids: vec!["succeeded".to_owned()],
        },
        &[TicketState::Succeeded],
    )
    .unwrap();

    let super::Disposition::Blocked = failed else {
        panic!("failed node must be blocked");
    };
    let super::Disposition::Planned { node_ids } = succeeded else {
        panic!("successful node must remain planned");
    };
    assert_eq!(node_ids, ["succeeded"]);
}

#[test]
fn continued_disposition_rejects_missing_or_non_terminal_ticket_state() {
    let disposition = super::Disposition::Planned {
        node_ids: vec!["node".to_owned()],
    };

    let missing = super::continued_disposition(&disposition, &[]).unwrap_err();
    let ready = super::continued_disposition(&disposition, &[TicketState::Ready]).unwrap_err();

    assert!(missing.to_string().contains("has no tickets"));
    assert!(ready.to_string().contains("non-terminal"));
}

async fn open_workflow_job(cp: &crate::ControlPlane) -> JobId {
    cp.open_job(NewJob {
        kind: "synthetic.workflow".to_owned(),
        priority: 0,
        created_at: T0,
    })
    .await
    .unwrap()
    .id
}

async fn latest_job_id(cp: &crate::ControlPlane) -> JobId {
    let id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    JobId(u64::try_from(id).unwrap())
}

async fn job_ticket_count(cp: &crate::ControlPlane, job_id: JobId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ?")
        .bind(i64::try_from(job_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap()
}

async fn job_lease_count(cp: &crate::ControlPlane, job_id: JobId) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM leases l \
         JOIN tickets t ON t.id = l.ticket_id WHERE t.job_id = ?",
    )
    .bind(i64::try_from(job_id.0).unwrap())
    .fetch_one(&cp.pool)
    .await
    .unwrap()
}

async fn ticket_and_lease_event_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM events \
         WHERE subject_type IN ('ticket', 'lease')",
    )
    .fetch_one(&cp.pool)
    .await
    .unwrap()
}

async fn assert_job_opened_then_failed_with_stale_reason(cp: &crate::ControlPlane, job_id: JobId) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind, payload FROM events \
         WHERE subject_type = 'job' AND subject_id = ? ORDER BY event_id",
    )
    .bind(i64::try_from(job_id.0).unwrap())
    .fetch_all(&cp.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "job.opened");
    assert_eq!(rows[1].0, "job.failed");
    assert!(rows[1].1.contains("stale identity evidence"));
}

async fn artifact_count(cp: &crate::ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM artifact_handles")
        .fetch_one(&cp.pool)
        .await
        .unwrap()
}

async fn assert_active_version(
    cp: &crate::ControlPlane,
    lineage_version: FileVersionId,
    expected_active: FileVersionId,
) {
    assert_eq!(
        active_version_id(cp, lineage_version).await,
        expected_active
    );
}

async fn active_version_id(
    cp: &crate::ControlPlane,
    lineage_version: FileVersionId,
) -> FileVersionId {
    let asset_id = cp
        .identity()
        .get_file_version(lineage_version)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let active = cp
        .identity()
        .get_active_version_with_snapshot(asset_id)
        .await
        .unwrap()
        .unwrap()
        .0;
    active.id
}

async fn workflow_effect_counts(cp: &crate::ControlPlane, job_id: JobId) -> (i64, i64, i64) {
    let summaries = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_summaries WHERE job_id = ?")
        .bind(i64::try_from(job_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let phases =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflow_phase_summaries WHERE job_id = ?")
            .bind(i64::try_from(job_id.0).unwrap())
            .fetch_one(&cp.pool)
            .await
            .unwrap();
    let file_phases =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflow_file_phase_summaries WHERE job_id = ?")
            .bind(i64::try_from(job_id.0).unwrap())
            .fetch_one(&cp.pool)
            .await
            .unwrap();
    (summaries, phases, file_phases)
}

async fn record_run_start(
    cp: &crate::ControlPlane,
    job_id: JobId,
    branch_id: &str,
    version_id: FileVersionId,
    phase_ordinal: u32,
) {
    record_run_starts(
        cp,
        job_id,
        vec![NewFileRunStart {
            branch_id: branch_id.to_owned(),
            starting_file_version_id: version_id,
            starting_phase_ordinal: phase_ordinal,
        }],
    )
    .await;
}

async fn record_run_starts(cp: &crate::ControlPlane, job_id: JobId, starts: Vec<NewFileRunStart>) {
    cp.workflow_summaries()
        .insert_file_run_starts(job_id, starts.clone())
        .await
        .unwrap();
    cp.workflow_summaries()
        .insert_file_window(
            job_id,
            4,
            starts
                .iter()
                .enumerate()
                .map(|(input_ordinal, start)| NewFileProgress {
                    branch_id: start.branch_id.clone(),
                    input_ordinal: u32::try_from(input_ordinal).unwrap(),
                    next_phase_ordinal: start.starting_phase_ordinal,
                })
                .collect(),
            T0,
        )
        .await
        .unwrap();
    for _ in &starts {
        cp.workflow_summaries()
            .admit_next_file(job_id, T0)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn phase_file(
    cp: &crate::ControlPlane,
    version_id: FileVersionId,
    branch_id: &str,
) -> PhaseFile {
    let version = cp
        .identity()
        .get_file_version(version_id)
        .await
        .unwrap()
        .unwrap();
    let (active, snapshot) = cp
        .identity()
        .get_active_version_with_snapshot(version.file_asset_id)
        .await
        .unwrap()
        .unwrap();
    PhaseFile {
        asset_id: version.file_asset_id,
        version_id: active.id,
        snapshot,
        branch_id: branch_id.to_owned(),
        ordinal: 1,
        resume_ordinal: 0,
        phase_history: BTreeMap::new(),
    }
}

/// Write a prior-job `(file, phase)` row. For a `Committed` outcome the DB CHECK
/// requires the produced version, its live location, and its reprobe snapshot, so
/// resolve all three from `produced_version`; `Skipped`/`Blocked` carry none.
async fn record_file_phase(
    cp: &crate::ControlPlane,
    job_id: JobId,
    phase_ordinal: u32,
    branch_id: &str,
    outcome: FilePhaseOutcome,
    produced_version: Option<FileVersionId>,
) {
    let produced = if outcome == FilePhaseOutcome::Committed {
        let version = produced_version.unwrap();
        let location = cp
            .identity()
            .list_live_file_locations_by_version(version)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let snapshot = latest_snapshot(cp, version).await;
        (Some(version), Some(location.id), Some(snapshot.id))
    } else {
        (None, None, None)
    };
    cp.workflow_summaries()
        .upsert_file_phase_summary(
            NewFilePhaseSummary {
                job_id,
                phase_ordinal,
                branch_id: branch_id.to_owned(),
                ticket_ids: Vec::new(),
                produced_file_version_id: produced.0,
                produced_file_location_id: produced.1,
                artifact_handle_id: None,
                artifact_verification_id: None,
                reprobe_snapshot_id: produced.2,
                outcome,
            },
            T0,
        )
        .await
        .unwrap();
    let progress = cp
        .workflow_summaries()
        .file_progress(job_id, branch_id)
        .await
        .unwrap();
    if progress.is_some_and(|row| row.next_phase_ordinal == phase_ordinal) {
        assert!(
            cp.workflow_summaries()
                .advance_file_progress(job_id, branch_id, phase_ordinal, phase_ordinal + 1)
                .await
                .unwrap()
        );
    }
}

/// Append a transcode-produced version to `parent`'s asset, give it a live
/// location and a recorded snapshot, and return the new version id. The live
/// location is required because the resume backfill resolves `ProducedRefs`,
/// which reads `list_live_file_locations_by_version`.
async fn advance_chain_tip(
    cp: &crate::ControlPlane,
    parent: FileVersionId,
    hash: &str,
    payload: Value,
) -> FileVersionId {
    let asset_id = cp
        .identity()
        .get_file_version(parent)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let version = cp
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: hash.to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Transcode,
            produced_from_version_id: Some(parent),
            created_at: T0,
        })
        .await
        .unwrap();
    cp.create_file_location(NewFileLocation {
        file_version_id: version.id,
        kind: FileLocationKind::LocalPath,
        value: format!("/lib/produced/{hash}.mkv"),
        proof: None,
        observed_at: T0,
    })
    .await
    .unwrap();
    cp.record_media_snapshot(version.id, None, payload, T0)
        .await
        .unwrap();
    version.id
}

#[tokio::test]
async fn reconcile_resume_resumes_after_highest_recorded_phase() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/r/movie.mkv", "hash-r1", reprobe_payload("h264")).await;
    record_run_start(&cp, prior, "movie", v, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Committed, Some(v)).await;
    record_file_phase(&cp, prior, 1, "movie", FilePhaseOutcome::Committed, Some(v)).await;

    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, v, "movie").await], 4)
        .await
        .unwrap();

    assert_eq!(prepared.files.len(), 1);
    assert_eq!(
        prepared.files[0].resume_ordinal, 2,
        "highest recorded (1) + 1"
    );
    assert_eq!(
        prepared.seeds.len(),
        2,
        "terminalization retains both committed phase rows"
    );
    assert_eq!(prepared.run_starts[0].starting_phase_ordinal, 2);
}

#[tokio::test]
async fn phase_run_gate_evaluates_completed_and_modified_per_file() {
    let (cp, _tmp) = cp().await;
    let first = seed_version(
        &cp,
        "/lib/gates/first.mkv",
        "hash-gate-first",
        reprobe_payload("h264"),
    )
    .await;
    let second = seed_version(
        &cp,
        "/lib/gates/second.mkv",
        "hash-gate-second",
        reprobe_payload("h264"),
    )
    .await;
    let mut committed = phase_file(&cp, first, "first").await;
    committed
        .phase_history
        .insert(0, FilePhaseOutcome::Committed);
    let mut skipped = phase_file(&cp, second, "second").await;
    skipped.phase_history.insert(0, FilePhaseOutcome::Skipped);
    let files = vec![committed, skipped];

    assert_eq!(
        super::phase_gate_admission(
            &policy_with_run_if(voom_policy::RunIfTrigger::Completed),
            "normalize",
            &files,
        )
        .unwrap(),
        vec![true, true]
    );
    assert_eq!(
        super::phase_gate_admission(
            &policy_with_run_if(voom_policy::RunIfTrigger::Modified),
            "normalize",
            &files,
        )
        .unwrap(),
        vec![true, false]
    );
}

#[tokio::test]
async fn phase_planning_applies_each_files_modified_gate_decision() {
    let (cp, _tmp) = cp().await;
    let first = seed_version(
        &cp,
        "/lib/gate-plan/first.mkv",
        "hash-gate-plan-first",
        reprobe_payload("h264"),
    )
    .await;
    let second = seed_version(
        &cp,
        "/lib/gate-plan/second.mkv",
        "hash-gate-plan-second",
        reprobe_payload("h264"),
    )
    .await;
    let mut committed = phase_file(&cp, first, "first").await;
    committed
        .phase_history
        .insert(0, FilePhaseOutcome::Committed);
    let mut skipped = phase_file(&cp, second, "second").await;
    skipped.phase_history.insert(0, FilePhaseOutcome::Skipped);
    let files = vec![committed, skipped];
    let mut policy = policy_with_run_if(voom_policy::RunIfTrigger::Modified);
    policy.phases[1].operations = vec![voom_policy::CompiledOperation::ClearTags(
        voom_policy::compiled::CompiledClearTagsOperation {},
    )];
    let phase_loop = super::PhaseLoop::new(
        &cp,
        super::PhaseLoopInputs {
            job_id: JobId(1),
            policy,
            context: voom_plan::PlanningContext::default(),
            base_draft: file_draft(
                "gate-planning",
                &files
                    .iter()
                    .map(|file| file.snapshot.clone())
                    .collect::<Vec<_>>(),
            ),
            files: files.clone(),
            seed_file_phases: Vec::new(),
            options: ComplianceExecutionOptions::default(),
            runtimes: crate::workflow::WorkerRuntimeRegistry::new(),
        },
    );

    let planned = phase_loop
        .plan_phase_for_files("normalize", &files)
        .unwrap();

    assert!(
        planned.plan.nodes.iter().all(|node| {
            let TargetRef::FileVersion { id } = node.target else {
                return false;
            };
            id == first
        }),
        "only the modified file should have phase nodes"
    );
    let super::Disposition::Skipped = &planned.dispositions[1] else {
        panic!("the unmodified file must be classified as skipped");
    };
}

#[tokio::test]
async fn phase_planning_reports_zero_checks_when_no_files_pass_the_gate() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/gate-plan/unmodified.mkv",
        "hash-gate-plan-unmodified",
        reprobe_payload("h264"),
    )
    .await;
    let mut file = phase_file(&cp, version, "unmodified").await;
    file.phase_history.insert(0, FilePhaseOutcome::Skipped);
    let files = vec![file];
    let mut policy = policy_with_run_if(voom_policy::RunIfTrigger::Modified);
    policy.phases[1].operations = vec![voom_policy::CompiledOperation::ClearTags(
        voom_policy::compiled::CompiledClearTagsOperation {},
    )];
    let phase_loop = super::PhaseLoop::new(
        &cp,
        super::PhaseLoopInputs {
            job_id: JobId(1),
            policy,
            context: voom_plan::PlanningContext::default(),
            base_draft: file_draft("gate-planning-none", &[files[0].snapshot.clone()]),
            files: files.clone(),
            seed_file_phases: Vec::new(),
            options: ComplianceExecutionOptions::default(),
            runtimes: crate::workflow::WorkerRuntimeRegistry::new(),
        },
    );

    let planned = phase_loop
        .plan_phase_for_files("normalize", &files)
        .unwrap();

    assert!(planned.plan.nodes.is_empty());
    assert_eq!(planned.report.summary.total_check_count, 0);
    let super::Disposition::Skipped = &planned.dispositions[0] else {
        panic!("the unmodified file must be classified as skipped");
    };
}

#[tokio::test]
async fn phase_run_gate_fails_loud_when_predecessor_history_is_missing() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/gates/missing.mkv",
        "hash-gate-missing",
        reprobe_payload("h264"),
    )
    .await;
    let files = vec![phase_file(&cp, version, "missing").await];

    let error = super::phase_gate_admission(
        &policy_with_run_if(voom_policy::RunIfTrigger::Completed),
        "normalize",
        &files,
    )
    .unwrap_err();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(error.to_string().contains("outcome is missing"));
    assert!(error.to_string().contains("missing"));
}

#[tokio::test]
async fn phase_finalization_records_skipped_survivors_gate_history() {
    let (cp, _tmp) = cp().await;
    let skipped_version = seed_version(
        &cp,
        "/lib/gate-finalize/skipped.mkv",
        "hash-gate-finalize-skipped",
        reprobe_payload("h264"),
    )
    .await;
    let committed_parent = seed_version(
        &cp,
        "/lib/gate-finalize/committed.mkv",
        "hash-gate-finalize-parent",
        reprobe_payload("h264"),
    )
    .await;
    let mut skipped = phase_file(&cp, skipped_version, "skipped").await;
    skipped.ordinal = 0;
    let mut committed = phase_file(&cp, committed_parent, "committed").await;
    committed.ordinal = 1;
    let mut files = vec![skipped, committed];
    let starts = super::run_starts_for_files(&files);
    let (job, _) = cp
        .open_sliding_file_job(&starts, Vec::new(), Vec::new(), &files, 2)
        .await
        .unwrap();
    cp.workflow_summaries()
        .admit_next_file(job.id, T0)
        .await
        .unwrap()
        .unwrap();
    cp.workflow_summaries()
        .admit_next_file(job.id, T0)
        .await
        .unwrap()
        .unwrap();

    cp.finalize_phase(
        job.id,
        0,
        &mut files,
        &[
            super::Disposition::Skipped,
            super::Disposition::Planned {
                node_ids: vec!["committed-node".to_owned()],
            },
        ],
    )
    .await
    .unwrap();

    assert_eq!(
        files[0].phase_history.get(&0),
        Some(&FilePhaseOutcome::Skipped)
    );
    assert_eq!(files[1].version_id, committed_parent);
    assert_eq!(
        files[1].phase_history.get(&0),
        Some(&FilePhaseOutcome::Skipped)
    );
}

#[tokio::test]
async fn admission_failure_drains_the_already_admitted_pipeline() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "admission-drain",
            "policy \"admission drain\" {\n  phase inspect {}\n}\n",
        )
        .await
        .unwrap();
    let first = seed_version(
        &cp,
        "/lib/admission/first.mkv",
        "hash-admission-first",
        reprobe_payload("h264"),
    )
    .await;
    let second = seed_version(
        &cp,
        "/lib/admission/second.mkv",
        "hash-admission-second",
        reprobe_payload("h264"),
    )
    .await;
    let snapshots = vec![
        latest_snapshot(&cp, first).await,
        latest_snapshot(&cp, second).await,
    ];
    let input = cp
        .create_policy_input_set(file_draft("admission-drain", &snapshots))
        .await
        .unwrap();
    let runtimes = crate::workflow::WorkerRuntimeRegistry::new();
    let (_, prepared) = cp
        .prepare_phase_barrier_run_inputs(policy.version.id, input.id, &runtimes)
        .await
        .unwrap();
    let starts = super::run_starts_for_files(&prepared.files);
    let options = ComplianceExecutionOptions {
        max_in_flight_files: 2,
        ..ComplianceExecutionOptions::default()
    };
    let (job, _) = cp
        .open_sliding_file_job(&starts, Vec::new(), Vec::new(), &prepared.files, 2)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_second_admission BEFORE UPDATE ON workflow_file_progress \
         WHEN OLD.input_ordinal = 2 \
         BEGIN SELECT RAISE(ABORT, 'forced second admission failure'); END",
    )
    .execute(&cp.pool)
    .await
    .unwrap();

    let result = cp
        .run_sliding_file_window(super::PhaseLoopInputs {
            job_id: job.id,
            policy: prepared.policy,
            context: prepared.context,
            base_draft: prepared.base_draft,
            files: prepared.files,
            seed_file_phases: Vec::new(),
            options,
            runtimes,
        })
        .await;
    let error = cp
        .finish_phase_barrier_job(job.id, result)
        .await
        .unwrap_err();

    assert_eq!(error.source.code(), "DB_UNREACHABLE");
    let progress = cp
        .workflow_summaries()
        .file_progress_for_job(job.id)
        .await
        .unwrap();
    assert_eq!(
        progress
            .iter()
            .map(|row| (row.input_ordinal, row.state.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "terminal"), (2, "pending")]
    );
    assert_eq!(
        cp.workflow_summaries()
            .file_phases_for_job(job.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(job_state(&cp, job.id).await, "failed");
}

#[tokio::test]
async fn cancelled_sliding_job_admits_no_pending_files() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/cancelled/movie.mkv",
        "hash-cancelled",
        reprobe_payload("h264"),
    )
    .await;
    let file = phase_file(&cp, version, "movie").await;
    let policy = cp
        .create_policy_document(
            "cancelled-window",
            "policy \"cancelled window\" {\n  phase inspect {}\n}\n",
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(file_draft(
            "cancelled-window",
            std::slice::from_ref(&file.snapshot),
        ))
        .await
        .unwrap();
    let runtimes = crate::workflow::WorkerRuntimeRegistry::new();
    let (_, prepared) = cp
        .prepare_phase_barrier_run_inputs(policy.version.id, input.id, &runtimes)
        .await
        .unwrap();
    let starts = super::run_starts_for_files(&prepared.files);
    let options = ComplianceExecutionOptions::default();
    let (job, _) = cp
        .open_sliding_file_job(&starts, Vec::new(), Vec::new(), &prepared.files, 4)
        .await
        .unwrap();
    cp.cancel_job(job.id, "operator cancelled sliding run".to_owned(), T0)
        .await
        .unwrap();

    let result = cp
        .run_sliding_file_window(super::PhaseLoopInputs {
            job_id: job.id,
            policy: prepared.policy,
            context: prepared.context,
            base_draft: prepared.base_draft,
            files: prepared.files,
            seed_file_phases: Vec::new(),
            options,
            runtimes,
        })
        .await;
    let error = cp
        .finish_phase_barrier_job(job.id, result)
        .await
        .unwrap_err();

    assert_eq!(error.source.code(), "USER_CANCELLATION");
    let progress = cp
        .workflow_summaries()
        .file_progress_for_job(job.id)
        .await
        .unwrap();
    assert_eq!(progress[0].state.as_str(), "pending");
    assert_eq!(job_state(&cp, job.id).await, "cancelled");
    assert!(
        cp.workflow_summaries()
            .file_phases_for_job(job.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn resume_carries_phase_history_across_repeated_new_jobs() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let version = seed_version(
        &cp,
        "/lib/history-carry/movie.mkv",
        "hash-history-carry",
        reprobe_payload("h264"),
    )
    .await;
    record_run_start(&cp, prior, "movie", version, 0).await;
    record_file_phase(
        &cp,
        prior,
        0,
        "movie",
        FilePhaseOutcome::Committed,
        Some(version),
    )
    .await;
    record_file_phase(&cp, prior, 1, "movie", FilePhaseOutcome::Skipped, None).await;

    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 4)
        .await
        .unwrap();
    assert_eq!(
        prepared.files[0].phase_history,
        BTreeMap::from([
            (0, FilePhaseOutcome::Committed),
            (1, FilePhaseOutcome::Skipped),
        ])
    );
    assert_eq!(
        prepared.history,
        vec![
            NewFileRunHistory {
                branch_id: "movie".to_owned(),
                phase_ordinal: 0,
                outcome: FilePhaseOutcome::Committed,
            },
            NewFileRunHistory {
                branch_id: "movie".to_owned(),
                phase_ordinal: 1,
                outcome: FilePhaseOutcome::Skipped,
            },
        ]
    );

    let (next_job, _) = cp
        .open_sliding_file_job(
            &prepared.run_starts,
            prepared.history.clone(),
            prepared.seeds,
            &prepared.files,
            4,
        )
        .await
        .unwrap();
    let repeated = cp
        .prepare_resume(
            next_job.id,
            vec![phase_file(&cp, version, "movie").await],
            4,
        )
        .await
        .unwrap();

    assert_eq!(
        repeated.files[0].phase_history,
        prepared.files[0].phase_history
    );
    assert_eq!(repeated.history, prepared.history);
    assert_eq!(repeated.files[0].resume_ordinal, 2);
}

#[tokio::test]
async fn reconcile_resume_keeps_blocked_file_until_terminalization_replays() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/b/movie.mkv", "hash-b1", reprobe_payload("h264")).await;
    record_run_start(&cp, prior, "movie", v, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Blocked, None).await;

    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, v, "movie").await], 4)
        .await
        .unwrap();

    assert_eq!(
        prepared.files[0].resume_ordinal, 4,
        "blocked phase work is complete but terminalization is not"
    );
    assert_eq!(prepared.seeds.len(), 1);
    assert_eq!(prepared.seeds[0].outcome, FilePhaseOutcome::Blocked);
    assert_eq!(prepared.run_starts[0].starting_phase_ordinal, 4);
}

#[tokio::test]
async fn reconcile_resume_keeps_fully_recorded_file_until_terminalization_replays() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/c/movie.mkv", "hash-c1", reprobe_payload("h264")).await;
    record_run_start(&cp, prior, "movie", v, 0).await;
    for ordinal in 0..2 {
        record_file_phase(
            &cp,
            prior,
            ordinal,
            "movie",
            FilePhaseOutcome::Committed,
            Some(v),
        )
        .await;
    }
    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, v, "movie").await], 2)
        .await
        .unwrap();
    assert_eq!(prepared.files[0].resume_ordinal, 2);
    assert_eq!(prepared.run_starts[0].starting_phase_ordinal, 2);
}

#[tokio::test]
async fn resume_validates_window_cursor_and_terminalization_state() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/cursor/missing.mkv",
        "hash-cursor-missing",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "missing", version, 0).await;
    sqlx::query("DELETE FROM workflow_file_progress WHERE job_id = ?")
        .bind(i64::try_from(prior.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();
    assert_resume_incomplete(
        &cp.prepare_resume(prior, vec![phase_file(&cp, version, "missing").await], 2)
            .await
            .unwrap_err(),
    );

    let (cp, _tmp) = crate::cases::cp().await;
    let version = seed_version(
        &cp,
        "/lib/cursor/disagree.mkv",
        "hash-cursor-disagree",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "disagree", version, 0).await;
    sqlx::query("UPDATE workflow_file_progress SET next_phase_ordinal = 1 WHERE job_id = ?")
        .bind(i64::try_from(prior.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();
    assert_resume_incomplete(
        &cp.prepare_resume(prior, vec![phase_file(&cp, version, "disagree").await], 2)
            .await
            .unwrap_err(),
    );

    let (cp, _tmp) = crate::cases::cp().await;
    let version = seed_version(
        &cp,
        "/lib/cursor/terminalizing.mkv",
        "hash-cursor-terminalizing",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "terminalizing", version, 0).await;
    cp.workflow_summaries()
        .begin_file_terminalization(prior, "terminalizing")
        .await
        .unwrap();
    assert_resume_incomplete(
        &cp.prepare_resume(
            prior,
            vec![phase_file(&cp, version, "terminalizing").await],
            2,
        )
        .await
        .unwrap_err(),
    );
}

#[tokio::test]
async fn terminalizing_completed_branch_replays_but_terminal_branch_does_not() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/terminalize/movie.mkv",
        "hash-terminalize",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", version, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Blocked, None).await;
    cp.workflow_summaries()
        .begin_file_terminalization(prior, "movie")
        .await
        .unwrap();

    let replay = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 2)
        .await
        .unwrap();
    assert_eq!(replay.files[0].resume_ordinal, 2);

    cp.workflow_summaries()
        .mark_file_terminal(prior, "movie", T0)
        .await
        .unwrap();
    let completed = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 2)
        .await
        .unwrap();
    assert!(completed.files.is_empty());
}

#[tokio::test]
async fn terminalizing_committed_branch_replays_through_resume_runner() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/terminalize/committed.mkv",
        "hash-terminalize-committed",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "committed", version, 0).await;
    record_file_phase(
        &cp,
        prior,
        0,
        "committed",
        FilePhaseOutcome::Committed,
        Some(version),
    )
    .await;
    cp.workflow_summaries()
        .begin_file_terminalization(prior, "committed")
        .await
        .unwrap();
    let file = phase_file(&cp, version, "committed").await;
    let snapshot = file.snapshot.clone();
    let policy = policy_with_on_error(None);
    let preparation = cp.prepare_resume(prior, vec![file], 1).await.unwrap();

    let outcome = cp
        .run_prepared_resume_phase_barrier(
            super::PreparedResumeRunInputs {
                policy,
                context: voom_plan::PlanningContext::default(),
                base_draft: file_draft("terminalization-replay", &[snapshot]),
                preparation,
            },
            ComplianceExecutionOptions::default(),
            crate::workflow::WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.file_phases.len(), 1);
    assert_eq!(outcome.file_phases[0].outcome, FilePhaseOutcome::Committed);
    let progress = cp
        .workflow_summaries()
        .file_progress(outcome.job_id, "committed")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        progress.state,
        voom_store::repo::workflow_summaries::FileProgressState::Terminal
    );
}

#[tokio::test]
async fn phase_complete_terminalizing_resume_rejects_unrelated_chain_tip() {
    let (cp, _tmp) = cp().await;
    let original = seed_version(
        &cp,
        "/lib/terminalize/unrelated.mkv",
        "hash-terminalize-original",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "unrelated", original, 0).await;
    record_file_phase(
        &cp,
        prior,
        0,
        "unrelated",
        FilePhaseOutcome::Committed,
        Some(original),
    )
    .await;
    cp.workflow_summaries()
        .begin_file_terminalization(prior, "unrelated")
        .await
        .unwrap();
    let unrelated = advance_chain_tip(
        &cp,
        original,
        "hash-terminalize-unrelated",
        reprobe_payload("av1"),
    )
    .await;

    let error = cp
        .prepare_resume(
            prior,
            vec![phase_file(&cp, unrelated, "unrelated").await],
            1,
        )
        .await
        .unwrap_err();

    assert_resume_incomplete(&error);
    assert!(
        error
            .to_string()
            .contains("phase-complete branch unrelated")
    );
}

#[tokio::test]
async fn phase_outcome_matches_completion_rows_to_entered_branches() {
    let (cp, _tmp) = cp().await;
    let seeded_version = seed_version(
        &cp,
        "/lib/report/seed.mkv",
        "hash-report-seed",
        reprobe_payload("h264"),
    )
    .await;
    let completed_version = seed_version(
        &cp,
        "/lib/report/completed.mkv",
        "hash-report-completed",
        reprobe_payload("h264"),
    )
    .await;
    let failed_version = seed_version(
        &cp,
        "/lib/report/failed.mkv",
        "hash-report-failed",
        reprobe_payload("h264"),
    )
    .await;
    let mut files = vec![
        phase_file(&cp, seeded_version, "seed").await,
        phase_file(&cp, completed_version, "completed").await,
        phase_file(&cp, failed_version, "failed").await,
    ];
    for (ordinal, file) in files.iter_mut().enumerate() {
        file.ordinal = u32::try_from(ordinal).unwrap();
    }
    let starts = super::run_starts_for_files(&files);
    let (job, _) = cp
        .open_sliding_file_job(&starts, Vec::new(), Vec::new(), &files, 4)
        .await
        .unwrap();
    for _ in &files {
        cp.workflow_summaries()
            .admit_next_file(job.id, T0)
            .await
            .unwrap()
            .unwrap();
    }
    record_file_phase(
        &cp,
        job.id,
        0,
        "seed",
        FilePhaseOutcome::Committed,
        Some(seeded_version),
    )
    .await;
    record_file_phase(
        &cp,
        job.id,
        0,
        "completed",
        FilePhaseOutcome::Committed,
        Some(completed_version),
    )
    .await;
    for file in files.iter().filter(|file| file.branch_id != "seed") {
        cp.workflow_summaries()
            .upsert_file_phase_entry(
                NewFilePhaseEntry {
                    job_id: job.id,
                    phase_ordinal: 0,
                    branch_id: file.branch_id.clone(),
                    media_snapshot_id: file.snapshot.id,
                    gate_admitted: true,
                },
                T0,
            )
            .await
            .unwrap();
    }
    let inputs = super::PhaseLoopInputs {
        job_id: job.id,
        policy: policy_with_on_error(None),
        context: voom_plan::PlanningContext::default(),
        base_draft: file_draft(
            "branch-keyed-outcomes",
            &files
                .iter()
                .map(|file| file.snapshot.clone())
                .collect::<Vec<_>>(),
        ),
        files,
        seed_file_phases: Vec::new(),
        options: ComplianceExecutionOptions::default(),
        runtimes: crate::workflow::WorkerRuntimeRegistry::new(),
    };

    let (phases, _) = cp.persist_sliding_phase_summaries(&inputs).await.unwrap();

    assert_eq!(phases.len(), 1);
    assert_eq!(phases[0].outcome, PhaseOutcome::PartiallyCommitted);
}

#[tokio::test]
async fn reconcile_resume_rejects_unproven_committed_tip_without_row() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v0 = seed_version(&cp, "/lib/d/movie.mkv", "hash-d0", reprobe_payload("h264")).await;
    record_run_start(&cp, prior, "movie", v0, 0).await;
    record_file_phase(
        &cp,
        prior,
        0,
        "movie",
        FilePhaseOutcome::Committed,
        Some(v0),
    )
    .await;
    let v1 = advance_chain_tip(&cp, v0, "hash-d1", reprobe_payload("hevc")).await;

    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, v0, "movie").await], 4)
        .await
        .unwrap_err();

    assert_resume_incomplete(&error);
    assert!(
        error
            .to_string()
            .contains("without committed prior-job evidence")
    );
    assert_eq!(active_version_id(&cp, v0).await, v1);
}

#[tokio::test]
async fn reconcile_resume_zero_rows_rejects_unproven_advanced_tip() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await; // no rows at all under this job
    let v0 = seed_version(&cp, "/lib/e/movie.mkv", "hash-e0", reprobe_payload("h264")).await;
    record_run_start(&cp, prior, "movie", v0, 0).await;
    let _v1 = advance_chain_tip(&cp, v0, "hash-e1", reprobe_payload("hevc")).await;

    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, v0, "movie").await], 4)
        .await
        .unwrap_err();

    assert_resume_incomplete(&error);
    assert!(
        error
            .to_string()
            .contains("without committed prior-job evidence")
    );
}

fn assert_resume_incomplete(error: &voom_core::VoomError) {
    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(
        error.to_string().contains("resume state is incomplete"),
        "unexpected resume error: {error}"
    );
}

#[tokio::test]
async fn resume_uses_durable_start_instead_of_historical_input_selection() {
    let (cp, _tmp) = cp().await;
    let v0 = seed_version(
        &cp,
        "/lib/history/movie.mkv",
        "hash-h0",
        reprobe_payload("h264"),
    )
    .await;
    let v1 = advance_chain_tip(&cp, v0, "hash-h1", reprobe_payload("hevc")).await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", v1, 0).await;

    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, v0, "movie").await], 4)
        .await
        .unwrap();

    assert!(prepared.seeds.is_empty());
    assert_eq!(prepared.files[0].version_id, v1);
    assert_eq!(prepared.files[0].resume_ordinal, 0);
}

#[tokio::test]
async fn empty_resumed_run_retains_nonzero_starting_ordinal() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/chained/movie.mkv",
        "hash-chain",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", version, 2).await;

    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 4)
        .await
        .unwrap();

    assert!(prepared.seeds.is_empty());
    assert_eq!(prepared.files[0].resume_ordinal, 2);
    assert_eq!(prepared.run_starts[0].starting_phase_ordinal, 2);
}

#[tokio::test]
async fn resume_rejects_pre_migration_file_job_without_opening_another_job() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/legacy/movie.mkv",
        "hash-legacy",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;

    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 4)
        .await
        .unwrap_err();

    assert_resume_incomplete(&error);
    let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(job_count, 1);
}

#[tokio::test]
async fn resume_rejects_mismatched_current_and_prior_branch_sets() {
    let (cp, _tmp) = cp().await;
    let a = seed_version(&cp, "/lib/set/a.mkv", "hash-set-a", reprobe_payload("h264")).await;
    let b = seed_version(&cp, "/lib/set/b.mkv", "hash-set-b", reprobe_payload("h264")).await;
    let c = seed_version(&cp, "/lib/set/c.mkv", "hash-set-c", reprobe_payload("h264")).await;
    let prior = open_workflow_job(&cp).await;
    record_run_starts(
        &cp,
        prior,
        vec![
            NewFileRunStart {
                branch_id: "a".to_owned(),
                starting_file_version_id: a,
                starting_phase_ordinal: 0,
            },
            NewFileRunStart {
                branch_id: "c".to_owned(),
                starting_file_version_id: c,
                starting_phase_ordinal: 0,
            },
        ],
    )
    .await;

    let error = cp
        .prepare_resume(
            prior,
            vec![phase_file(&cp, a, "a").await, phase_file(&cp, b, "b").await],
            4,
        )
        .await
        .unwrap_err();

    assert_resume_incomplete(&error);
}

#[tokio::test]
async fn resume_rejects_gapped_and_out_of_range_phase_rows() {
    for (start, row, phase_count) in [(0, 1, 4), (0, 2, 2), (2, 0, 4)] {
        let (cp, _tmp) = cp().await;
        let version = seed_version(
            &cp,
            &format!("/lib/shape/{start}-{row}.mkv"),
            &format!("hash-shape-{start}-{row}"),
            reprobe_payload("h264"),
        )
        .await;
        let prior = open_workflow_job(&cp).await;
        record_run_start(&cp, prior, "movie", version, start).await;
        record_file_phase(&cp, prior, row, "movie", FilePhaseOutcome::Skipped, None).await;

        let error = cp
            .prepare_resume(
                prior,
                vec![phase_file(&cp, version, "movie").await],
                phase_count,
            )
            .await
            .unwrap_err();
        assert_resume_incomplete(&error);
    }
}

#[tokio::test]
async fn resume_rejects_rows_after_blocked_and_invalid_seed_shape() {
    let (cp, _tmp) = cp().await;
    let version = seed_version(
        &cp,
        "/lib/shape/blocked.mkv",
        "hash-blocked",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", version, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Blocked, None).await;
    record_file_phase(&cp, prior, 1, "movie", FilePhaseOutcome::Skipped, None).await;
    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 4)
        .await
        .unwrap_err();
    assert_resume_incomplete(&error);

    let (cp, _tmp) = crate::cases::cp().await;
    let version = seed_version(
        &cp,
        "/lib/shape/seed.mkv",
        "hash-seed",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", version, 2).await;
    record_file_phase(
        &cp,
        prior,
        1,
        "movie",
        FilePhaseOutcome::Committed,
        Some(version),
    )
    .await;
    sqlx::query(
        "UPDATE workflow_file_phase_summaries SET ticket_ids = '[1]' \
         WHERE job_id = ? AND branch_id = 'movie'",
    )
    .bind(i64::try_from(prior.0).unwrap())
    .execute(&cp.pool)
    .await
    .unwrap();
    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, version, "movie").await], 4)
        .await
        .unwrap_err();
    assert_resume_incomplete(&error);
}

#[tokio::test]
async fn resume_rejects_start_beyond_phase_count_and_cross_lineage_versions() {
    let (cp, _tmp) = cp().await;
    let a = seed_version(
        &cp,
        "/lib/lineage/a.mkv",
        "hash-lineage-a",
        reprobe_payload("h264"),
    )
    .await;
    let b = seed_version(
        &cp,
        "/lib/lineage/b.mkv",
        "hash-lineage-b",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", a, 5).await;
    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, a, "movie").await], 4)
        .await
        .unwrap_err();
    assert_resume_incomplete(&error);

    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", b, 0).await;
    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, a, "movie").await], 4)
        .await
        .unwrap_err();
    assert_resume_incomplete(&error);
}

#[tokio::test]
async fn resume_rejects_cross_lineage_committed_row_and_changed_terminal_tip() {
    let (cp, _tmp) = cp().await;
    let a = seed_version(
        &cp,
        "/lib/row-lineage/a.mkv",
        "hash-row-a",
        reprobe_payload("h264"),
    )
    .await;
    let b = seed_version(
        &cp,
        "/lib/row-lineage/b.mkv",
        "hash-row-b",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", a, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Committed, Some(b)).await;
    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, a, "movie").await], 4)
        .await
        .unwrap_err();
    assert_resume_incomplete(&error);

    let (cp, _tmp) = crate::cases::cp().await;
    let v0 = seed_version(
        &cp,
        "/lib/terminal/movie.mkv",
        "hash-terminal-0",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", v0, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Blocked, None).await;
    cp.workflow_summaries()
        .begin_file_terminalization(prior, "movie")
        .await
        .unwrap();
    cp.workflow_summaries()
        .mark_file_terminal(prior, "movie", T0)
        .await
        .unwrap();
    let _v1 = advance_chain_tip(&cp, v0, "hash-terminal-1", reprobe_payload("hevc")).await;
    let error = cp
        .prepare_resume(prior, vec![phase_file(&cp, v0, "movie").await], 4)
        .await
        .unwrap_err();
    assert_resume_incomplete(&error);
}

#[tokio::test]
async fn phase_barrier_job_open_rolls_back_job_event_starts_and_seed() {
    let (cp, _tmp) = cp().await;
    let v0 = seed_version(
        &cp,
        "/lib/atomic/movie.mkv",
        "hash-atomic-0",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "movie", v0, 0).await;
    record_file_phase(
        &cp,
        prior,
        0,
        "movie",
        FilePhaseOutcome::Committed,
        Some(v0),
    )
    .await;
    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, v0, "movie").await], 4)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_resume_seed BEFORE INSERT ON workflow_file_phase_summaries \
         BEGIN SELECT RAISE(ABORT, 'forced seed failure'); END",
    )
    .execute(&cp.pool)
    .await
    .unwrap();
    let before = durable_resume_counts(&cp).await;

    let error = cp
        .open_sliding_file_job(
            &prepared.run_starts,
            prepared.history,
            prepared.seeds,
            &prepared.files,
            4,
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert_eq!(durable_resume_counts(&cp).await, before);
}

async fn durable_resume_counts(cp: &crate::ControlPlane) -> (i64, i64, i64, i64, i64) {
    let jobs = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let events = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let starts = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_file_run_starts")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let history = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_file_run_history")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let phases = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_file_phase_summaries")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    (jobs, events, starts, history, phases)
}

#[tokio::test]
async fn zero_phase_run_preserves_seed_file_phases_without_repromotion() {
    use crate::cases::policy::compliance::ComplianceExecutionOptions;
    use crate::workflow::execution::WorkerRuntimeRegistry;

    let (cp, _db) = cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let staging_root = root.join("stage");
    let working = staging_root.join(".committed").join("transcode");
    let out_dir = root.join("out");
    std::fs::create_dir_all(&working).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();

    let artifact_path = working.join("Movie.default-hevc.hevc.mkv");
    std::fs::write(&artifact_path, b"terminal-bytes").unwrap();
    std::fs::write(out_dir.join("Movie.default-hevc.hevc.mkv"), b"existing").unwrap();
    let version = seed_version(
        &cp,
        &artifact_path.display().to_string(),
        "hash-zero-phase",
        reprobe_payload("hevc"),
    )
    .await;
    let seed_job = open_workflow_job(&cp).await;
    record_file_phase(
        &cp,
        seed_job,
        0,
        "Movie",
        FilePhaseOutcome::Committed,
        Some(version),
    )
    .await;
    let seed_file_phases = cp
        .workflow_summaries()
        .file_phases_for_job(seed_job)
        .await
        .unwrap();
    let policy = policy_with_on_error(None);
    let context = voom_plan::PlanningContext::default();
    let base_draft = file_draft("zero-phase-promotion", &[]);
    let options = ComplianceExecutionOptions {
        transcode_staging_root: staging_root,
        transcode_target_dir: out_dir,
        ..ComplianceExecutionOptions::default()
    };
    let runner = cp.clone();

    let outcome = cp
        .with_phase_barrier_job(move |job_id| {
            Box::pin(async move {
                runner
                    .drive_phase_loop(super::PhaseLoopInputs {
                        job_id,
                        policy,
                        context,
                        base_draft,
                        files: Vec::new(),
                        seed_file_phases,
                        options,
                        runtimes: WorkerRuntimeRegistry::new(),
                    })
                    .await
            })
        })
        .await
        .unwrap();

    assert!(
        artifact_path.is_file(),
        "a completed zero-phase path must not re-promote a prior artifact"
    );
    assert_eq!(outcome.file_phases.len(), 1);
    assert_eq!(outcome.file_phases[0].branch_id, "Movie");
    assert_eq!(outcome.file_phases[0].outcome, FilePhaseOutcome::Committed);
    assert_eq!(job_state(&cp, outcome.job_id).await, "succeeded");
}

#[tokio::test]
async fn zero_survivor_resume_does_not_promote_blocked_branch() {
    use crate::workflow::execution::WorkerRuntimeRegistry;

    let (cp, _db) = cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let staging_root = root.join("stage");
    let working = staging_root.join(".committed").join("transcode");
    let out_dir = root.join("out");
    std::fs::create_dir_all(&working).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();
    let artifact_path = working.join("Movie.phase-zero.mkv");
    std::fs::write(&artifact_path, b"incomplete-phase-zero").unwrap();

    let version = seed_version(
        &cp,
        &artifact_path.display().to_string(),
        "hash-blocked-zero-survivor",
        reprobe_payload("h264"),
    )
    .await;
    let prior = open_workflow_job(&cp).await;
    record_run_start(&cp, prior, "Movie", version, 0).await;
    record_file_phase(
        &cp,
        prior,
        0,
        "Movie",
        FilePhaseOutcome::Committed,
        Some(version),
    )
    .await;
    record_file_phase(&cp, prior, 1, "Movie", FilePhaseOutcome::Blocked, None).await;
    cp.workflow_summaries()
        .begin_file_terminalization(prior, "Movie")
        .await
        .unwrap();
    cp.workflow_summaries()
        .mark_file_terminal(prior, "Movie", T0)
        .await
        .unwrap();
    let seed_file_phases = cp
        .workflow_summaries()
        .file_phases_for_job(prior)
        .await
        .unwrap();
    let options = ComplianceExecutionOptions {
        transcode_staging_root: staging_root,
        transcode_target_dir: out_dir.clone(),
        ..ComplianceExecutionOptions::default()
    };
    let runner = cp.clone();

    cp.with_phase_barrier_job(move |job_id| {
        Box::pin(async move {
            runner
                .drive_phase_loop(super::PhaseLoopInputs {
                    job_id,
                    policy: policy_with_on_error(None),
                    context: voom_plan::PlanningContext::default(),
                    base_draft: file_draft("blocked-zero-survivor", &[]),
                    files: Vec::new(),
                    seed_file_phases,
                    options,
                    runtimes: WorkerRuntimeRegistry::new(),
                })
                .await
        })
    })
    .await
    .unwrap();

    assert!(
        artifact_path.is_file(),
        "a terminal blocked branch must keep its withheld intermediate"
    );
    assert!(
        !out_dir.join("Movie.phase-zero.mkv").exists(),
        "a zero-survivor resume must not publish a blocked branch"
    );
}

#[tokio::test]
async fn reconcile_resume_resumes_after_skipped_phase() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/pt/movie.mkv", "hash-pt", reprobe_payload("h264")).await;
    record_run_start(&cp, prior, "movie", v, 0).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Skipped, None).await;

    let prepared = cp
        .prepare_resume(prior, vec![phase_file(&cp, v, "movie").await], 4)
        .await
        .unwrap();
    assert_eq!(
        prepared.files[0].resume_ordinal, 1,
        "skipped row at 0 => resume at 1"
    );
    assert_eq!(prepared.seeds.len(), 1);
    assert_eq!(prepared.seeds[0].outcome, FilePhaseOutcome::Skipped);
}

#[tokio::test]
async fn resume_phase_barrier_rejects_unknown_prior_job() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let v = seed_version(&cp, "/lib/u/movie.mkv", "hash-u1", reprobe_payload("h264")).await;
    let s = latest_snapshot(&cp, v).await;
    let input = cp
        .create_policy_input_set(file_draft("unknown-prior", &[s]))
        .await
        .unwrap();

    let err = cp
        .resume_phase_barrier(
            JobId(999_999),
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.source.code(), "NOT_FOUND");
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "no job opens when the prior job is unknown");
}

#[tokio::test]
async fn resume_loads_legacy_continue_before_resume_state_validation() {
    let (cp, _tmp) = cp().await;
    let source = "policy \"on-error-guard\" {\n  config {\n    \
        languages: [\"eng\"]\n    on_error: continue\n  }\n  \
        phase normalize {\n    container mkv\n  }\n  \
        phase finalize {\n    depends_on: [normalize]\n    on_error: abort\n  }\n}\n";
    let created = cp
        .create_policy_document("on-error-guard", source)
        .await
        .unwrap();
    let mut legacy = created.version.compiled_json;
    legacy["config"] = json!({
        "languages": "languages audio: [eng]",
        "on_error": "on_error continue"
    });
    legacy["phases"][0]["on_error"] = Value::Null;
    // Simulate a row created by an older binary. Production rows are immutable
    // after insertion, including across binary upgrades.
    sqlx::query("DROP TRIGGER policy_versions_are_immutable")
        .execute(&cp.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE policy_versions SET compiled_json = ? WHERE id = ?")
        .bind(serde_json::to_string(&legacy).unwrap())
        .bind(i64::try_from(created.version.id.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();
    let stored = cp
        .get_policy_version(created.version.id)
        .await
        .unwrap()
        .unwrap();
    let loaded = cp.compiled_policy_for_version(&stored).await.unwrap();
    assert_eq!(
        loaded.phases[0].on_error,
        Some(voom_policy::ErrorStrategy::Continue)
    );
    assert_eq!(
        loaded.phases[1].on_error,
        Some(voom_policy::ErrorStrategy::Abort)
    );
    let v = seed_version(&cp, "/lib/o/movie.mkv", "hash-o1", reprobe_payload("h264")).await;
    let s = latest_snapshot(&cp, v).await;
    let input = cp
        .create_policy_input_set(file_draft("on-error", &[s]))
        .await
        .unwrap();
    let prior = open_workflow_job(&cp).await;

    let err = cp
        .resume_phase_barrier(
            prior,
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.source.code(), "POLICY_EXECUTION_ERROR");
    assert!(err.source.to_string().contains("resume state"));
    // Resume validation still precedes opening the replacement job, so only the
    // pre-existing prior job remains.
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1, "resume opened no job beyond the prior one");
}

#[tokio::test]
async fn tool_requirements_fail_before_fresh_or_resume_jobs_open() {
    let (cp, _tmp) = cp().await;
    let source = "policy \"requires ffmpeg\" {\n  metadata {\n    \
        requires_tools: [ffmpeg]\n  }\n  phase inspect {}\n}\n";
    let created = cp
        .create_policy_document("requires-ffmpeg", source)
        .await
        .unwrap();
    let version = seed_version(
        &cp,
        "/lib/tool/movie.mkv",
        "hash-tool",
        reprobe_payload("h264"),
    )
    .await;
    let snapshot = latest_snapshot(&cp, version).await;
    let input = cp
        .create_policy_input_set(file_draft("requires-ffmpeg", &[snapshot]))
        .await
        .unwrap();

    let fresh = cp
        .run_phase_barrier_with_runtimes(
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
            crate::workflow::WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(fresh.source.code(), "POLICY_EXECUTION_ERROR");
    assert!(fresh.source.to_string().contains("ffmpeg"));
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "fresh preflight failure opened a job");

    let prior = open_workflow_job(&cp).await;
    let resumed = cp
        .resume_phase_barrier_with_runtimes(
            prior,
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
            crate::workflow::WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(resumed.source.code(), "POLICY_EXECUTION_ERROR");
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1, "resume preflight failure opened a new job");
}

/// Post-run promotion canonicalizes the working dirs, so the candidate artifact
/// path must be canonicalized too: a live location recorded at a path that
/// traverses a symlink (e.g. macOS `/tmp` -> `/private/tmp`) must still match its
/// working dir and be promoted. A non-symmetric prefix match would silently leave
/// the terminal artifact in the working dir while the job succeeded.
#[tokio::test]
async fn promote_terminal_artifacts_matches_through_symlinked_working_dir() {
    use crate::cases::policy::compliance::{PromotionPair, PromotionPlan};

    let (cp, _db) = cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    // A real working dir + output dir, and a symlink pointing at the real root.
    let real = root.join("real");
    let working = real.join(".committed").join("remux");
    std::fs::create_dir_all(&working).unwrap();
    let out_dir = real.join("out");
    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // A committed terminal artifact on disk, recorded at its SYMLINKED path.
    std::fs::write(working.join("Movie.mkv"), b"terminal-bytes").unwrap();
    let symlinked_value = link
        .join(".committed")
        .join("remux")
        .join("Movie.mkv")
        .display()
        .to_string();
    let version = seed_version(
        &cp,
        &symlinked_value,
        "hash-symlink",
        reprobe_payload("hevc"),
    )
    .await;

    // The working dir is supplied symlinked, exactly as it would arrive from a
    // `--staging-root` that traverses a symlink.
    let plan = PromotionPlan {
        pairs: vec![PromotionPair {
            working_dir: link.join(".committed").join("remux"),
            output_dir: out_dir.clone(),
        }],
    };

    let location_id = live_location_id(&cp, version).await;
    cp.promote_terminal_artifacts(&plan, &[location_id])
        .await
        .unwrap();

    let promoted = out_dir.join("Movie.mkv");
    assert!(
        promoted.is_file(),
        "the terminal artifact must be promoted through the symlinked working dir"
    );
    assert!(
        !working.join("Movie.mkv").exists(),
        "the artifact must be moved out of the working dir"
    );
    let location = cp
        .identity()
        .list_live_file_locations_by_version(version)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        location.value,
        promoted.display().to_string(),
        "the chain tip location must repoint to the promoted (canonical) path"
    );
}

#[tokio::test]
async fn promotion_location_ids_rejects_negative_ticket_result_location_id() {
    let (cp, _db) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: TicketOperation::new("synthetic.workflow.operation.test").unwrap(),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ?, \
         epoch = epoch + 1 WHERE id = ?",
    )
    .bind(serde_json::to_string(&json!({"result_file_location_id": -7})).unwrap())
    .bind(T0.format(&Iso8601::DEFAULT).unwrap())
    .bind(i64::try_from(ticket.id.0).unwrap())
    .execute(&cp.pool)
    .await
    .unwrap();

    let err = cp
        .ticket_result_location_ids_for_tickets(&[ticket.id])
        .await
        .unwrap_err();

    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert!(
        err.to_string()
            .contains("promotion ticket result location id"),
        "error should identify the corrupted column: {err}"
    );
    assert!(
        err.to_string().contains("-7"),
        "error should include the invalid persisted value: {err}"
    );
}

#[tokio::test]
async fn phase_ticket_lookup_ignores_matching_nodes_from_other_invocations() {
    let (cp, _db) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tickets = Vec::new();
    for workflow_id in [
        format!("workflow-{}-phase-0", job.id.0),
        format!("workflow-{}-phase-1", job.id.0),
    ] {
        tickets.push(
            cp.create_ticket(NewTicket {
                job_id: Some(job.id),
                kind: TicketOperation::new("synthetic.workflow.operation.test").unwrap(),
                priority: 0,
                payload: json!({
                    "workflow_id": workflow_id,
                    "node_id": "policy-node-normalize"
                }),
                max_attempts: 1,
                created_at: T0,
            })
            .await
            .unwrap(),
        );
    }

    let found = cp
        .ticket_ids_for_phase_scope(job.id, 1, "policy-node-normalize", None)
        .await
        .unwrap();

    assert_eq!(found, vec![tickets[1].id]);
}

#[test]
fn promotion_sqlite_conversions_reject_out_of_range_values() {
    let read_err = super::sqlite_u64(-1, "promotion location id").unwrap_err();
    assert_eq!(read_err.code(), "DB_UNREACHABLE");
    assert!(
        read_err.to_string().contains("promotion location id -1"),
        "read error should identify the invalid value: {read_err}"
    );

    let bind_err = super::sqlite_i64(u64::MAX, "promotion asset id").unwrap_err();
    assert_eq!(bind_err.code(), "DB_UNREACHABLE");
    assert!(
        bind_err.to_string().contains("does not fit SQLite i64"),
        "bind error should identify SQLite's integer boundary: {bind_err}"
    );
}

/// A whole-library run over two sources that share a basename across
/// subdirectories must not collide at promotion (issue #197): each terminal
/// artifact lands under `--output-dir` mirroring its source's path relative to
/// the run's common root. A flat-by-basename promotion would move both to the
/// same destination and fail the run after the transcodes already ran.
#[tokio::test]
async fn promote_terminal_artifacts_mirrors_source_subtree_for_duplicate_basenames() {
    use crate::cases::policy::compliance::{PromotionPair, PromotionPlan};

    let (cp, _db) = cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let working = root.join(".committed").join("remux");
    let out_dir = root.join("out");

    // Two assets whose ORIGINAL sources live in different subdirs but share the
    // basename `episode.mkv`.
    let mut tips = Vec::new();
    for season in ["S01", "S02"] {
        let source = format!("/library/{season}/episode.mkv");
        let v1 = seed_version(
            &cp,
            &source,
            &format!("hash-{season}"),
            reprobe_payload("h264"),
        )
        .await;
        let asset_id = cp
            .identity()
            .get_file_version(v1)
            .await
            .unwrap()
            .unwrap()
            .file_asset_id;
        // The terminal (chain-tip) artifact, committed into a per-source working
        // subdir (matching the commit-uniqueness layout).
        let v2 = cp
            .create_file_version(NewFileVersion {
                file_asset_id: asset_id,
                content_hash: format!("hash-{season}-remux"),
                size_bytes: 2048,
                produced_by: ProducedBy::Remux,
                produced_from_version_id: Some(v1),
                created_at: T0,
            })
            .await
            .unwrap();
        let artifact_dir = working.join(format!("v{}", v2.id.0));
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let artifact_path = artifact_dir.join("episode.remux.mkv");
        std::fs::write(&artifact_path, format!("{season}-bytes")).unwrap();
        cp.create_file_location(NewFileLocation {
            file_version_id: v2.id,
            kind: FileLocationKind::LocalPath,
            value: artifact_path.display().to_string(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();
        tips.push((season, v2.id));
    }

    let plan = PromotionPlan {
        pairs: vec![PromotionPair {
            working_dir: working.clone(),
            output_dir: out_dir.clone(),
        }],
    };

    let mut location_ids = Vec::new();
    for (_, vid) in &tips {
        location_ids.push(live_location_id(&cp, *vid).await);
    }
    cp.promote_terminal_artifacts(&plan, &location_ids)
        .await
        .unwrap();

    for (season, vid) in tips {
        let promoted = out_dir.join(season).join("episode.remux.mkv");
        assert!(
            promoted.is_file(),
            "terminal artifact for {season} must be promoted under its source subtree at {}",
            promoted.display()
        );
        let location = cp
            .identity()
            .list_live_file_locations_by_version(vid)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            location.value,
            promoted.display().to_string(),
            "the {season} chain tip must repoint to the mirrored promoted path"
        );
    }
}

#[tokio::test]
async fn promote_terminal_artifacts_ignores_unscoped_working_dir_artifacts() {
    use crate::cases::policy::compliance::{PromotionPair, PromotionPlan};

    let (cp, _db) = cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let working = root.join(".committed").join("remux");
    let out_dir = root.join("out");

    let first_path = working.join("run-a").join("Movie.remux.mkv");
    let second_path = working.join("run-b").join("Other.remux.mkv");
    let first =
        seed_terminal_artifact(&cp, "/library/run-a/Movie.mkv", "hash-run-a", &first_path).await;
    let second =
        seed_terminal_artifact(&cp, "/library/run-b/Other.mkv", "hash-run-b", &second_path).await;
    let plan = PromotionPlan {
        pairs: vec![PromotionPair {
            working_dir: working,
            output_dir: out_dir.clone(),
        }],
    };

    cp.promote_terminal_artifacts(&plan, &[first.location_id])
        .await
        .unwrap();

    assert!(
        out_dir.join("Movie.remux.mkv").is_file(),
        "scoped location should promote"
    );
    assert!(
        second_path.is_file(),
        "unscoped location {} must stay in the working dir",
        second_path.display()
    );
    let second_location = cp
        .identity()
        .list_live_file_locations_by_version(second.version_id)
        .await
        .unwrap()
        .into_iter()
        .find(|location| location.id == second.location_id)
        .unwrap();
    assert_eq!(second_location.value, second_path.display().to_string());
}

#[tokio::test]
async fn promote_terminal_artifacts_skips_non_tip_scoped_locations() {
    use crate::cases::policy::compliance::{PromotionPair, PromotionPlan};

    let (cp, _db) = cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let working = root.join(".committed").join("remux");
    let out_dir = root.join("out");
    std::fs::create_dir_all(&working).unwrap();
    let old_path = working.join("old.mkv");
    std::fs::write(&old_path, b"old-bytes").unwrap();
    let old_version = seed_version(
        &cp,
        &old_path.display().to_string(),
        "hash-old-tip",
        reprobe_payload("h264"),
    )
    .await;
    let old_location_id = live_location_id(&cp, old_version).await;

    let asset_id = cp
        .identity()
        .get_file_version(old_version)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let new_version = cp
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: "hash-new-tip".to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Remux,
            produced_from_version_id: Some(old_version),
            created_at: T0,
        })
        .await
        .unwrap();
    let new_path = working.join("new.mkv");
    std::fs::write(&new_path, b"new-bytes").unwrap();
    let new_location = cp
        .create_file_location(NewFileLocation {
            file_version_id: new_version.id,
            kind: FileLocationKind::LocalPath,
            value: new_path.display().to_string(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();
    let plan = PromotionPlan {
        pairs: vec![PromotionPair {
            working_dir: working,
            output_dir: out_dir.clone(),
        }],
    };

    cp.promote_terminal_artifacts(&plan, &[old_location_id, new_location.id])
        .await
        .unwrap();

    assert!(
        old_path.is_file(),
        "non-tip scoped location should stay in the working dir"
    );
    assert!(
        out_dir.join("new.mkv").is_file(),
        "chain-tip scoped location should promote"
    );
}

#[tokio::test]
async fn branch_promotion_ids_include_every_ordered_extract_output() {
    let (cp, _db) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: TicketOperation::new("synthetic.workflow.operation.extract").unwrap(),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ?, \
         epoch = epoch + 1 WHERE id = ?",
    )
    .bind(
        serde_json::to_string(&json!({
            "result_file_location_id": 101,
            "outputs": [
                {"result_file_location_id": 101},
                {"result_file_location_id": 102}
            ]
        }))
        .unwrap(),
    )
    .bind(T0.format(&Iso8601::DEFAULT).unwrap())
    .bind(i64::try_from(ticket.id.0).unwrap())
    .execute(&cp.pool)
    .await
    .unwrap();
    let rows = vec![voom_store::repo::workflow_summaries::FilePhaseSummary {
        id: 1,
        job_id: job.id,
        phase_ordinal: 0,
        branch_id: "movie".to_owned(),
        ticket_ids: vec![ticket.id],
        produced_file_version_id: Some(FileVersionId(1)),
        produced_file_location_id: Some(FileLocationId(101)),
        artifact_handle_id: None,
        artifact_verification_id: None,
        reprobe_snapshot_id: Some(voom_core::MediaSnapshotId(1)),
        outcome: FilePhaseOutcome::Committed,
        created_at: T0,
    }];

    let location_ids = cp
        .promotion_location_ids_for_branches(&rows, &["movie".to_owned()])
        .await
        .unwrap();

    assert_eq!(location_ids, vec![FileLocationId(101), FileLocationId(102)]);
}

struct TerminalArtifact {
    version_id: FileVersionId,
    location_id: FileLocationId,
}

async fn seed_terminal_artifact(
    cp: &crate::ControlPlane,
    source_path: &str,
    hash: &str,
    artifact_path: &std::path::Path,
) -> TerminalArtifact {
    let source = seed_version(cp, source_path, hash, reprobe_payload("h264")).await;
    let asset_id = cp
        .identity()
        .get_file_version(source)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let version = cp
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: format!("{hash}-remux"),
            size_bytes: 2048,
            produced_by: ProducedBy::Remux,
            produced_from_version_id: Some(source),
            created_at: T0,
        })
        .await
        .unwrap();
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(artifact_path, format!("{hash}-bytes")).unwrap();
    let location = cp
        .create_file_location(NewFileLocation {
            file_version_id: version.id,
            kind: FileLocationKind::LocalPath,
            value: artifact_path.display().to_string(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();
    TerminalArtifact {
        version_id: version.id,
        location_id: location.id,
    }
}

async fn live_location_id(cp: &crate::ControlPlane, version: FileVersionId) -> FileLocationId {
    cp.identity()
        .list_live_file_locations_by_version(version)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id
}
