#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Phase C × defensive trip-wires e2e — sub-slice 7 / sprint spec §9.3.2.
//!
//! Each of the four trip-wire sub-branches that the Phase C `Applied`
//! recheck can fire ends in `state = 'recovery_required'` with the
//! matching `recovery_reason` column AND both a
//! `commit.aborted_post_mutation` event and a `commit.recovery_required`
//! event sitting alongside it. The intent row never transitions to
//! `aborted` on these branches; `abort_reason` stays NULL and the
//! dedicated `recovery_reason` column carries the trip-wire tag so
//! recovery tooling has a single source of truth.
//!
//! The `stale_target_epoch` case bumps the target location's `epoch`
//! via a direct UPDATE between authorize and finalize, the dominant
//! Phase C escape path. The resolver and lease wires can fire without
//! the durable per-row state moving; epoch drift can fire without
//! either of the other two wires firing.
//!
//! Disk-mode parity is exercised through `fresh_initialized_pool_at`
//! on a `NamedTempFile`, matching the existing
//! `commit_safety_gate.rs` / `commit_safety_gate_after_rename.rs`
//! M1 harness.

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{CommitId, FileLocationId, FileVersionId};
use voom_events::EventKind;
use voom_store::repo::commit_safety_gate::{
    AffectedScopeClosure, AliasResolver, AuthorizeOutcome, BypassKind, CommitGateContext,
    CommitGateResult, CommitPermit, CommitTarget, DestructiveCommit, FinalizeOutcome,
    ForcePathToken, MutationOutcome, PrepareOutcome, authorize_destructive_commit,
    finalize_destructive_commit, prepare_destructive_commit,
};
use voom_store::repo::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::repo::identity::{
    FileLocationKind, IdentityRepo, NewFileLocation, NewFileVersion, ProducedBy, SqliteIdentityRepo,
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

struct Seeded {
    version_id: FileVersionId,
    location_id: FileLocationId,
}

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
        version_id: version.id,
        location_id: location.id,
    }
}

async fn run_prepare_and_authorize(pool: &SqlitePool, location_id: FileLocationId) -> CommitPermit {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        gate(pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };
    let outcome = authorize_destructive_commit(
        gate(pool, &identity, &events, &resolver),
        commit_id,
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    match outcome {
        AuthorizeOutcome::Authorized(p) => p,
        AuthorizeOutcome::Blocked { result, .. } => {
            panic!("expected Authorized, got Blocked({result:?})")
        }
    }
}

async fn recovery_row(
    pool: &SqlitePool,
    commit_id: CommitId,
) -> (String, Option<String>, Option<String>) {
    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state, abort_reason, recovery_reason FROM commit_intents WHERE id = ?",
    )
    .bind(commit_id.0.cast_signed())
    .fetch_one(pool)
    .await
    .unwrap();
    row
}

async fn event_count(pool: &SqlitePool, commit_id: CommitId, kind: EventKind) -> usize {
    let events = SqliteEventRepo::new(pool.clone());
    let page = events
        .list(
            EventFilter {
                kind: Some(kind),
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

#[tokio::test]
async fn phase_c_closure_grew_lands_recovery_required_with_unified_payload() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    let permit = run_prepare_and_authorize(&pool, seeded.location_id).await;
    let commit_id = permit.commit_id();

    // Add a second live FileLocation on the same FileVersion before
    // finalize — the Phase C closure recompute observes the addition.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let _added = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: seeded.version_id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/x-added".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(2),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    assert!(matches!(
        gate_outcome.result,
        CommitGateResult::BlockedByClosureGrew { .. }
    ));

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("closure_grew"));

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );

    // Durable mutation must NOT have been applied — Phase C aborted
    // before the silent dispatch step.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}

#[tokio::test]
async fn phase_c_fresh_lease_lands_recovery_required_with_unified_payload() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    let permit = run_prepare_and_authorize(&pool, seeded.location_id).await;
    let commit_id = permit.commit_id();

    // Direct INSERT of a blocking lease on the version scope — like
    // the Phase B sibling, we exercise the trip-wire in isolation
    // (acquire_in_tx would hit the pending-commit lock).
    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_version_id, issuer_kind, issuer_ref, blocking_mode, \
          ttl_bound, clock_source, acquired_at, expires_at) \
         VALUES ('playback', ?, 'user', 'alice', 'blocking', 1, 'control_plane', ?, ?)",
    )
    .bind(seeded.version_id.0.cast_signed())
    .bind("2026-05-18T00:00:00Z")
    .bind("2026-05-19T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    assert!(matches!(
        gate_outcome.result,
        CommitGateResult::BlockedByUseLease { .. }
    ));

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("fresh_lease"));

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );
}

#[tokio::test]
async fn phase_c_closure_grew_and_fresh_lease_lands_recovery_required_with_combined_reason() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    let permit = run_prepare_and_authorize(&pool, seeded.location_id).await;
    let commit_id = permit.commit_id();

    // Both escape paths fire: a fresh alias AND a fresh blocking lease
    // between authorize and finalize.
    let identity = SqliteIdentityRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let _added = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: seeded.version_id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/x-added".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(2),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_version_id, issuer_kind, issuer_ref, blocking_mode, \
          ttl_bound, clock_source, acquired_at, expires_at) \
         VALUES ('playback', ?, 'user', 'alice', 'blocking', 1, 'control_plane', ?, ?)",
    )
    .bind(seeded.version_id.0.cast_signed())
    .bind("2026-05-18T00:00:00Z")
    .bind("2026-05-19T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    // Spec §9.3.2 step 3 third bullet: combined trip-wire returns
    // BlockedByClosureGrew (closure shift is dominant).
    assert!(matches!(
        gate_outcome.result,
        CommitGateResult::BlockedByClosureGrew { .. }
    ));

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(
        recovery_reason.as_deref(),
        Some("closure_grew_and_fresh_lease")
    );

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );
}

#[tokio::test]
async fn phase_c_stale_target_epoch_lands_recovery_required_with_drift_payload() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    let permit = run_prepare_and_authorize(&pool, seeded.location_id).await;
    let commit_id = permit.commit_id();

    // Bump the target location's epoch directly. Same member set,
    // empty lease query — but the per-member epoch comparison drifts.
    sqlx::query("UPDATE file_locations SET epoch = epoch + 1 WHERE id = ?")
        .bind(seeded.location_id.0.cast_signed())
        .execute(&pool)
        .await
        .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    let drift = match gate_outcome.result {
        CommitGateResult::BlockedByStaleTargetEpoch { drift } => drift,
        other => panic!("expected BlockedByStaleTargetEpoch, got {other:?}"),
    };
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].id, seeded.location_id.0);

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("stale_target_epoch"));

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );

    // The retired location row must still be live — Phase C did not
    // apply the durable mutation on the trip-wire branch.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}

#[tokio::test]
async fn phase_c_applied_mutation_failure_lands_recovery_required_with_reason() {
    // Round-7 finding #1: SAVEPOINT around the post-trip-wire block.
    // Force the dispatch UPDATE (retire) to fail via a BEFORE UPDATE
    // trigger; the savepoint rolls back to pre-dispatch state and the
    // outer tx commits the recovery_required transition with
    // recovery_reason='mutation_failed' + both events.
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    let permit = run_prepare_and_authorize(&pool, seeded.location_id).await;
    let commit_id = permit.commit_id();

    sqlx::query(
        "CREATE TRIGGER force_dispatch_retire_failure_e2e \
         BEFORE UPDATE OF retired_at ON file_locations \
         WHEN NEW.retired_at IS NOT NULL AND OLD.retired_at IS NULL \
         BEGIN SELECT RAISE(ABORT, 'forced for mutation_failed e2e'); END",
    )
    .execute(&pool)
    .await
    .unwrap();

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    assert!(
        matches!(
            gate_outcome.result,
            CommitGateResult::BlockedByMutationFailed { .. }
        ),
        "got {:?}",
        gate_outcome.result
    );

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("mutation_failed"));

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );

    // The retired_at column on the target row must still be NULL —
    // the savepoint rolled back the inner UPDATE.
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());

    sqlx::query("DROP TRIGGER force_dispatch_retire_failure_e2e")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn phase_c_applied_with_observed_alias_drives_recovery_required_with_merged_delta() {
    // Round-7 finding #2: the caller's observed-alias set is merged
    // into closure_final and surfaces as `added_*` entries on the
    // closure-grew trip-wire's delta. Without the merge, the
    // observed-only members would be silently dropped.
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;
    let permit = run_prepare_and_authorize(&pool, seeded.location_id).await;
    let commit_id = permit.commit_id();

    let mut observed = AffectedScopeClosure::default();
    observed
        .file_locations
        .insert(FileLocationId(seeded.location_id.0 + 7_777));

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied {
            observed: Some(observed),
        },
        T0 + Duration::seconds(3),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    let delta = match gate_outcome.result {
        CommitGateResult::BlockedByClosureGrew { delta } => delta,
        other => panic!("expected BlockedByClosureGrew, got {other:?}"),
    };
    assert!(
        delta
            .added_locations
            .contains(&FileLocationId(seeded.location_id.0 + 7_777)),
        "expected merged observed member in added_locations: {:?}",
        delta.added_locations
    );

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("closure_grew"));

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );
}

#[tokio::test]
async fn phase_c_trip_wire_recompute_failure_lands_recovery_required_with_mutation_failed() {
    // Round-8 finding #1 (critical) e2e: the Applied recovery boundary
    // covers EVERY post-Applied failure path, not just the silent
    // dispatch + completion + event append. The trip-wire recompute
    // (closure walker, lease re-eval, per-member epoch check)
    // previously ran via `?` outside the round-7 savepoint, so a
    // closure-walker failure at Phase C left the row in `'authorized'`
    // even though the caller had already mutated the filesystem.
    //
    // Force-path bypass gets the intent through prepare + authorize
    // while the resolver fails on the seeded version; Phase C
    // deliberately walks with an empty bypass set so the same
    // resolver failure becomes `VoomError::Internal` via the
    // closure-incomplete escape. The recovery boundary now routes
    // this Err to `recovery_required` with `mutation_failed`.
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_chain(&pool, "/srv/x").await;

    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new([seeded.version_id]);

    let mut bypass = std::collections::BTreeSet::new();
    bypass.insert(BypassKind::ClosureIncomplete);
    let token = ForcePathToken {
        actor: "ops@example.com".to_owned(),
        reason: "round-8 finding #1 e2e".to_owned(),
        bypass,
    };
    let outcome = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: Some(token),
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match outcome {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending with bypass, got Blocked({result:?})")
        }
    };

    let outcome = authorize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        commit_id,
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let permit = match outcome {
        AuthorizeOutcome::Authorized(p) => p,
        AuthorizeOutcome::Blocked { result, .. } => {
            panic!("expected Authorized with bypass, got Blocked({result:?})")
        }
    };

    let outcome = finalize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        permit,
        MutationOutcome::Applied { observed: None },
        T0 + Duration::seconds(2),
    )
    .await
    .unwrap();
    let gate_outcome = match outcome {
        FinalizeOutcome::Blocked(o) => o,
        other => panic!("expected Blocked, got {other:?}"),
    };
    assert!(
        matches!(
            gate_outcome.result,
            CommitGateResult::BlockedByMutationFailed { .. }
        ),
        "expected BlockedByMutationFailed, got {:?}",
        gate_outcome.result
    );

    let (state, abort_reason, recovery_reason) = recovery_row(&pool, commit_id).await;
    assert_eq!(state, "recovery_required");
    assert!(abort_reason.is_none());
    assert_eq!(recovery_reason.as_deref(), Some("mutation_failed"));

    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitAbortedPostMutation).await,
        1
    );
    assert_eq!(
        event_count(&pool, commit_id, EventKind::CommitRecoveryRequired).await,
        1
    );

    // The target row must still be live — no dispatch ran (failure
    // occurred during the trip-wire recompute, before dispatch).
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(seeded.location_id.0.cast_signed())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retired_at.is_none());
}
