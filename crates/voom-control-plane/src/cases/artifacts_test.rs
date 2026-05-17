use super::*;

use time::OffsetDateTime;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};

use crate::cases::cp;

fn handle_input() -> NewArtifactHandle {
    NewArtifactHandle {
        size_bytes: Some(100),
        checksum: Some("sha256:dead".to_owned()),
        privacy_class: "internal".to_owned(),
        durability_class: "ephemeral".to_owned(),
        allowed_access_modes: vec!["read".to_owned()],
        mutability: "immutable".to_owned(),
        source_lineage: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

#[tokio::test]
async fn create_artifact_handle_emits_artifact_handle_created() {
    let (cp, _tmp) = cp().await;
    let h = cp.create_artifact_handle(handle_input()).await.unwrap();
    assert_eq!(count(&cp, EventKind::ArtifactHandleCreated).await, 1);
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::ArtifactHandleCreated),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items[0].envelope.subject_id, Some(h.id.0));
}

#[tokio::test]
async fn record_artifact_location_emits_artifact_location_recorded() {
    let (cp, _tmp) = cp().await;
    let h = cp.create_artifact_handle(handle_input()).await.unwrap();
    let loc = cp
        .record_artifact_location(NewArtifactLocation {
            artifact_handle_id: h.id,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::ArtifactLocationRecorded).await, 1);
    let _ = loc;
}

#[tokio::test]
async fn retire_artifact_location_emits_artifact_location_retired() {
    let (cp, _tmp) = cp().await;
    let h = cp.create_artifact_handle(handle_input()).await.unwrap();
    let loc = cp
        .record_artifact_location(NewArtifactLocation {
            artifact_handle_id: h.id,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.retire_artifact_location(
        loc.id,
        h.id,
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::ArtifactLocationRetired).await, 1);
}

#[tokio::test]
async fn record_artifact_lineage_emits_artifact_lineage_recorded() {
    let (cp, _tmp) = cp().await;
    let parent = cp.create_artifact_handle(handle_input()).await.unwrap();
    let child = cp.create_artifact_handle(handle_input()).await.unwrap();
    cp.record_artifact_lineage(NewArtifactLineage {
        parent_artifact_id: parent.id,
        child_artifact_id: child.id,
        operation: "transcode".to_owned(),
        recorded_at: OffsetDateTime::UNIX_EPOCH,
    })
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::ArtifactLineageRecorded).await, 1);
}
