use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{FileAssetId, FileLocationId, FileVersionId, UseLeaseId};

use super::*;
use crate::repo::identity::{IdentityRepo, SqliteIdentityRepo};
use crate::test_support::{T0, fresh_initialized_pool_at};

/// Spin up a fresh pool with migration 0004 applied, plus a single
/// `file_assets` row so tests have a live scope to attach leases to.
async fn pool_with_asset() -> (SqlitePool, NamedTempFile, FileAssetId) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let asset = SqliteIdentityRepo::new(pool.clone())
        .create_file_asset(T0)
        .await
        .unwrap();
    (pool, tmp, asset.id)
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
async fn heartbeat_twice_does_not_inflate_ttl() {
    // Regression: anchoring `ttl` on `acquired_at` after the first
    // heartbeat already shifted `expires_at` forward inflated the
    // derived TTL on every subsequent heartbeat (60s → 90s → 150s …).
    // The anchor must be `last_heartbeat_at` once set, falling back to
    // `acquired_at` only on the first heartbeat.
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
    let _beat1 = repo
        .heartbeat(lease.id, T0 + Duration::seconds(30))
        .await
        .unwrap();
    let beat2 = repo
        .heartbeat(lease.id, T0 + Duration::seconds(60))
        .await
        .unwrap();
    // After two heartbeats with TTL = 60s, expires_at must be
    // (second heartbeat) + 60s = T0 + 120s, NOT T0 + 150s (the bug).
    assert_eq!(beat2.expires_at, Some(T0 + Duration::seconds(120)));
    assert_eq!(beat2.last_heartbeat_at, Some(T0 + Duration::seconds(60)));
    assert_eq!(beat2.epoch, 2);
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

// --- Task 8: release ---

#[tokio::test]
async fn release_with_released_marks_terminal() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
    let released = repo
        .release(
            l.id,
            UseLeaseReleaseReason::Released,
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    assert_eq!(
        released.release_reason,
        Some(UseLeaseReleaseReason::Released)
    );
    assert_eq!(released.released_at, Some(T0 + Duration::seconds(5)));
    assert!(!released.is_live());
}

#[tokio::test]
async fn release_with_superseded_marks_terminal() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::ControlPlane,
            issuer_ref: "op".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();
    let released = repo
        .release(
            l.id,
            UseLeaseReleaseReason::Superseded,
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    assert_eq!(
        released.release_reason,
        Some(UseLeaseReleaseReason::Superseded)
    );
}

#[tokio::test]
async fn release_with_expired_reason_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
    let err = repo
        .release(
            l.id,
            UseLeaseReleaseReason::Expired,
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn release_with_force_released_or_issuer_lost_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
    for bad in [
        UseLeaseReleaseReason::ForceReleased,
        UseLeaseReleaseReason::IssuerLost,
    ] {
        let err = repo
            .release(l.id, bad, T0 + Duration::seconds(5))
            .await
            .unwrap_err();
        assert!(matches!(err, VoomError::Config(_)), "{bad:?}: got {err:?}");
    }
}

#[tokio::test]
async fn release_against_already_released_is_conflict() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
        l.id,
        UseLeaseReleaseReason::Released,
        T0 + Duration::seconds(5),
    )
    .await
    .unwrap();
    let err = repo
        .release(
            l.id,
            UseLeaseReleaseReason::Released,
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

// --- Task 9: force_release ---

#[tokio::test]
async fn force_release_marks_terminal_with_force_released() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
    let out = repo
        .force_release(l.id, T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(
        out.release_reason,
        Some(UseLeaseReleaseReason::ForceReleased)
    );
    assert_eq!(out.released_at, Some(T0 + Duration::seconds(5)));
}

#[tokio::test]
async fn force_release_accepts_manual_locks_too() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
    let out = repo
        .force_release(l.id, T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(
        out.release_reason,
        Some(UseLeaseReleaseReason::ForceReleased)
    );
}

#[tokio::test]
async fn force_release_against_terminal_is_conflict() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
        l.id,
        UseLeaseReleaseReason::Released,
        T0 + Duration::seconds(5),
    )
    .await
    .unwrap();
    let err = repo
        .force_release(l.id, T0 + Duration::seconds(10))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

// --- Task 10: expire_due ---

#[tokio::test]
async fn expire_due_transitions_overdue_leases() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let a = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(10)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    let b = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "w-1".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(Duration::seconds(120)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    // Now = T0 + 30s — `a` is overdue (expired at T0+10), `b` is not (T0+120).
    let report = repo.expire_due(T0 + Duration::seconds(30)).await.unwrap();
    assert_eq!(report.expired, vec![a.id]);
    let a_after = repo.get(a.id).await.unwrap().unwrap();
    let b_after = repo.get(b.id).await.unwrap().unwrap();
    assert_eq!(a_after.release_reason, Some(UseLeaseReleaseReason::Expired));
    assert!(b_after.is_live());
}

#[tokio::test]
async fn expire_due_skips_manual_locks() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let _m = repo
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
    let report = repo.expire_due(T0 + Duration::hours(24)).await.unwrap();
    assert!(report.expired.is_empty());
}

#[tokio::test]
async fn expire_due_on_clean_db_is_empty() {
    let (pool, _tmp, _asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let report = repo.expire_due(T0 + Duration::hours(1)).await.unwrap();
    assert!(report.expired.is_empty());
}

// --- Task 11: recover_stale_issuer ---

#[tokio::test]
async fn recover_stale_issuer_on_manual_lock_marks_issuer_lost() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "w-1".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();
    let out = repo
        .recover_stale_issuer(l.id, T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(out.release_reason, Some(UseLeaseReleaseReason::IssuerLost));
    assert_eq!(out.released_at, Some(T0 + Duration::seconds(5)));
}

#[tokio::test]
async fn recover_stale_issuer_on_ttl_lease_is_rejected() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
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
    let err = repo
        .recover_stale_issuer(l.id, T0 + Duration::seconds(5))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn recover_stale_issuer_against_terminal_is_conflict() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let l = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Asset(asset),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "w-1".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();
    repo.force_release(l.id, T0 + Duration::seconds(5))
        .await
        .unwrap();
    let err = repo
        .recover_stale_issuer(l.id, T0 + Duration::seconds(10))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

// --- Task 12: reanchor_on_move ---

#[tokio::test]
async fn reanchor_on_move_updates_all_live_leases() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    // Seed a file_version + two file_locations to use as the rename pair.
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         created_at) VALUES (?, 'hash', 1, 'ingest', ?)",
    )
    .bind(i64::try_from(asset.0).unwrap())
    .bind(
        T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let now_iso = T0
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap();
    let loc_old = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/old', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let loc_new = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/new', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let loc_old = FileLocationId(u64::try_from(loc_old).unwrap());
    let loc_new = FileLocationId(u64::try_from(loc_new).unwrap());

    let repo = SqliteUseLeaseRepo::new(pool);
    // Two leases on the old location: one TTL-bound blocking, one manual advisory.
    let a = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(loc_old),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    let b = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Location(loc_old),
            issuer_kind: IssuerKind::ControlPlane,
            issuer_ref: "op".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();

    let report = repo
        .reanchor_on_move(loc_old, loc_new, T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(report.reanchored.len(), 2);
    assert!(report.reanchored.contains(&a.id));
    assert!(report.reanchored.contains(&b.id));

    let a_after = repo.get(a.id).await.unwrap().unwrap();
    let b_after = repo.get(b.id).await.unwrap().unwrap();
    assert_eq!(a_after.scope, LeaseScope::Location(loc_new));
    assert_eq!(b_after.scope, LeaseScope::Location(loc_new));
    // Epoch bumped:
    assert_eq!(a_after.epoch, 1);
    assert_eq!(b_after.epoch, 1);
    // Other fields preserved (per §9.2 last paragraph):
    assert_eq!(a_after.acquired_at, a.acquired_at);
    assert_eq!(a_after.expires_at, a.expires_at);
    assert_eq!(b_after.acquired_at, b.acquired_at);
}

#[tokio::test]
async fn reanchor_skips_terminal_leases() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         created_at) VALUES (?, 'hash', 1, 'ingest', ?)",
    )
    .bind(i64::try_from(asset.0).unwrap())
    .bind(
        T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let now_iso = T0
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap();
    let loc_old = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/old', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let loc_new = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/new', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let loc_old = FileLocationId(u64::try_from(loc_old).unwrap());
    let loc_new = FileLocationId(u64::try_from(loc_new).unwrap());

    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(loc_old),
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
    let report = repo
        .reanchor_on_move(loc_old, loc_new, T0 + Duration::seconds(10))
        .await
        .unwrap();
    assert!(report.reanchored.is_empty());
    let l_after = repo.get(lease.id).await.unwrap().unwrap();
    assert_eq!(l_after.scope, LeaseScope::Location(loc_old));
}

#[tokio::test]
async fn reanchor_on_move_with_no_matching_leases_is_empty() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         created_at) VALUES (?, 'h', 1, 'ingest', ?)",
    )
    .bind(i64::try_from(asset.0).unwrap())
    .bind(
        T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let now_iso = T0
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap();
    let loc_old = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/old', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let loc_new = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/new', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let repo = SqliteUseLeaseRepo::new(pool);
    let report = repo
        .reanchor_on_move(
            FileLocationId(u64::try_from(loc_old).unwrap()),
            FileLocationId(u64::try_from(loc_new).unwrap()),
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert!(report.reanchored.is_empty());
}

/// `retired == new` is a contractual no-op. The repo must return an
/// empty report without touching any rows — case-handler drain loops
/// rely on this to terminate, since the candidate scan keys on
/// `scope_location_id = retired` and an update setting
/// `scope_location_id = retired` would leave every row still matching
/// the filter and re-pick the same batch forever.
#[tokio::test]
async fn reanchor_on_move_with_same_location_is_noop() {
    let (pool, _tmp, asset) = pool_with_asset().await;
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         created_at) VALUES (?, 'hash', 1, 'ingest', ?)",
    )
    .bind(i64::try_from(asset.0).unwrap())
    .bind(
        T0.format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let now_iso = T0
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap();
    let loc = sqlx::query(
        "INSERT INTO file_locations (file_version_id, kind, value, observed_at) VALUES (?, \
         'local_path', '/same', ?)",
    )
    .bind(version_id)
    .bind(&now_iso)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let loc = FileLocationId(u64::try_from(loc).unwrap());

    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(loc),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let report = repo
        .reanchor_on_move(loc, loc, T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert!(report.reanchored.is_empty());

    // The lease must be untouched: same scope, same epoch.
    let after = repo.get(lease.id).await.unwrap().unwrap();
    assert_eq!(after.scope, LeaseScope::Location(loc));
    assert_eq!(after.epoch, lease.epoch);
}

// --- M3 Phase 2 commit 5: pending-commit lock retrofit ---
//
// `acquire_in_tx` consults `consult_pending_commit_lock_in_tx` after the
// scope-liveness probe. A live `commit_intents` row in
// state IN ('pending','authorized') whose `commit_intent_scope_members`
// row covers the requested `LeaseScope` rejects with `Conflict`. Tests
// here both pin the new rejection and verify the no-in-flight-commit
// path is unchanged.

use crate::repo::commit_safety_gate::{
    CommitTarget, DestructiveCommit, PrepareOutcome, prepare_destructive_commit,
};
use crate::repo::events::SqliteEventRepo;
use crate::repo::identity::{NewFileLocation, NewFileVersion, ProducedBy};
use crate::test_support::FailingAliasResolver;

/// Seed an `(asset, version, location)` chain so a `DeleteFileLocation`
/// commit intent has a concrete closure to populate.
async fn seed_pool_with_location() -> (
    SqlitePool,
    NamedTempFile,
    FileAssetId,
    FileVersionId,
    FileLocationId,
) {
    use crate::repo::identity::{FileLocationKind, IdentityRepo};
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let identity = SqliteIdentityRepo::new(pool.clone());
    let asset = identity.create_file_asset(T0).await.unwrap();
    let version = identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "h".to_owned(),
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
                value: "/srv/x".to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (pool, tmp, asset.id, version.id, location.id)
}

/// Land a `state = 'pending'` `commit_intents` row plus its
/// `commit_intent_scope_members` rows by running Phase A against the
/// supplied location.
async fn seed_pending_intent(pool: &SqlitePool, location_id: FileLocationId) {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        pool,
        &identity,
        &events,
        &resolver,
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(location_id),
            accepted_evidence_ids: Vec::new(),
        },
        T0,
    )
    .await
    .unwrap();
    match outcome {
        PrepareOutcome::Pending(_) => {}
        PrepareOutcome::Blocked { result, .. } => {
            panic!("seed_pending_intent: expected Pending, got Blocked({result:?})")
        }
    }
}

#[tokio::test]
async fn acquire_blocking_lease_rejects_when_pending_commit_covers_scope() {
    // In-flight commit on a FileLocation puts its asset / version /
    // location into commit_intent_scope_members. A `Version`-scoped
    // blocking lease overlaps the version row → Conflict.
    let (pool, _tmp, _asset, version_id, location_id) = seed_pool_with_location().await;
    seed_pending_intent(&pool, location_id).await;

    let repo = SqliteUseLeaseRepo::new(pool);
    let err = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(version_id),
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

#[tokio::test]
async fn acquire_advisory_lease_rejects_when_pending_commit_covers_scope() {
    // Advisory leases also consult the pending-commit lock — the lock
    // protects identity invariants, not just blocking-mode arbitration.
    let (pool, _tmp, _asset, _version, location_id) = seed_pool_with_location().await;
    seed_pending_intent(&pool, location_id).await;

    let repo = SqliteUseLeaseRepo::new(pool);
    let err = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Location(location_id),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "scanner".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn acquire_succeeds_when_no_in_flight_commit_covers_scope() {
    // With no `commit_intents` row in 'pending'/'authorized', the lock
    // helper returns None and acquire's pre-lock behavior is unchanged.
    let (pool, _tmp, _asset, _version, location_id) = seed_pool_with_location().await;
    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(location_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(lease.scope, LeaseScope::Location(location_id));
    assert!(lease.is_live());
}

#[tokio::test]
async fn acquire_succeeds_on_unrelated_scope_when_pending_commit_exists() {
    // The lock is scoped: a pending commit on location A does not
    // block acquire on an unrelated file_asset.
    let (pool, _tmp, _asset_a, _ver_a, loc_a) = seed_pool_with_location().await;
    seed_pending_intent(&pool, loc_a).await;
    // Seed a second, unrelated asset.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let other = identity.create_file_asset(T0).await.unwrap();

    let repo = SqliteUseLeaseRepo::new(pool);
    let lease = repo
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(other.id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(lease.scope, LeaseScope::Asset(other.id));
}
