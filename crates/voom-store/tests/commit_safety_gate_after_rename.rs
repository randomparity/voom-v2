#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Phase B × external-rename e2e — sub-slice 6 / sprint spec §9.4.
//!
//! Sequence covered:
//!   1. `prepare_destructive_commit` against a `FileVersion` carrying a
//!      single live `FileLocation` (the rename will retire it).
//!   2. An external rename lands via `IdentityRepo::reconcile_rename_in_tx`.
//!      Renames are deliberately exempt from the pending-commit lock
//!      (arch spec lines 697-708; sprint spec §8.7, §9.2) so this step
//!      proceeds even though a commit-intent is in flight on the
//!      affected version.
//!   3. `authorize_destructive_commit` re-walks the closure and observes
//!      a non-empty `ClosureMemberDelta`: the retired prior location
//!      drops out (`removed_locations`) and the new location appears
//!      (`added_locations`).
//!   4. Phase B aborts the intent with `BlockedByClosureGrew`, transitions
//!      the row to `aborted` with `abort_reason='closure_grew'`, and
//!      emits `commit.aborted_by_closure_grew`.
//!   5. The location-scoped use lease that was anchored to the retired
//!      location has been re-anchored to the new location by the
//!      rename (M2 §9.2 behavior — re-asserted here so the e2e flow
//!      pins the cross-feature interaction).

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{CommitId, FileLocationId};
use voom_events::EventKind;
use voom_store::repo::commit_safety_gate::{
    AliasResolver, AuthorizeOutcome, CommitGateContext, CommitGateResult, CommitTarget,
    DestructiveCommit, PrepareOutcome, authorize_destructive_commit, prepare_destructive_commit,
};
use voom_store::repo::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, LocationProof, ObservedBytes,
    RenameProof, SqliteIdentityRepo,
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

async fn open_pool() -> (SqlitePool, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

async fn seed_via_ingest(pool: &SqlitePool, path: &str) -> FileLocationId {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    // Ingest path produces a FileLocation with a LocalFileIdGeneration
    // proof — required for the subsequent rename to verify same-physical-object.
    let mut tx = pool.begin().await.unwrap();
    let outcome = identity
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.to_owned(),
                content_hash: "h".to_owned(),
                size_bytes: 10,
                observed_at: T0,
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 99,
                    generation: 1,
                }),
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _ = events; // event repo unused here; ingest tx already emitted its own events.
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!("expected NewFileAsset, got {outcome:?}");
    };
    file_location_id
}

async fn aborted_state(pool: &SqlitePool, commit_id: CommitId) -> (String, Option<String>) {
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT state, abort_reason FROM commit_intents WHERE id = ?")
            .bind(commit_id.0.cast_signed())
            .fetch_one(pool)
            .await
            .unwrap();
    row
}

async fn count_event(pool: &SqlitePool, commit_id: CommitId, kind: EventKind) -> usize {
    let events = SqliteEventRepo::new(pool.clone());
    let page = events
        .list(
            EventFilter {
                kind: Some(kind),
                subject_type: Some(voom_events::SubjectType::CommitIntent),
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "e2e flow sequences prepare → external rename → authorize across three repos plus event audit; \
              splitting would scatter the §9.4 invariants under test"
)]
async fn authorize_after_external_rename_blocks_with_closure_grew_and_reanchors_lease() {
    let (pool, _tmp) = open_pool().await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let use_leases = SqliteUseLeaseRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty());

    // 1. Ingest seeds an asset → version → location chain with a
    //    LocalFileIdGeneration proof.
    let prior_location_id = seed_via_ingest(&pool, "/srv/old.mkv").await;

    // Attach a location-scoped use lease so the rename's re-anchor
    // behavior is exercised on the same tx that retires the prior row.
    let lease = use_leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::ManualLock,
            scope: LeaseScope::Location(prior_location_id),
            issuer_kind: IssuerKind::ControlPlane,
            issuer_ref: "op".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: None,
            acquired_at: T0,
        })
        .await
        .unwrap();
    let lease_before = use_leases.get(lease.id).await.unwrap().unwrap();
    assert_eq!(lease_before.scope, LeaseScope::Location(prior_location_id));

    // 2. Prepare a destructive-commit targeting the prior location.
    let outcome = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(prior_location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Pending(intent) => intent.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };

    // 3. External rename lands. `reconcile_rename_in_tx` is deliberately
    //    exempt from the pending-commit lock (architectural exemption);
    //    the call succeeds even though a commit-intent is in flight on
    //    the same version. Re-anchoring of location-scoped leases is
    //    composed in the same tx (sprint-1 design §9.2) — the
    //    `ControlPlane::reconcile_rename` orchestrator chains
    //    `reanchor_on_move_in_tx` after the rename in the same atomic
    //    boundary; we replicate that composition here so the test pins
    //    the cross-feature contract at the repo level.
    let mut tx = pool.begin().await.unwrap();
    let rename = identity
        .reconcile_rename_in_tx(
            &mut tx,
            RenameProof::LocalFileIdGeneration {
                prior_location_id,
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 99,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 10,
            },
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap();
    let _reanchored = use_leases
        .reanchor_on_move_in_tx(
            &mut tx,
            rename.retired_location_id,
            rename.new_file_location_id,
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rename.retired_location_id, prior_location_id);
    let new_location_id = rename.new_file_location_id;
    assert_ne!(new_location_id, prior_location_id);

    // Re-anchor sanity check: the lease that was anchored to the prior
    // (now retired) location is now anchored to the new location, with
    // a bumped epoch. This is M2's `reconcile_rename` behavior — the
    // e2e re-asserts it because Phase B's drift detection depends on
    // the rename leaving the old location retired in the live-listing
    // query, and the lease state must reflect the same observed-world.
    let lease_after = use_leases.get(lease.id).await.unwrap().unwrap();
    assert_eq!(
        lease_after.scope,
        LeaseScope::Location(new_location_id),
        "lease scope follows the rename"
    );
    assert!(
        lease_after.epoch > lease_before.epoch,
        "lease epoch bumps on re-anchor: {} -> {}",
        lease_before.epoch,
        lease_after.epoch
    );

    // 4. Authorize: the closure recompute sees the prior location is no
    //    longer live (retired by rename) and the new location is live
    //    (created by rename). Both deltas are non-empty → BlockedByClosureGrew.
    let outcome = authorize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        commit_id,
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let result = match outcome {
        AuthorizeOutcome::Blocked { result, .. } => result,
        AuthorizeOutcome::Authorized(p) => panic!("expected Blocked, got Authorized({p:?})"),
    };
    let delta = match result {
        CommitGateResult::BlockedByClosureGrew { delta } => delta,
        other => panic!("expected BlockedByClosureGrew, got {other:?}"),
    };
    assert!(
        delta.removed_locations.contains(&prior_location_id),
        "retired prior location appears in removed_locations: {:?}",
        delta.removed_locations
    );
    assert!(
        delta.added_locations.contains(&new_location_id),
        "rename-created location appears in added_locations: {:?}",
        delta.added_locations
    );

    // 5. Durable row state + event audit.
    let (state, reason) = aborted_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(reason.as_deref(), Some("closure_grew"));
    assert_eq!(
        count_event(&pool, commit_id, EventKind::CommitAbortedByClosureGrew).await,
        1,
    );
}
