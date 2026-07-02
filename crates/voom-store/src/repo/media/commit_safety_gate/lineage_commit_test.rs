#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]

use super::*;

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::ids::{FileAssetId, FileVersionId};

use crate::repo::media::bundles::{
    BundleMemberRole, NewAssetBundle, NewBundleMember, SqliteBundleRepo,
};
use crate::repo::media::identity::{
    FileLocationKind, MediaWorkKind, NewFileLocation, NewFileVersion, NewMediaVariant,
    NewMediaWork, ProducedBy, SqliteIdentityRepo,
};
use crate::repo::media::use_leases::{
    BlockingMode, IssuerKind, NewUseLease, SqliteUseLeaseRepo, UseLeaseKind, UseLeaseReleaseReason,
};
use crate::test_support::{T0, fresh_initialized_pool_at};

/// A seeded asset → version → live-location chain.
#[expect(
    clippy::struct_field_names,
    reason = "every field is an ID by design; the `_id` postfix is the codebase convention (see SeededLocation in commit_safety_gate_test.rs)"
)]
struct Seed {
    asset_id: FileAssetId,
    version_id: FileVersionId,
    location_id: voom_core::FileLocationId,
}

async fn fresh_pool() -> (SqlitePool, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

async fn seed(pool: &SqlitePool, value: &str) -> Seed {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let asset = identity.create_file_asset(T0).await.unwrap();
    let version = identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: format!("hash-{value}"),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: value.to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Seed {
        asset_id: asset.id,
        version_id: version.id,
        location_id: location.id,
    }
}

/// Attach `asset_id` to a fresh bundle and return the bundle id.
async fn bundle_for_asset(pool: &SqlitePool, asset_id: FileAssetId) -> voom_core::BundleId {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let bundles = SqliteBundleRepo::new(pool.clone());
    let work = identity
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "T".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let variant = identity
        .create_media_variant(NewMediaVariant {
            media_work_id: work.id,
            label: "v".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle = bundles
        .create(NewAssetBundle {
            media_variant_id: variant.id,
            display_name: "b".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    bundles
        .add_member(NewBundleMember {
            bundle_id: bundle.id,
            file_asset_id: asset_id,
            role: BundleMemberRole::PrimaryVideo,
        })
        .await
        .unwrap();
    bundle.id
}

async fn acquire(pool: &SqlitePool, input: NewUseLease) -> voom_core::UseLeaseId {
    SqliteUseLeaseRepo::new(pool.clone())
        .acquire(input)
        .await
        .unwrap()
        .id
}

fn ttl_lease(scope: LeaseScope, mode: BlockingMode) -> NewUseLease {
    NewUseLease {
        kind: UseLeaseKind::Playback,
        scope,
        issuer_kind: IssuerKind::User,
        issuer_ref: "alice".to_owned(),
        blocking_mode: mode,
        ttl: Some(Duration::seconds(60)),
        acquired_at: T0,
    }
}

async fn run(pool: &SqlitePool, seed: &Seed, now: time::OffsetDateTime) -> LineageCommitLeaseCheck {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let out =
        check_lineage_commit_leases_in_tx(&mut tx, &identity, seed.asset_id, seed.version_id, now)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    out
}

const T1: time::OffsetDateTime = T0; // seed time; checks run at T0 + delta below.

#[tokio::test]
async fn no_leases_yields_empty_check() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert!(out.blocking.is_none());
    assert!(out.evaluated_lease_ids.is_empty());
    // The closure is anchored on the commit's asset/version/location.
    assert!(out.closure.file_assets.contains(&s.asset_id));
    assert!(out.closure.file_versions.contains(&s.version_id));
    assert!(out.closure.file_locations.contains(&s.location_id));
}

#[tokio::test]
async fn blocking_lease_on_asset_scope_blocks() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let id = acquire(
        &pool,
        ttl_lease(LeaseScope::Asset(s.asset_id), BlockingMode::Blocking),
    )
    .await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert_eq!(out.blocking, Some((id, LeaseScope::Asset(s.asset_id))));
    assert!(out.evaluated_lease_ids.contains(&id));
}

#[tokio::test]
async fn blocking_lease_on_version_scope_blocks() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let id = acquire(
        &pool,
        ttl_lease(LeaseScope::Version(s.version_id), BlockingMode::Blocking),
    )
    .await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert_eq!(out.blocking, Some((id, LeaseScope::Version(s.version_id))));
}

#[tokio::test]
async fn blocking_lease_on_location_scope_blocks() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let id = acquire(
        &pool,
        ttl_lease(LeaseScope::Location(s.location_id), BlockingMode::Blocking),
    )
    .await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert_eq!(
        out.blocking,
        Some((id, LeaseScope::Location(s.location_id)))
    );
}

#[tokio::test]
async fn blocking_lease_on_bundle_scope_blocks() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let bundle_id = bundle_for_asset(&pool, s.asset_id).await;
    let id = acquire(
        &pool,
        ttl_lease(LeaseScope::Bundle(bundle_id), BlockingMode::Blocking),
    )
    .await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert_eq!(out.blocking, Some((id, LeaseScope::Bundle(bundle_id))));
    assert!(out.closure.bundles.contains(&bundle_id));
}

#[tokio::test]
async fn advisory_lease_is_evaluated_but_does_not_block() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let id = acquire(
        &pool,
        ttl_lease(LeaseScope::Version(s.version_id), BlockingMode::Advisory),
    )
    .await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert!(out.blocking.is_none());
    assert!(out.evaluated_lease_ids.contains(&id));
}

#[tokio::test]
async fn released_terminal_lease_does_not_block() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
    let lease = leases
        .acquire(ttl_lease(
            LeaseScope::Version(s.version_id),
            BlockingMode::Blocking,
        ))
        .await
        .unwrap();
    leases
        .release(
            lease.id,
            UseLeaseReleaseReason::Released,
            T1 + Duration::seconds(1),
        )
        .await
        .unwrap();
    let out = run(&pool, &s, T1 + Duration::seconds(2)).await;
    assert!(out.blocking.is_none());
    assert!(out.evaluated_lease_ids.is_empty());
}

#[tokio::test]
async fn ttl_expired_lease_does_not_block() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    // TTL lease acquired at T0 with a 60s TTL → expires at T0+60s.
    let id = acquire(
        &pool,
        ttl_lease(LeaseScope::Version(s.version_id), BlockingMode::Blocking),
    )
    .await;
    // Evaluate well past expiry; the row still has release_reason = NULL.
    let out = run(&pool, &s, T1 + Duration::seconds(120)).await;
    assert!(out.blocking.is_none(), "expired TTL lease must not block");
    assert!(!out.evaluated_lease_ids.contains(&id));
}

#[tokio::test]
async fn manual_lock_blocks_regardless_of_time() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let id = acquire(
        &pool,
        NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Version(s.version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: None,
            acquired_at: T0,
        },
    )
    .await;
    // Far in the future — a manual lock is not TTL-bound and keeps blocking.
    let out = run(&pool, &s, T1 + Duration::days(365)).await;
    assert_eq!(out.blocking, Some((id, LeaseScope::Version(s.version_id))));
}

#[tokio::test]
async fn lease_on_unrelated_asset_does_not_block() {
    let (pool, _tmp) = fresh_pool().await;
    let s = seed(&pool, "/srv/a").await;
    let other = seed(&pool, "/srv/b").await;
    let _id = acquire(
        &pool,
        ttl_lease(
            LeaseScope::Version(other.version_id),
            BlockingMode::Blocking,
        ),
    )
    .await;
    let out = run(&pool, &s, T1 + Duration::seconds(1)).await;
    assert!(out.blocking.is_none());
    assert!(out.evaluated_lease_ids.is_empty());
}
