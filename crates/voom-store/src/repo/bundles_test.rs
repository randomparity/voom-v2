use super::*;

use time::Duration;

use crate::repo::identity::{
    IdentityRepo, MediaWorkKind, NewMediaVariant, NewMediaWork, SqliteIdentityRepo,
};
use crate::test_support::{T0, fresh_initialized_pool_at};

async fn fresh() -> (
    SqliteBundleRepo,
    SqliteIdentityRepo,
    voom_core::MediaVariantId,
    voom_core::FileAssetId,
    voom_core::FileAssetId,
    tempfile::NamedTempFile,
) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let id_repo = SqliteIdentityRepo::new(pool.clone());
    let bun_repo = SqliteBundleRepo::new(pool.clone());
    let mw = id_repo
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "Solaris".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let mv = id_repo
        .create_media_variant(NewMediaVariant {
            media_work_id: mw.id,
            label: "4K".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let asset_a = id_repo.create_file_asset(T0).await.unwrap();
    let asset_b = id_repo.create_file_asset(T0).await.unwrap();
    (bun_repo, id_repo, mv.id, asset_a.id, asset_b.id, tmp)
}

#[tokio::test]
async fn create_and_list_bundle() {
    let (bun, _id, mv_id, _a, _b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "Movie+Subs".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(bundle.epoch, 0);
    let list = bun.list_by_variant(mv_id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, bundle.id);
    let got = bun.get(bundle.id).await.unwrap().unwrap();
    assert_eq!(got.display_name, "Movie+Subs");
}

#[tokio::test]
async fn add_member_then_remove_member() {
    let (bun, _id, mv_id, a, _b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "B".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    bun.add_member(NewBundleMember {
        bundle_id: bundle.id,
        file_asset_id: a,
        role: BundleMemberRole::PrimaryVideo,
    })
    .await
    .unwrap();
    let members = bun.list_members(bundle.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].file_asset_id, a);
    // Remove succeeds.
    let mut tx = bun.pool.begin().await.unwrap();
    bun.remove_member_in_tx(&mut tx, bundle.id, a)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(bun.list_members(bundle.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn external_audio_member_role_round_trips() {
    let (bun, _id, mv_id, _a, b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "Movie+ExtractedAudio".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();

    let member = bun
        .add_member(NewBundleMember {
            bundle_id: bundle.id,
            file_asset_id: b,
            role: BundleMemberRole::ExternalAudio,
        })
        .await
        .unwrap();

    assert_eq!(member.role, BundleMemberRole::ExternalAudio);
    let members = bun.list_members(bundle.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].role, BundleMemberRole::ExternalAudio);
}

#[tokio::test]
async fn add_member_rejects_duplicate_file_asset_membership() {
    let (bun, _id, mv_id, a, _b, _tmp) = fresh().await;
    let bundle1 = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "first".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle2 = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "second".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    bun.add_member(NewBundleMember {
        bundle_id: bundle1.id,
        file_asset_id: a,
        role: BundleMemberRole::PrimaryVideo,
    })
    .await
    .unwrap();
    let err = bun
        .add_member(NewBundleMember {
            bundle_id: bundle2.id,
            file_asset_id: a,
            role: BundleMemberRole::ExternalSubtitle,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn get_member_by_file_asset_in_tx_returns_existing_membership() {
    let (bun, _id, mv_id, a, _b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "primary".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    bun.add_member(NewBundleMember {
        bundle_id: bundle.id,
        file_asset_id: a,
        role: BundleMemberRole::PrimaryVideo,
    })
    .await
    .unwrap();

    let mut tx = bun.pool.begin().await.unwrap();
    let found = bun
        .get_member_by_file_asset_in_tx(&mut tx, a)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(found.bundle_id, bundle.id);
    assert_eq!(found.file_asset_id, a);
    assert_eq!(found.role, BundleMemberRole::PrimaryVideo);
}

#[tokio::test]
async fn remove_member_for_unknown_pair_returns_not_found() {
    let (bun, _id, mv_id, a, _b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "B".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = bun.pool.begin().await.unwrap();
    let err = bun
        .remove_member_in_tx(&mut tx, bundle.id, a)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got: {err:?}");
}

#[tokio::test]
async fn update_display_name_bumps_epoch_and_gate_on_stale_epoch() {
    let (bun, _id, mv_id, _a, _b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "before".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = bun.pool.begin().await.unwrap();
    let updated = bun
        .update_display_name_in_tx(&mut tx, bundle.id, "after".to_owned(), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(updated.display_name, "after");
    assert_eq!(updated.epoch, 1);
    let mut tx = bun.pool.begin().await.unwrap();
    let err = bun
        .update_display_name_in_tx(&mut tx, bundle.id, "x".to_owned(), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn deleting_bundle_cascades_to_members() {
    let (bun, _id, mv_id, a, _b, _tmp) = fresh().await;
    let bundle = bun
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "B".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    bun.add_member(NewBundleMember {
        bundle_id: bundle.id,
        file_asset_id: a,
        role: BundleMemberRole::PrimaryVideo,
    })
    .await
    .unwrap();
    // Hit the underlying pool to DELETE the bundle row; the ON DELETE
    // CASCADE on asset_bundle_members.bundle_id should drop the membership.
    sqlx::query("DELETE FROM asset_bundles WHERE id = ?")
        .bind(i64_from_u64(bundle.id.0))
        .execute(&bun.pool)
        .await
        .unwrap();
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM asset_bundle_members WHERE bundle_id = ?")
            .bind(i64_from_u64(bundle.id.0))
            .fetch_one(&bun.pool)
            .await
            .unwrap();
    assert_eq!(count.0, 0);
    // Variant + duration imports stay alive — silence the lint.
    let _ = Duration::seconds(0);
}
