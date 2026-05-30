use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;
use voom_core::FileVersionId;
use voom_policy::TargetRef;
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, MediaSnapshot, NewFileVersion,
    ProducedBy,
};

use crate::cases::cp;

use super::{active_version_with_snapshot, project_media_snapshot_input};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

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

#[tokio::test]
async fn active_version_with_snapshot_returns_none_for_unknown_asset() {
    let (cp, _tmp) = cp().await;

    let result = active_version_with_snapshot(cp.identity(), voom_core::FileAssetId(9_999))
        .await
        .unwrap();

    assert!(result.is_none());
}
