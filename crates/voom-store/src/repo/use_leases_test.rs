use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{FileAssetId, UseLeaseId};

use super::*;
use crate::test_support::{T0, fresh_initialized_pool_at};

/// Spin up a fresh pool with migration 0004 applied, plus a single
/// `file_assets` row so tests have a live scope to attach leases to.
async fn pool_with_asset() -> (SqlitePool, NamedTempFile, FileAssetId) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(
            T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
                .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
    (pool, tmp, FileAssetId(u64::try_from(asset_id).unwrap()))
}

#[tokio::test]
async fn get_returns_none_for_unknown_id() {
    let (pool, _tmp, _asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    assert!(repo.get(UseLeaseId(99999)).await.unwrap().is_none());
}

#[tokio::test]
async fn list_for_scope_returns_empty_on_clean_db() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let listed = repo.list_for_scope(LeaseScope::Asset(asset)).await.unwrap();
    assert!(listed.is_empty());
}

// --- Task 6: acquire ---

#[tokio::test]
async fn acquire_ttl_bound_persists_expires_at() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert!(lease.ttl_bound);
    assert_eq!(lease.expires_at, Some(T0 + Duration::seconds(60)));
    assert!(lease.release_reason.is_none());
    assert_eq!(lease.epoch, 0);
    assert_eq!(lease.scope, LeaseScope::Asset(asset));
}

#[tokio::test]
async fn acquire_manual_lock_has_no_expires_at() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::ControlPlane,
            issuer_ref: "operator".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert!(!lease.ttl_bound);
    assert!(lease.expires_at.is_none());
}

#[tokio::test]
async fn acquire_ttl_bound_with_no_ttl_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    // `Playback` is a TTL-bound kind; passing `ttl: None` is a contract
    // violation surfaced as Config (callers picked the wrong combination).
    let err = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn acquire_manual_lock_with_ttl_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let err = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::ControlPlane,
            issuer_ref: "operator".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn acquire_zero_or_negative_ttl_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    for ttl in [Duration::ZERO, Duration::seconds(-1)] {
        let err = repo
            .acquire(NewUseLease {
                kind: UseLeaseKind::Playback,
                scope: LeaseScope::Asset(asset),
                issuer_kind: IssuerKind::User,
                issuer_ref: "alice".to_owned(),
                blocking_mode: BlockingMode::Blocking,
                ttl: Some(ttl),
                acquired_at: T0,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, VoomError::Config(_)),
            "ttl={ttl:?}: got {err:?}"
        );
    }
}

#[tokio::test]
async fn acquire_against_unknown_asset_is_not_found() {
    let (pool, _tmp, _real_asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let err = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(FileAssetId(99_999)),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got {err:?}");
}

#[tokio::test]
async fn acquire_against_retired_asset_is_conflict() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    // Soft-delete the asset:
    sqlx::query("UPDATE file_assets SET retired_at = ? WHERE id = ?")
        .bind(
            T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
                .unwrap(),
        )
        .bind(i64::try_from(asset.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let repo = SqliteUseLeaseRepo::new(pool);
    let err = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

// --- Task 7: heartbeat ---

#[tokio::test]
async fn heartbeat_extends_expires_at_by_original_ttl() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    let later = T0 + Duration::seconds(30);
    let beat = repo.heartbeat(lease.id, later).await.unwrap();
    assert_eq!(beat.last_heartbeat_at, Some(later));
    assert_eq!(beat.expires_at, Some(later + Duration::seconds(60)));
    assert_eq!(beat.epoch, 1);
}

#[tokio::test]
async fn heartbeat_against_manual_lock_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::ControlPlane,
            issuer_ref: "op".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();
    let err = repo
        .heartbeat(lease.id, T0 + Duration::seconds(1))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
#[ignore = "depends on Task 8 release"]
async fn heartbeat_against_terminal_lease_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    repo.release(
        lease.id,
        UseLeaseReleaseReason::Released,
        T0 + Duration::seconds(5),
    )
    .await
    .unwrap();
    let err = repo
        .heartbeat(lease.id, T0 + Duration::seconds(10))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn heartbeat_against_unknown_id_is_not_found() {
    let (pool, _tmp, _asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let err = repo.heartbeat(UseLeaseId(99_999), T0).await.unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got {err:?}");
}
