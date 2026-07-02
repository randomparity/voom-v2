use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, MediaSnapshotId};
use voom_policy::{FixtureName, TargetRef, load_fixture};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::policy_inputs::PolicyInputTargetRef;

use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryRootKind, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

use crate::cases::cp;

use super::{PolicyInputFromScanInput, RootScopedScanInput, WholeScanInput};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn create_policy_input_set_round_trips_fixture() {
    let (cp, _tmp) = cp().await;
    let draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();

    let created = cp.create_policy_input_set(draft.clone()).await.unwrap();
    let fetched = cp.get_policy_input_set(created.id).await.unwrap().unwrap();

    assert_eq!(created, fetched);
    assert_eq!(created.slug, draft.slug);
}

#[tokio::test]
async fn create_policy_input_set_rejects_invalid_model() {
    let (cp, _tmp) = cp().await;
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.slug = " ".to_owned();

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
}

#[tokio::test]
async fn list_policy_input_sets_is_deterministic() {
    let (cp, _tmp) = cp().await;
    let mut b = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();
    b.slug = "b-policy-inputs".to_owned();
    b.fixture_labels = vec!["b_policy_inputs".to_owned()];
    let mut a = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    a.slug = "a-policy-inputs".to_owned();
    a.fixture_labels = vec!["a_policy_inputs".to_owned()];

    cp.create_policy_input_set(b).await.unwrap();
    cp.create_policy_input_set(a).await.unwrap();

    let listed = cp.list_policy_input_sets().await.unwrap();
    let slugs: Vec<&str> = listed.iter().map(|set| set.slug.as_str()).collect();
    assert_eq!(slugs, ["a-policy-inputs", "b-policy-inputs"]);
}

#[tokio::test]
async fn create_policy_input_set_failure_leaves_no_partial_rows() {
    let (cp, _tmp) = cp().await;
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: voom_core::MediaWorkId(9_999),
    };

    let err = cp.create_policy_input_set(draft).await.unwrap_err();
    let listed = cp.policy_inputs().list_input_sets().await.unwrap();

    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_links_existing_rows() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;

    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(created.slug, "scan-h264");
    assert_eq!(created.source_kind.as_str(), "imported");
    assert_eq!(created.file_version_id, file_version_id);
    assert_eq!(created.media_snapshot_id, media_snapshot_id);

    let input_set = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.slug, "scan-h264");
    assert_eq!(input_set.fixture_labels, ["scan-scan-h264"]);
    assert_eq!(input_set.media_snapshots.len(), 1);
    let media = &input_set.media_snapshots[0];
    assert_eq!(
        media.target,
        PolicyInputTargetRef::FileVersion {
            id: file_version_id
        }
    );
    assert_eq!(media.container.as_deref(), Some("mp4"));
    assert_eq!(media.video_codec.as_deref(), Some("h264"));
    assert_eq!(media.existing_media_snapshot_id, Some(media_snapshot_id));
}

#[tokio::test]
async fn input_from_scan_copies_snapshot_stream_facts() {
    let (cp, _tmp) = cp().await;
    let streams = json!([
        {
            "id": "stream-0",
            "index": 0,
            "kind": "video",
            "codec_name": "h264"
        },
        {
            "id": "stream-1",
            "index": 1,
            "kind": "video",
            "codec_name": "hevc"
        },
        {
            "id": "stream-2",
            "index": 2,
            "kind": "audio",
            "codec_name": "aac",
            "language": "eng"
        }
    ]);
    let payload = json!({
        "format": "test",
        "streams": streams.clone()
    });
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot_with_payload(&cp, "/srv/a.mp4", "hash-a", payload).await;

    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
        })
        .await
        .unwrap();

    let input_set = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    let media = &input_set.media_snapshots[0];

    assert_eq!(media.container.as_deref(), Some("mkv"));
    assert_eq!(media.video_codec.as_deref(), Some("hevc"));
    assert_eq!(media.stream_summary["video_stream_count"], 2);
    assert_eq!(media.stream_summary["streams"], streams);
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_missing_file_version() {
    let (cp, _tmp) = cp().await;
    let (_, media_snapshot_id) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id: FileVersionId(999_999),
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::NotFound.as_str());
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_missing_snapshot() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, _) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id,
            media_snapshot_id: MediaSnapshotId(999_999),
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::NotFound.as_str());
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_snapshot_for_other_file_version() {
    let (cp, _tmp) = cp().await;
    let (_, media_snapshot_id) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;
    let (other_file_version_id, _) = scanned_snapshot(&cp, "/srv/b.mp4", "hash-b").await;

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id: other_file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::Conflict.as_str());
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn whole_scan_includes_video_and_skips_non_video() {
    let (cp, _tmp) = cp().await;
    let video_payload = json!({
        "container": "mp4",
        "video_codec": "h264",
        "streams": [
            {"id": "v-0", "index": 0, "kind": "video", "codec_name": "h264"},
            {"id": "a-0", "index": 1, "kind": "audio", "codec_name": "aac", "language": "eng"}
        ]
    });
    let audio_payload = json!({
        "container": "mp4",
        "streams": [
            {"id": "a-0", "index": 0, "kind": "audio", "codec_name": "aac", "language": "eng"}
        ]
    });
    let (video_file_version_id, video_media_snapshot_id) =
        scanned_snapshot_with_payload(&cp, "/srv/movie.mp4", "hash-video", video_payload).await;
    let (_audio_file_version_id, _audio_media_snapshot_id) =
        scanned_snapshot_with_payload(&cp, "/srv/song.m4a", "hash-audio", audio_payload).await;

    let result = cp
        .create_policy_input_set_from_whole_scan(WholeScanInput {
            slug: "whole-library".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(result.slug, "whole-library");
    assert_eq!(result.included_count, 1);
    assert_eq!(result.skipped_count, 1);

    let input_set = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.media_snapshots.len(), 1);
    let media = &input_set.media_snapshots[0];
    assert_eq!(
        media.target,
        PolicyInputTargetRef::FileVersion {
            id: video_file_version_id
        }
    );
    assert_eq!(media.container.as_deref(), Some("mp4"));
    assert_eq!(media.video_codec.as_deref(), Some("h264"));
    assert_eq!(
        media.existing_media_snapshot_id,
        Some(video_media_snapshot_id)
    );
}

async fn library_root_at(
    cp: &crate::ControlPlane,
    slug: &str,
    path: &str,
) -> voom_core::LibraryRootId {
    let lib = cp
        .create_library(NewLibrary {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    cp.create_library_root(NewLibraryRoot {
        library_id: lib.id,
        root_kind: LibraryRootKind::LocalPath,
        canonical_path: path.to_owned(),
        display_path: path.to_owned(),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root: None,
        default_staging_root: None,
        default_backup_root: None,
        enabled: true,
    })
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn root_scoped_scan_includes_only_files_under_the_root() {
    let (cp, _tmp) = cp().await;
    let video = || {
        json!({
            "container": "mp4",
            "video_codec": "h264",
            "streams": [{"id": "v-0", "index": 0, "kind": "video", "codec_name": "h264"}]
        })
    };
    // Under the root.
    let (under, _) =
        scanned_snapshot_with_payload(&cp, "/media/films/a.mp4", "hash-a", video()).await;
    // Different library.
    scanned_snapshot_with_payload(&cp, "/media/shows/b.mp4", "hash-b", video()).await;
    // Sibling sharing a textual prefix — must NOT match.
    scanned_snapshot_with_payload(&cp, "/media/films-4k/c.mp4", "hash-c", video()).await;

    let root_id = library_root_at(&cp, "films", "/media/films").await;
    let result = cp
        .create_policy_input_set_from_root(RootScopedScanInput {
            slug: "films-input".to_owned(),
            library_root_id: root_id,
        })
        .await
        .unwrap();

    assert_eq!(result.included_count, 1);
    assert_eq!(result.skipped_count, 2);
    let input_set = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.media_snapshots.len(), 1);
    assert_eq!(
        input_set.media_snapshots[0].target,
        PolicyInputTargetRef::FileVersion { id: under }
    );
}

#[tokio::test]
async fn root_scoped_scan_missing_root_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp
        .create_policy_input_set_from_root(RootScopedScanInput {
            slug: "x".to_owned(),
            library_root_id: voom_core::LibraryRootId(999),
        })
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

async fn scanned_snapshot(
    cp: &crate::ControlPlane,
    path: &str,
    hash: &str,
) -> (FileVersionId, MediaSnapshotId) {
    scanned_snapshot_with_payload(cp, path, hash, json!({"format": "test", "streams": []})).await
}

async fn scanned_snapshot_with_payload(
    cp: &crate::ControlPlane,
    path: &str,
    hash: &str,
    payload: serde_json::Value,
) -> (FileVersionId, MediaSnapshotId) {
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
    let snapshot = cp
        .record_media_snapshot(file_version_id, None, payload, T0)
        .await
        .unwrap();
    (file_version_id, snapshot.id)
}
