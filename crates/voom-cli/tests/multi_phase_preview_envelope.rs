//! Deterministic CLI golden for the multi-phase pre-run preview (issue #167,
//! spec §9). `compliance report` plans the combined three-phase policy (container
//! remux + track selection, video transcode, audio transcode) against a declared,
//! fixed input snapshot and emits the compliance report envelope.
//!
//! This is the *preview* half of the determinism split: it runs no workers and
//! no real media tools, so the content-addressed report identity is stable and
//! can be locked with `insta`. The real multi-phase *execute* flow embeds
//! run-varying ffmpeg facts and is field-asserted instead (see
//! `crates/voom-cli/tests/multi_phase_flow.rs` and
//! `crates/voom-control-plane/tests/phase_barrier_combined_flow.rs`).

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result through every assertion"
)]

use std::process::Command;

use serde_json::{Value, json};
use tempfile::{NamedTempFile, TempDir};
use time::OffsetDateTime;
use voom_control_plane::ControlPlane;
use voom_control_plane::policy::PolicyInputFromScanInput;
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::test_support::sqlite_url_for;

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

/// `compliance report` plans every phase of the combined policy against the
/// declared snapshot and emits a stable report envelope: the video transcode,
/// container remux, and audio transcode checks all resolve against the same
/// fixed facts, so the content-addressed report identity is deterministic.
#[tokio::test]
async fn compliance_report_previews_combined_multi_phase_policy() {
    let seeded = seed_combined().await;

    let output = compliance_report(&seeded.url, seeded.version_id, seeded.input_id);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "compliance");
    assert_eq!(json["status"], "ok");
    // The preview plans all three operation kinds against the declared facts.
    let kinds = &json["data"]["report"]["summary"]["operation_counts_by_kind"];
    assert_eq!(kinds["transcode_video"], 1, "report previews the transcode");
    assert_eq!(kinds["remux"], 1, "report previews the remux");
    assert_eq!(
        kinds["transcode_audio"], 1,
        "report previews the audio mutation"
    );

    redact_local(&mut json);
    insta::assert_json_snapshot!(
        "compliance_report_previews_combined_multi_phase_policy",
        json
    );
}

struct Seeded {
    _tmp: NamedTempFile,
    _dir: TempDir,
    url: String,
    version_id: u64,
    input_id: u64,
}

/// Seed a store with the combined policy and a fixed, audio-rich input snapshot
/// (one h264 video stream and one eng aac audio stream carrying the language,
/// title, channels, and commentary-disposition facts the audio planner needs).
/// All facts and timestamps are constant, so the report is byte-stable.
async fn seed_combined() -> Seeded {
    let tmp = NamedTempFile::new().unwrap();
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let created = cp
        .create_policy_document("sprint-16-combined", COMBINED_POLICY)
        .await
        .unwrap();

    let source = root.join("Movie.mkv");
    let source_bytes = b"combined preview source bytes";
    std::fs::write(&source, source_bytes).unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: source.display().to_string(),
                content_hash: blake3_checksum(source_bytes),
                size_bytes: u64::try_from(source_bytes.len()).unwrap(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
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
        panic!("seed_combined should create a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            json!({
                "container": { "format_name": "matroska,webm" },
                "streams": [
                    {
                        "id": "stream-0",
                        "index": 0,
                        "kind": "video",
                        "codec_name": "h264",
                        "disposition": { "default": true }
                    },
                    {
                        "id": "stream-1",
                        "index": 1,
                        "kind": "audio",
                        "codec_name": "aac",
                        "language": "eng",
                        "title": "Main",
                        "channels": 2,
                        "disposition": { "default": true, "commentary": false }
                    }
                ]
            }),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "cli-multi-phase-preview".to_owned(),
            file_version_id,
            media_snapshot_id: snapshot.id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    Seeded {
        _tmp: tmp,
        _dir: dir,
        url,
        version_id: created.version.id.0,
        input_id: input.input_set_id.0,
    }
}

fn compliance_report(url: &str, version_id: u64, input_id: u64) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            url,
            "compliance",
            "report",
            "--policy-version-id",
            &version_id.to_string(),
            "--input-set-id",
            &input_id.to_string(),
        ])
        .output()
        .unwrap()
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

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
