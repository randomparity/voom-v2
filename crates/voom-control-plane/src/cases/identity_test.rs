use serde_json::json;
use time::{Duration, OffsetDateTime};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IngestOutcome, LocationProof, MediaWorkKind, NewMediaWork,
};

use crate::cases::cp;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 200,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

#[tokio::test]
async fn create_media_work_emits_event() {
    let (cp, _tmp) = cp().await;
    let mw = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "Solaris".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(mw.display_title, "Solaris");
    assert_eq!(count(&cp, EventKind::MediaWorkCreated).await, 1);
}

#[tokio::test]
async fn record_discovered_file_emits_full_event_chain() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/a.mkv".to_owned(),
                content_hash: "h-x".to_owned(),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset { .. } = outcome else {
        panic!("expected NewFileAsset");
    };
    assert_eq!(count(&cp, EventKind::FileAssetCreated).await, 1);
    assert_eq!(count(&cp, EventKind::FileVersionCreated).await, 1);
    assert_eq!(count(&cp, EventKind::FileLocationRecorded).await, 1);
    // No alias_proof supplied → no path_rule evidence event.
    // No prior hash → no hash_match evidence event.
    assert_eq!(count(&cp, EventKind::IdentityEvidenceRecorded).await, 0);
}

#[tokio::test]
async fn record_discovered_file_hash_match_emits_evidence_event() {
    let (cp, _tmp) = cp().await;
    // First discovery — seeds the hash.
    let _ = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/a.mkv".to_owned(),
                content_hash: "h-dup".to_owned(),
                size_bytes: 10,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    // Second discovery with same hash — new asset, hash_match evidence
    // event must be emitted.
    let _ = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/b.mkv".to_owned(),
                content_hash: "h-dup".to_owned(),
                size_bytes: 10,
                observed_at: T0 + Duration::seconds(1),
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::IdentityEvidenceRecorded).await, 1);
}

#[tokio::test]
async fn reconcile_rename_emits_paired_move_events() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/old.mkv".to_owned(),
                content_hash: "h".to_owned(),
                size_bytes: 1,
                observed_at: T0,
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 7,
                    generation: 1,
                }),
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!("expected NewFileAsset");
    };
    let before_retired = count(&cp, EventKind::FileLocationRetiredByMove).await;
    let before_recorded = count(&cp, EventKind::FileLocationRecordedByMove).await;
    let result = cp
        .reconcile_rename(
            voom_store::repo::identity::RenameProof::LocalFileIdGeneration {
                prior_location_id: file_location_id,
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 7,
                generation: 1,
                prior_path_missing: true,
            },
            voom_store::repo::identity::ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 1,
            },
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap();
    assert_eq!(result.retired_location_id, file_location_id);
    assert_eq!(
        count(&cp, EventKind::FileLocationRetiredByMove).await,
        before_retired + 1
    );
    assert_eq!(
        count(&cp, EventKind::FileLocationRecordedByMove).await,
        before_recorded + 1
    );
    // path_rule_match evidence emitted on the new location.
    assert!(count(&cp, EventKind::IdentityEvidenceRecorded).await >= 1);
}

#[tokio::test]
async fn record_media_snapshot_emits_event() {
    let (cp, _tmp) = cp().await;
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/x.mkv".to_owned(),
                content_hash: "h".to_owned(),
                size_bytes: 1,
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
        panic!("expected NewFileAsset");
    };
    let _ = cp
        .record_media_snapshot(
            file_version_id,
            None,
            json!({"streams": []}),
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::MediaSnapshotRecorded).await, 1);
}
