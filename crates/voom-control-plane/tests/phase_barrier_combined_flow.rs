//! Sprint 16 §9/§10 closeout: the combined heterogeneous multi-phase flow.
//!
//! A single policy runs three different real mutation operations as phase
//! barriers over one file — container `remux` + track selection, then `transcode
//! video` (h264 -> hevc), then `transcode audio` (aac -> opus) — each phase
//! planning and committing against the artifact the prior phase produced and
//! re-probed. This is the only test that exercises all three operation kinds in
//! one chain; the sibling `phase_barrier_flow.rs` chains video transcode twice.
//!
//! Real ffmpeg/mkvmerge/ffprobe output embeds run- and version-varying facts, so
//! this is a field-assertion test, not an `insta` golden (same reason as
//! `phase_barrier_flow.rs` and `multi_phase_flow.rs`). The deterministic preview
//! path is goldened separately in `voom-cli/tests/multi_phase_preview_envelope.rs`.
//!
//! Per the project test-layout rule, the full multi-phase run launches the
//! bundled ffprobe on staged output and is therefore exercised only by
//! `cargo test --workspace`; it skips with a clear message when ffmpeg, ffprobe,
//! or mkvmerge is absent.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::Path;
use std::process::Command;

use voom_control_plane::ControlPlane;
use voom_control_plane::CoordinatorOutcome;
use voom_control_plane::policy::{ComplianceExecutionOptions, PolicyInputFromScanInput};
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::FileVersionId;
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, PhaseOutcome, SqliteWorkflowSummaryRepo,
};
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_build_package, hide_stale_fake_ffprobe_sibling,
    target_debug_binary,
};

/// A three-phase policy combining the three real mutation operation kinds. Each
/// phase depends on the prior, so the coordinator runs them as ordered barriers,
/// and every phase plans against the artifact the prior phase produced and
/// re-probed.
///
/// Phase order is `remux -> transcode -> audio`:
///
/// * The remux phase's `keep audio where lang in [eng, und]` drops the fixture's
///   `spa` track (genuine track selection, not a no-op), leaving the `eng` track.
/// * The transcode phase re-encodes the video to hevc; its `-c:a copy` carries the
///   audio stream (and its language/title/disposition tags) through untouched.
/// * The audio phase's `transcode audio to opus where lang in [eng, und]` plans
///   against the *re-probed* transcode output. Per ADR-0011 the planner gates
///   transcode plannability on the source codec + container only, so this commits
///   even though the fixture's audio tracks are title-less — the case real media
///   hits because muxers do not synthesize a title.
const COMBINED_POLICY: &str = r#"
policy "sprint 16 combined" {
  phase remux {
    container mkv
    keep audio where lang in [eng, und]
    order tracks [video, audio, subtitle]
    defaults audio: first
  }
  phase transcode {
    depends_on: [remux]
    transcode video to hevc
  }
  phase audio {
    depends_on: [transcode]
    transcode audio to opus where lang in [eng, und]
  }
}
"#;

/// The full remux -> transcode -> audio chain commits one new `FileVersion` per
/// phase, each planned against the prior phase's produced + re-probed artifact,
/// with an append-only lineage and a re-probe snapshot keyed to each produced
/// version — all tied together by the durable three-grain workflow summary.
#[tokio::test]
async fn phase_barrier_runs_transcode_remux_audio_chain_end_to_end() {
    require_command("ffmpeg", &["-version"]);
    require_command("ffprobe", &["-version"]);
    require_command("mkvmerge", &["--version"]);
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("phase-barrier-combined").unwrap();
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    cargo_build_package("voom-mkvtoolnix-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = root.join("Movie.mkv");
    generate_combined_fixture(&source);

    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let scanned = scan_one(&cp, &source).await;
    let scanned_version = scanned.file_version_id;
    let policy = cp
        .create_policy_document("sprint-16-combined", COMBINED_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "combined-e2e".to_owned(),
            file_version_id: scanned.file_version_id,
            media_snapshot_id: scanned.media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let mut workers = CombinedWorkers::start(&cp).await;
    let outcome = cp
        .run_phase_barrier(
            policy.version.id,
            input.input_set_id,
            combined_execution_options(&root),
        )
        .await;
    workers.shutdown();
    let outcome = outcome.unwrap_or_else(|err| {
        panic!(
            "combined phase-barrier run must succeed; error={err:?}, partial={:?}",
            err.partial
        )
    });

    let produced = assert_three_phase_commit(&outcome);
    assert_lineage_chain(&url, scanned_version, &produced).await;
    assert_reprobe_snapshots_keyed(&url, &produced).await;
    assert_phase_mutations(&url, &produced).await;
    assert_durable_summary(&url, outcome.job_id, &produced).await;
}

/// Each phase performed its declared mutation, read back from the produced
/// version's re-probe snapshot — not merely "a commit happened". This guards
/// against a phase silently degrading (e.g. a remux that copies every track, or
/// a transcode that stream-copies) yet still committing a new version:
///
/// * remux (`produced[0]`): the `spa` track is gone — exactly one audio stream
///   survives, in `eng` — proving `keep audio where lang in [eng, und]` selected
///   tracks rather than copying the source's two audio streams through.
/// * transcode (`produced[1]`): the video stream is now `hevc`.
/// * audio (`produced[2]`): the surviving audio stream is now `opus`.
async fn assert_phase_mutations(url: &str, produced: &[FileVersionId]) {
    let remux_audio = audio_streams(url, produced[0]).await;
    assert_eq!(
        remux_audio.len(),
        1,
        "remux must drop the spa track (track selection), leaving one audio: {remux_audio:?}"
    );
    assert_eq!(
        remux_audio[0].1.as_deref(),
        Some("eng"),
        "the surviving audio after remux must be the eng track: {remux_audio:?}"
    );

    assert_eq!(
        video_codecs(url, produced[1]).await,
        vec!["hevc".to_owned()],
        "the transcode phase must produce an hevc video stream"
    );

    let audio_after = audio_streams(url, produced[2]).await;
    assert_eq!(
        audio_after.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        vec!["opus"],
        "the audio phase must transcode the surviving track to opus: {audio_after:?}"
    );
}

/// Every phase completed and committed: three `Completed` phase rows named
/// `[remux, transcode, audio]`, and three `Committed` per-`(file, phase)` rows
/// (ordinals 0/1/2) each carrying produced references and attributing tickets.
/// Returns the produced version per ordinal, in phase order.
fn assert_three_phase_commit(outcome: &CoordinatorOutcome) -> Vec<FileVersionId> {
    let phase_names: Vec<&str> = outcome
        .phases
        .iter()
        .map(|phase| phase.phase_name.as_str())
        .collect();
    assert_eq!(
        phase_names,
        vec!["remux", "transcode", "audio"],
        "all three phases ran in order: {outcome:?}"
    );
    assert!(
        outcome
            .phases
            .iter()
            .all(|phase| phase.outcome == PhaseOutcome::Completed),
        "every phase must complete: {:?}",
        outcome.phases
    );

    let mut produced = Vec::new();
    for ordinal in 0..3u32 {
        let row = outcome
            .file_phases
            .iter()
            .find(|row| row.phase_ordinal == ordinal)
            .unwrap_or_else(|| {
                panic!(
                    "phase {ordinal} must record a per-file row: {:?}",
                    outcome.file_phases
                )
            });
        assert_eq!(
            row.outcome,
            FilePhaseOutcome::Committed,
            "phase {ordinal} must commit (no no-op stall): {row:?}"
        );
        assert!(
            row.produced_file_location_id.is_some(),
            "phase {ordinal} committed row records the produced location"
        );
        assert!(
            row.reprobe_snapshot_id.is_some(),
            "phase {ordinal} committed row records the post-commit reprobe snapshot"
        );
        assert!(
            !row.ticket_ids.is_empty(),
            "phase {ordinal} committed row attributes its operation tickets"
        );
        produced.push(
            row.produced_file_version_id
                .expect("committed row records its produced version"),
        );
    }
    assert_eq!(
        produced
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "each phase produces a distinct version: {produced:?}"
    );
    produced
}

/// The produced versions form an append-only chain rooted at the scan:
/// scan -> v0 (remux) -> v1 (transcode) -> v2 (audio), via `produced_from`.
async fn assert_lineage_chain(url: &str, scanned: FileVersionId, produced: &[FileVersionId]) {
    let mut parent = scanned;
    for (ordinal, version) in produced.iter().enumerate() {
        assert_eq!(
            produced_from(url, *version).await,
            Some(i64::try_from(parent.0).unwrap()),
            "phase {ordinal}'s artifact must descend from the prior phase's version"
        );
        parent = *version;
    }
}

/// Each produced version carries exactly one media snapshot — its post-commit
/// reprobe — which is the fact the coordinator projects into the next phase's
/// planner (issue #163). The chain-tip snapshot of phase N is phase N+1's input.
async fn assert_reprobe_snapshots_keyed(url: &str, produced: &[FileVersionId]) {
    for (ordinal, version) in produced.iter().enumerate() {
        let snapshots = snapshots_for_version(url, *version).await;
        assert_eq!(
            snapshots.len(),
            1,
            "phase {ordinal}'s produced version carries exactly its reprobe snapshot: {snapshots:?}"
        );
    }
}

/// The durable two-grain summary ties every phase to its tickets, artifacts, and
/// reprobe snapshots: re-read through a fresh repo, three `Completed` phase rows
/// each carry a content-addressed report id matching the embedded report
/// identity, and three `Committed` per-file rows record the produced versions.
async fn assert_durable_summary(url: &str, job_id: voom_core::JobId, produced: &[FileVersionId]) {
    let repo = SqliteWorkflowSummaryRepo::new(voom_store::connect(url).await.unwrap());

    let phases = repo.phases_for_job(job_id).await.unwrap();
    assert_eq!(phases.len(), 3, "three durable phase rows");
    for phase in &phases {
        assert_eq!(phase.outcome, PhaseOutcome::Completed);
        let report = phase
            .report
            .as_ref()
            .expect("a completed phase records a regenerated report");
        assert!(
            !report.report_id.is_empty(),
            "phase {} records a content-addressed report id",
            phase.phase_ordinal
        );
        assert_eq!(
            report.report_id, report.report["report_id"],
            "the row's report_id matches the embedded report identity"
        );
    }

    let file_phases = repo.file_phases_for_job(job_id).await.unwrap();
    assert_eq!(file_phases.len(), 3, "three durable committed file rows");
    let mut durable_versions: Vec<i64> = file_phases
        .iter()
        .map(|row| {
            assert_eq!(row.outcome, FilePhaseOutcome::Committed);
            i64::try_from(
                row.produced_file_version_id
                    .expect("durable committed row records its produced version")
                    .0,
            )
            .unwrap()
        })
        .collect();
    durable_versions.sort_unstable();
    let mut expected: Vec<i64> = produced
        .iter()
        .map(|v| i64::try_from(v.0).unwrap())
        .collect();
    expected.sort_unstable();
    assert_eq!(
        durable_versions, expected,
        "durable file rows record exactly the produced versions"
    );
}

// --- harness ---------------------------------------------------------------

#[derive(Clone, Copy)]
struct ScannedFile {
    file_version_id: FileVersionId,
    media_snapshot_id: voom_core::MediaSnapshotId,
}

async fn scan_one(cp: &ControlPlane, source: &Path) -> ScannedFile {
    let scan = cp
        .scan_path(ScanPathInput {
            path: source.to_owned(),
        })
        .await
        .unwrap();
    let scanned = scan
        .files
        .iter()
        .find(|file| file.status == ScanReportFileStatus::Scanned)
        .unwrap();
    ScannedFile {
        file_version_id: scanned.file_version_id.unwrap(),
        media_snapshot_id: scanned.media_snapshot_id.unwrap(),
    }
}

fn combined_execution_options(root: &Path) -> ComplianceExecutionOptions {
    ComplianceExecutionOptions {
        transcode_staging_root: root.join("stage"),
        transcode_target_dir: root.join("out/transcode"),
        remux_staging_root: root.join("stage"),
        remux_target_dir: root.join("out/remux"),
        audio_staging_root: root.join("stage"),
        audio_target_dir: root.join("out/audio"),
        backup_root: None,
    }
}

/// The three mutation workers the combined policy needs, all registered and
/// running for the single `run_phase_barrier` call: `transcode_video` and
/// `transcode_audio` are served by the ffmpeg worker (two distinct
/// registrations), `remux` by the mkvtoolnix worker.
struct CombinedWorkers {
    transcode_video: TestWorkerLaunch,
    remux: TestWorkerLaunch,
    transcode_audio: TestWorkerLaunch,
}

impl CombinedWorkers {
    async fn start(cp: &ControlPlane) -> Self {
        let transcode_video = TestWorkerLaunch::start(
            cp,
            TestWorkerConfig::synthetic(
                target_debug_binary("voom-ffmpeg-worker"),
                "combined-transcode-video",
                "combined-e2e-secret-transcode-video",
                "transcode_video",
            ),
        )
        .await
        .unwrap();
        let remux = TestWorkerLaunch::start(
            cp,
            TestWorkerConfig::synthetic(
                target_debug_binary("voom-mkvtoolnix-worker"),
                "combined-remux",
                "combined-e2e-secret-remux",
                "remux",
            ),
        )
        .await
        .unwrap();
        let transcode_audio = TestWorkerLaunch::start(
            cp,
            TestWorkerConfig::synthetic(
                target_debug_binary("voom-ffmpeg-worker"),
                "combined-transcode-audio",
                "combined-e2e-secret-transcode-audio",
                "transcode_audio",
            ),
        )
        .await
        .unwrap();
        Self {
            transcode_video,
            remux,
            transcode_audio,
        }
    }

    fn shutdown(&mut self) {
        self.transcode_video.shutdown().unwrap();
        self.remux.shutdown().unwrap();
        self.transcode_audio.shutdown().unwrap();
    }
}

async fn produced_from(url: &str, version: FileVersionId) -> Option<i64> {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT produced_from_version_id FROM file_versions WHERE id = ?",
    )
    .bind(i64::try_from(version.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap()
}

async fn snapshots_for_version(url: &str, version: FileVersionId) -> Vec<i64> {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM media_snapshots WHERE file_version_id = ? ORDER BY id ASC",
    )
    .bind(i64::try_from(version.0).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap()
}

/// The streams array of a version's chain-tip (latest) re-probe snapshot.
async fn version_streams(url: &str, version: FileVersionId) -> Vec<serde_json::Value> {
    let pool = voom_store::connect(url).await.unwrap();
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM media_snapshots WHERE file_version_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(i64::try_from(version.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    payload["streams"].as_array().cloned().unwrap_or_default()
}

/// `(codec_name, language)` for each audio stream in the version's chain-tip
/// snapshot, in stream order.
async fn audio_streams(url: &str, version: FileVersionId) -> Vec<(String, Option<String>)> {
    version_streams(url, version)
        .await
        .iter()
        .filter(|stream| stream["kind"] == "audio")
        .map(|stream| {
            (
                stream["codec_name"]
                    .as_str()
                    .expect("audio stream records a codec")
                    .to_owned(),
                stream["language"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// `codec_name` for each video stream in the version's chain-tip snapshot.
async fn video_codecs(url: &str, version: FileVersionId) -> Vec<String> {
    version_streams(url, version)
        .await
        .iter()
        .filter(|stream| stream["kind"] == "video")
        .map(|stream| {
            stream["codec_name"]
                .as_str()
                .expect("video stream records a codec")
                .to_owned()
        })
        .collect()
}

fn require_command(program: &str, args: &[&str]) {
    let output = Command::new(program).args(args).output().unwrap_or_else(|err| {
        panic!("required media tool `{program}` is unavailable; install it for the Sprint 16 combined flow: {err}")
    });
    assert!(
        output.status.success(),
        "required media tool `{program}` failed its setup check with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One-second 32x32 h264 video plus two aac audio tracks (`eng`, `spa`) in an
/// mkv container (the path extension selects the muxer). The `spa` track exists
/// so the remux phase's `keep audio where lang in [eng, und]` removes it — making
/// the remux real work rather than a no-op — leaving `eng` for the audio phase to
/// transcode.
fn generate_combined_fixture(path: &Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=32x32:rate=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660:sample_rate=48000",
            "-t",
            "1",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            // ADR-0011: the audio-transcode planner no longer requires a per-
            // stream title/commentary. These tracks are deliberately title-less
            // to prove a title-less remux -> transcode -> audio chain commits;
            // only language + disposition are set (`-disposition:a:N default|0`
            // sets an explicit flag set, which clears `comment` to a concrete
            // `false`).
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=spa",
            "-disposition:a:0",
            "default",
            "-disposition:a:1",
            "0",
            path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg combined fixture generation failed: {status}"
    );
}
