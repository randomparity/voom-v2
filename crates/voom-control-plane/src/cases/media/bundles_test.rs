use time::{Duration, OffsetDateTime};
use voom_events::payload::AssetBundleMemberRemovedPayload;
use voom_events::{Event, EventKind};
use voom_store::repo::bundles::{BundleMemberRole, NewAssetBundle};
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{MediaWorkKind, NewMediaVariant, NewMediaWork};

use crate::cases::{count, cp};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn create_bundle_emits_event() {
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
    let mv = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: mw.id,
            label: "4K".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: mv.id,
            display_name: "primary".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(bundle.display_name, "primary");
    assert_eq!(count(&cp, EventKind::AssetBundleCreated).await, 1);
}

#[tokio::test]
async fn add_then_remove_member_emits_paired_events() {
    let (cp, _tmp) = cp().await;
    let mw = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "T".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let mv = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: mw.id,
            label: "L".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: mv.id,
            display_name: "B".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let asset = cp.create_file_asset(T0).await.unwrap();
    cp.add_bundle_member(bundle.id, asset.id, BundleMemberRole::PrimaryVideo, T0)
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::AssetBundleMemberAdded).await, 1);
    cp.remove_bundle_member(bundle.id, asset.id, T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::AssetBundleMemberRemoved).await, 1);
    assert!(cp.list_bundle_members(bundle.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn remove_bundle_member_event_role_matches_stored_row() {
    // The audit event's `role` must be derived from the persisted row,
    // not from a caller-supplied argument that a stale UI / retried call
    // could disagree with.
    let (cp, _tmp) = cp().await;
    let mw = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "T".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let mv = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: mw.id,
            label: "L".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: mv.id,
            display_name: "B".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let asset = cp.create_file_asset(T0).await.unwrap();
    cp.add_bundle_member(bundle.id, asset.id, BundleMemberRole::CommentaryAudio, T0)
        .await
        .unwrap();
    let removed = cp
        .remove_bundle_member(bundle.id, asset.id, T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(removed.role, BundleMemberRole::CommentaryAudio);
    let evs = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::AssetBundleMemberRemoved),
                ..EventFilter::default()
            },
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items;
    let Some(payload): Option<&AssetBundleMemberRemovedPayload> =
        evs.iter().find_map(|e| match &e.envelope.payload {
            Event::AssetBundleMemberRemoved(p) => Some(p),
            _ => None,
        })
    else {
        panic!("member_removed event");
    };
    assert_eq!(payload.role, "commentary_audio");
}

#[tokio::test]
async fn add_member_duplicate_returns_conflict_and_emits_no_event() {
    let (cp, _tmp) = cp().await;
    let mw = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "T".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let mv = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: mw.id,
            label: "L".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle1 = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: mv.id,
            display_name: "one".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle2 = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: mv.id,
            display_name: "two".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let asset = cp.create_file_asset(T0).await.unwrap();
    cp.add_bundle_member(bundle1.id, asset.id, BundleMemberRole::PrimaryVideo, T0)
        .await
        .unwrap();
    let before = count(&cp, EventKind::AssetBundleMemberAdded).await;
    let err = cp
        .add_bundle_member(
            bundle2.id,
            asset.id,
            BundleMemberRole::ExternalSubtitle,
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::Conflict(_)),
        "got: {err:?}"
    );
    // Failed mutation must roll back the event too.
    assert_eq!(count(&cp, EventKind::AssetBundleMemberAdded).await, before);
}
