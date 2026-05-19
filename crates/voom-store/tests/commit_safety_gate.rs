#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Phase A of the commit safety gate — end-to-end coverage for the
//! `prepare_destructive_commit` entry point landed in commit 4 of M3
//! Phase 2. Parametrized over the four Phase A `CommitGateResult`
//! variants (Pending success, `BlockedByUseLease`, `BlockedByStaleEvidence`,
//! `BlockedByClosureIncomplete`). Disk-mode parity via the M1 harness.

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::{Duration, OffsetDateTime};
use voom_core::{CommitId, FileLocationId, FileVersionId};
use voom_events::{EventKind, SubjectType};
use voom_store::repo::commit_safety_gate::{
    AliasResolver, CommitGateResult, CommitTarget, DestructiveCommit, EvidenceDrift,
    PrepareOutcome, prepare_destructive_commit,
};
use voom_store::repo::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::repo::identity::{
    AcceptedPin, FileLocationKind, IdentityEvidenceTarget, IdentityRepo, NewFileLocation,
    NewFileVersion, NewIdentityEvidence, ProducedBy, SqliteIdentityRepo,
};
use voom_store::repo::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, SqliteUseLeaseRepo, UseLeaseKind,
    UseLeaseRepo,
};
use voom_store::test_support::{FailingAliasResolver, T0, fresh_initialized_pool_at};

struct Seeded {
    version_id: FileVersionId,
    location_id: FileLocationId,
}

async fn open_pool() -> (SqlitePool, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

/// Seed one `file_asset` + `file_version` + `file_location` chain.
async fn seed_location(pool: &SqlitePool, value: &str) -> Seeded {
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
        version_id: version.id,
        location_id: location.id,
    }
}

async fn count_events(pool: &SqlitePool, commit_id: CommitId, kind: EventKind) -> usize {
    let events = SqliteEventRepo::new(pool.clone());
    let page = events
        .list(
            EventFilter {
                kind: Some(kind),
                subject_type: Some(SubjectType::CommitIntent),
                subject_id: Some(commit_id.0),
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

async fn row_state(pool: &SqlitePool, commit_id: CommitId) -> (String, Option<String>) {
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT state, abort_reason FROM commit_intents WHERE id = ?")
            .bind(commit_id.0.cast_signed())
            .fetch_one(pool)
            .await
            .unwrap();
    row
}

fn delete_target(location_id: FileLocationId) -> DestructiveCommit {
    DestructiveCommit {
        target: CommitTarget::DeleteFileLocation(location_id),
        accepted_evidence_ids: Vec::new(),
    }
}

#[tokio::test]
async fn phase_a_success_lands_pending_intent_plus_scope_members_and_event() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new(std::iter::empty::<
        FileVersionId,
    >()));

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        resolver.as_ref(),
        delete_target(seeded.location_id),
        T0,
    )
    .await
    .unwrap();
    let intent = match outcome {
        PrepareOutcome::Pending(i) => i,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };
    let (state, abort_reason) = row_state(&pool, intent.commit_id).await;
    assert_eq!(state, "pending");
    assert!(abort_reason.is_none());
    // scope_members landed (3: asset + version + location, no bundle).
    let scope_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM commit_intent_scope_members WHERE commit_intent_id = ?",
    )
    .bind(intent.commit_id.0.cast_signed())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scope_count, 3);
    // commit.intent_recorded emitted.
    assert_eq!(
        count_events(&pool, intent.commit_id, EventKind::CommitIntentRecorded).await,
        1
    );
}

#[tokio::test]
async fn phase_a_blocked_by_use_lease_lands_aborted_intent_plus_event() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
    // Blocking lease on the version overlaps the closure's Version
    // granularity → Phase A blocks.
    leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(seeded.version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new(std::iter::empty::<
        FileVersionId,
    >()));

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        resolver.as_ref(),
        delete_target(seeded.location_id),
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(result, CommitGateResult::BlockedByUseLease { .. }));
    let (state, abort_reason) = row_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("fresh_lease"));
    assert_eq!(
        count_events(&pool, commit_id, EventKind::CommitAbortedByUseLease).await,
        1
    );
    // No pending row materialized (two-tx pattern aborts directly).
    let pending_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM commit_intents WHERE state = 'pending'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending_count, 0);
}

#[tokio::test]
async fn phase_a_blocked_by_stale_evidence_lands_aborted_intent_plus_event() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());

    // Record + accept evidence with a hash pin, then drift the hash.
    let mut tx = pool.begin().await.unwrap();
    let recorded = identity
        .record_identity_evidence_in_tx(
            &mut tx,
            NewIdentityEvidence {
                target_type: IdentityEvidenceTarget::FileVersion,
                target_id: seeded.version_id.0,
                assertion_type: voom_events::AssertionKind::HashMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "0".to_owned(),
                confidence: 1.0,
                provenance: serde_json::json!({}),
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    identity
        .accept_identity_evidence_in_tx(
            &mut tx,
            recorded.id,
            Some("alice".to_owned()),
            T0,
            AcceptedPin {
                file_version_ids: None,
                hashes: Some(serde_json::json!([[seeded.version_id.0, "hash-/srv/x"]])),
                locations: None,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE file_versions SET content_hash = 'drifted' WHERE id = ?")
        .bind(seeded.version_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new(std::iter::empty::<
        FileVersionId,
    >()));

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        resolver.as_ref(),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: vec![recorded.id],
        },
        T0 + Duration::seconds(2),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByStaleEvidence {
            drift: EvidenceDrift::PinnedHashDiffers,
            ..
        }
    ));
    let (state, abort_reason) = row_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("stale_evidence"));
    assert_eq!(
        count_events(&pool, commit_id, EventKind::CommitAbortedByStaleEvidence).await,
        1
    );
}

#[tokio::test]
async fn phase_a_blocked_by_closure_incomplete_lands_aborted_intent_plus_event() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new([seeded.version_id]));

    let outcome = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        resolver.as_ref(),
        delete_target(seeded.location_id),
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked, got Pending({i:?})"),
    };
    assert!(matches!(
        result,
        CommitGateResult::BlockedByClosureIncomplete { .. }
    ));
    let (state, abort_reason) = row_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("closure_incomplete"));
    assert_eq!(
        count_events(
            &pool,
            commit_id,
            EventKind::CommitAbortedByClosureIncomplete
        )
        .await,
        1
    );
}

#[tokio::test]
async fn phase_a_disk_mode_parity_survives_reconnect() {
    // M1 disk-mode harness: open, drive Phase A success and abort, close
    // pool, reopen, assert both rows + events persist.
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let seeded_a = seed_location(&pool, "/srv/a").await;
    let seeded_b = seed_location(&pool, "/srv/b").await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
    leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(seeded_b.version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver: Box<dyn AliasResolver> = Box::new(FailingAliasResolver::new(std::iter::empty::<
        FileVersionId,
    >()));

    let pending = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        resolver.as_ref(),
        delete_target(seeded_a.location_id),
        OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();
    let pending_id = match pending {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected pending, got Blocked({result:?})")
        }
    };
    let blocked = prepare_destructive_commit(
        &pool,
        &identity,
        &events,
        resolver.as_ref(),
        delete_target(seeded_b.location_id),
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(1),
    )
    .await
    .unwrap();
    let blocked_id = match blocked {
        PrepareOutcome::Blocked { commit_id, .. } => commit_id,
        PrepareOutcome::Pending(i) => panic!("expected blocked, got Pending({i:?})"),
    };
    drop(pool);

    // Reopen the same disk path and assert both rows + events persist.
    let pool2 = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let (state_p, reason_p) = row_state(&pool2, pending_id).await;
    assert_eq!(state_p, "pending");
    assert!(reason_p.is_none());
    let (state_b, reason_b) = row_state(&pool2, blocked_id).await;
    assert_eq!(state_b, "aborted");
    assert_eq!(reason_b.as_deref(), Some("fresh_lease"));
    assert_eq!(
        count_events(&pool2, pending_id, EventKind::CommitIntentRecorded).await,
        1
    );
    assert_eq!(
        count_events(&pool2, blocked_id, EventKind::CommitAbortedByUseLease).await,
        1
    );
}
