#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Pending-commit lock retrofit — end-to-end coverage for the two call
//! sites wired in M3 Phase 2 commit 5 (`SqliteUseLeaseRepo::acquire_in_tx` and
//! `IdentityRepo::record_discovered_file_in_tx::AliasAttached`) plus the
//! architectural exemption for `IdentityRepo::reconcile_rename_in_tx`.
//! Disk-mode parity via the M1 harness mirrors the Phase A suite.

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{FileAssetId, FileLocationId, FileVersionId, VoomError};
use voom_events::EventKind;
use voom_store::repo::commit_safety_gate::{
    AliasResolver, CommitGateContext, CommitGateResult, CommitTarget, DestructiveCommit,
    PrepareOutcome, prepare_destructive_commit,
};
use voom_store::repo::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::repo::identity::{
    AliasProof, DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, LocationProof,
    NewFileLocation, NewFileVersion, ObservedBytes, ProducedBy, RenameProof, SqliteIdentityRepo,
};
use voom_store::repo::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, SqliteUseLeaseRepo, UseLeaseKind,
};
use voom_store::test_support::{FailingAliasResolver, T0, fresh_initialized_pool_at};

fn gate<'a>(
    pool: &'a SqlitePool,
    identity_repo: &'a dyn IdentityRepo,
    event_repo: &'a dyn EventRepo,
    alias_resolver: &'a dyn AliasResolver,
) -> CommitGateContext<'a> {
    CommitGateContext {
        pool,
        identity_repo,
        event_repo,
        alias_resolver,
    }
}

struct Seeded {
    asset: FileAssetId,
    version: FileVersionId,
    location: FileLocationId,
}

async fn open_pool() -> (SqlitePool, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

/// Seed one (asset, version, location) chain — pattern matches
/// `commit_safety_gate.rs` so test reading transfers.
async fn seed_chain(pool: &SqlitePool, value: &str) -> Seeded {
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
    Seeded {
        asset: asset.id,
        version: version.id,
        location: location.id,
    }
}

/// Seed a chain whose live location carries a `file_id_generation`
/// proof so alias-attach validation succeeds when re-attempted.
async fn seed_chain_with_local_proof(
    pool: &SqlitePool,
    value: &str,
    file_id: u128,
    generation: u64,
) -> Seeded {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let outcome = identity
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: value.to_owned(),
                content_hash: "h-local".to_owned(),
                size_bytes: 1,
                observed_at: T0,
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id,
                    generation,
                }),
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let IngestOutcome::NewFileAsset {
        file_asset_id,
        file_version_id,
        file_location_id,
        ..
    } = outcome
    else {
        panic!("seed must produce NewFileAsset");
    };
    Seeded {
        asset: file_asset_id,
        version: file_version_id,
        location: file_location_id,
    }
}

/// Run Phase A against `location_id` and assert a 'pending' intent landed.
async fn land_pending_intent(pool: &SqlitePool, location_id: FileLocationId) {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new(std::iter::empty::<
        FileVersionId,
    >()));
    let outcome = prepare_destructive_commit(
        gate(pool, &identity, &events, resolver.as_ref()),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0,
    )
    .await
    .unwrap();
    match outcome {
        PrepareOutcome::Pending(_) => {}
        PrepareOutcome::Blocked { result, .. } => {
            panic!("seed_pending: expected Pending, got Blocked({result:?})")
        }
    }
}

#[tokio::test]
async fn blocking_use_lease_acquire_rejects_when_in_flight_commit_covers_scope() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    land_pending_intent(&pool, seeded.location).await;

    let leases = SqliteUseLeaseRepo::new(pool.clone());
    let err = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(seeded.version),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");

    // The retrofit gates the insert: no asset_use_leases row landed.
    let leased: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM asset_use_leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leased, 0);
}

#[tokio::test]
async fn advisory_use_lease_acquire_rejects_when_in_flight_commit_covers_scope() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/y").await;
    land_pending_intent(&pool, seeded.location).await;

    let leases = SqliteUseLeaseRepo::new(pool.clone());
    let err = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Asset(seeded.asset),
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
async fn alias_attach_rejected_when_in_flight_commit_covers_file_version() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain_with_local_proof(&pool, "/srv/local/old.mkv", 1234, 1).await;
    land_pending_intent(&pool, seeded.location).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let err = identity
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/local/alias.mkv".to_owned(),
                content_hash: "h-local".to_owned(),
                size_bytes: 1,
                observed_at: T0 + Duration::seconds(2),
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 1234,
                    generation: 1,
                }),
            },
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 1234,
                generation: 1,
                prior_location_id: seeded.location,
            }),
        )
        .await
        .unwrap_err();
    drop(tx);
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");

    let live = identity
        .list_live_file_locations_by_version(seeded.version)
        .await
        .unwrap();
    assert_eq!(live.len(), 1, "alias attach must not have persisted");
    assert_eq!(live[0].id, seeded.location);
}

#[tokio::test]
async fn reconcile_rename_proceeds_against_in_flight_commit_on_same_file_version() {
    // Arch spec lines 697–708: rename is exempt. The intent row stays
    // untouched; the rename retires the prior location and inserts a
    // new one. (Phase B's closure-recheck handles drift later — that
    // contract belongs to commit 6, not this test.)
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain_with_local_proof(&pool, "/srv/local/will_move.mkv", 42, 1).await;
    land_pending_intent(&pool, seeded.location).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let outcome = identity
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: seeded.location,
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/local/moved.mkv".to_owned(),
                file_id: 42,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h-local".to_owned(),
                size_bytes: 1,
            },
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    assert_eq!(outcome.file_version_id, seeded.version);
    assert_eq!(outcome.retired_location_id, seeded.location);

    // Intent row untouched.
    let state: String =
        sqlx::query_scalar("SELECT state FROM commit_intents ORDER BY id ASC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "pending");
}

#[tokio::test]
async fn pending_lock_disk_mode_parity_survives_reconnect() {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let seeded = seed_chain(&pool, "/srv/disk").await;
    land_pending_intent(&pool, seeded.location).await;

    let leases = SqliteUseLeaseRepo::new(pool.clone());
    let err = leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(seeded.asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
    drop(pool);

    // Reopen on disk and re-prove the lock survived the reconnect.
    let pool2 = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let leases2 = SqliteUseLeaseRepo::new(pool2.clone());
    let err2 = leases2
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(seeded.asset),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err2, VoomError::Conflict(_)), "got {err2:?}");
}

#[tokio::test]
async fn second_prepare_against_overlapping_scope_is_blocked_by_pending_commit() {
    // Round-7 finding #3: prepare-vs-prepare race. Two operators that
    // prepare destructive commits on overlapping scope (same location)
    // both used to receive `pending` intents. Phase A now consults
    // consult_pending_commit_lock_in_tx for every member of
    // closure_initial before inserting the new intent — first match
    // aborts via the two-tx pattern with BlockedByPendingCommit +
    // commit.aborted_by_pending_commit.
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    land_pending_intent(&pool, seeded.location).await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new(std::iter::empty::<
        FileVersionId,
    >()));
    let outcome = prepare_destructive_commit(
        gate(&pool, &identity, &events, resolver.as_ref()),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let (blocked_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(_) => panic!("expected Blocked, got Pending"),
    };
    match result {
        CommitGateResult::BlockedByPendingCommit {
            commit_id: _,
            offending_scope,
        } => {
            assert!(
                matches!(offending_scope, LeaseScope::Location(id) if id == seeded.location),
                "got {offending_scope:?}"
            );
        }
        other => panic!("expected BlockedByPendingCommit, got {other:?}"),
    }

    // Durable row landed as `aborted` with the new abort_reason tag.
    let (state, abort_reason): (String, String) =
        sqlx::query_as("SELECT state, abort_reason FROM commit_intents WHERE id = ?")
            .bind(blocked_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason, "pending_commit");

    // Matching event row written via the two-tx pattern.
    let count = pending_commit_event_count(&pool, blocked_id).await;
    assert_eq!(count, 1, "expected one CommitAbortedByPendingCommit event");
}

async fn pending_commit_event_count(pool: &SqlitePool, commit_id: voom_core::CommitId) -> usize {
    let events = SqliteEventRepo::new(pool.clone());
    let page = events
        .list(
            EventFilter {
                kind: Some(EventKind::CommitAbortedByPendingCommit),
                subject_type: Some(voom_events::SubjectType::CommitIntent),
                subject_id: Some(commit_id.0),
                since: None,
                until: None,
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap();
    page.items.len()
}
