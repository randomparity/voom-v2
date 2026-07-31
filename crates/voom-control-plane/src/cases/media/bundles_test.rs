use time::{Duration, OffsetDateTime};
use voom_events::payload::AssetBundleMemberRemovedPayload;
use voom_events::{Event, EventKind};
use voom_store::repo::bundles::{BundleMemberRole, NewAssetBundle};
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, FileVersionRepo, IngestOutcome, MediaWorkKind,
    NewFileVersion, NewMediaVariant, NewMediaWork, ProducedBy,
};

use crate::cases::{begin_immediate_tx, commit_tx, count, cp};

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

#[tokio::test]
async fn primary_bundle_for_exact_active_version_creates_once_and_reuses_without_events() {
    let (cp, _tmp) = cp().await;
    let (_, version_id) = discovered_source(&cp).await;
    let event_id_before: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(event_id), 0) FROM events")
        .fetch_one(&cp.pool)
        .await
        .unwrap();

    let mut tx = begin_immediate_tx(&cp.pool).await.unwrap();
    let created = cp
        .resolve_or_create_primary_bundle_in_tx(
            &mut tx,
            version_id,
            std::path::Path::new("/library/Movie.mkv"),
            T0,
        )
        .await
        .unwrap();
    commit_tx(tx).await.unwrap();

    assert!(created.created);
    let members = cp.list_bundle_members(created.bundle_id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].role, BundleMemberRole::PrimaryVideo);
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM events WHERE event_id > ? ORDER BY event_id ASC")
            .bind(event_id_before)
            .fetch_all(&cp.pool)
            .await
            .unwrap();
    assert_eq!(
        kinds,
        [
            "media_work.created",
            "media_variant.created",
            "asset_bundle.created",
            "asset_bundle.member_added",
        ]
    );

    let event_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&cp.pool).await.unwrap();
    let reused = cp
        .resolve_or_create_primary_bundle_in_tx(
            &mut tx,
            version_id,
            std::path::Path::new("/library/Movie.mkv"),
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap();
    commit_tx(tx).await.unwrap();

    assert!(!reused.created);
    assert_eq!(reused.bundle_id, created.bundle_id);
    let event_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(event_count_after, event_count_before);
}

#[tokio::test]
async fn primary_bundle_rejects_superseded_exact_version_without_rows_or_events() {
    let (cp, _tmp) = cp().await;
    let (asset_id, version_id) = discovered_source(&cp).await;
    cp.identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: "sha256:new".to_owned(),
            size_bytes: 20,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0 + Duration::seconds(1),
        })
        .await
        .unwrap();
    let before = durable_primary_bundle_counts(&cp).await;

    let mut tx = begin_immediate_tx(&cp.pool).await.unwrap();
    let error = cp
        .resolve_or_create_primary_bundle_in_tx(
            &mut tx,
            version_id,
            std::path::Path::new("/library/Movie.mkv"),
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, voom_core::VoomError::StaleIdentityEvidence(_)),
        "got: {error:?}"
    );
    drop(tx);
    assert_eq!(durable_primary_bundle_counts(&cp).await, before);
}

#[tokio::test]
async fn primary_bundle_rejects_existing_non_primary_membership_without_rows_or_events() {
    let (cp, _tmp) = cp().await;
    let (asset_id, version_id) = discovered_source(&cp).await;
    let work = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "Movie".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let variant = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: work.id,
            label: "source".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: variant.id,
            display_name: "Movie".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    cp.add_bundle_member(bundle.id, asset_id, BundleMemberRole::ExternalAudio, T0)
        .await
        .unwrap();
    let before = durable_primary_bundle_counts(&cp).await;

    let mut tx = begin_immediate_tx(&cp.pool).await.unwrap();
    let error = cp
        .resolve_or_create_primary_bundle_in_tx(
            &mut tx,
            version_id,
            std::path::Path::new("/library/Movie.mkv"),
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, voom_core::VoomError::Conflict(_)),
        "got: {error:?}"
    );
    drop(tx);
    assert_eq!(durable_primary_bundle_counts(&cp).await, before);
}

async fn discovered_source(
    cp: &crate::ControlPlane,
) -> (voom_core::FileAssetId, voom_core::FileVersionId) {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/library/Movie.mkv".to_owned(),
                content_hash: "sha256:source".to_owned(),
                size_bytes: 10,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_asset_id,
        file_version_id,
        ..
    } = outcome
    else {
        panic!("source must create a new file asset");
    };
    (file_asset_id, file_version_id)
}

async fn durable_primary_bundle_counts(cp: &crate::ControlPlane) -> [i64; 5] {
    let mut counts = [0; 5];
    for (index, table) in [
        "media_works",
        "media_variants",
        "asset_bundles",
        "asset_bundle_members",
        "events",
    ]
    .into_iter()
    .enumerate()
    {
        counts[index] = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&cp.pool)
            .await
            .unwrap();
    }
    counts
}
