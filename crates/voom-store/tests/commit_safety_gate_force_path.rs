#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Force-path bypass + retrofit — sub-slice 11 of the M3 Phase 2 plan
//! (commit 10). Three scenarios:
//!
//! 1. **Valid token honored through to authorize.** A
//!    `DestructiveCommit` carrying
//!    `Some(ForcePathToken { bypass: { ClosureIncomplete } })` is
//!    prepared against a `FailingAliasResolver` that would normally
//!    drive `BlockedByClosureIncomplete`. The bypass is honored: Phase
//!    A lands a `pending` row + emits `commit.forced_override` + emits
//!    `commit.intent_recorded` (no
//!    `commit.aborted_by_closure_incomplete`). Phase B re-reads the
//!    persisted token, re-applies the same bypass, and transitions the
//!    row to `authorized`.
//!
//! 2. **Pre-commit-10 behavior preserved.** The same closure-walk
//!    scenario but with `override_token = None` still aborts
//!    unconditionally with `BlockedByClosureIncomplete`. Encodes the
//!    property that **bypass logic and audit event ship together
//!    atomically** — no in-tree caller has access to a bypass branch
//!    without the matching `commit.forced_override` audit row.
//!
//! 3. **Bypass does NOT silence the use-lease check.** A force-path
//!    token with `ClosureIncomplete` does not weaken the unrelated
//!    blocking-lease check. With a blocking lease over the closure's
//!    version, Phase A still aborts with `BlockedByUseLease` and emits
//!    `commit.aborted_by_use_lease` (not
//!    `commit.aborted_by_closure_incomplete`). The token still lands
//!    as `commit.forced_override` only on the success path.

use std::collections::BTreeSet;

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{CommitId, FileLocationId, FileVersionId};
use voom_events::{EventKind, SubjectType};
use voom_store::repo::commit_safety_gate::{
    AliasResolver, AuthorizeOutcome, BypassKind, CommitGateContext, CommitGateResult, CommitTarget,
    DestructiveCommit, ForcePathToken, PrepareOutcome, authorize_destructive_commit,
    prepare_destructive_commit,
};
use voom_store::repo::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::repo::identity::{
    FileLocationKind, IdentityRepo, NewFileLocation, NewFileVersion, ProducedBy, SqliteIdentityRepo,
};
use voom_store::repo::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, SqliteUseLeaseRepo, UseLeaseKind,
    UseLeaseRepo,
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
    version_id: FileVersionId,
    location_id: FileLocationId,
}

async fn open_pool() -> (SqlitePool, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

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

async fn override_token_column(pool: &SqlitePool, commit_id: CommitId) -> Option<String> {
    let row: (Option<String>,) =
        sqlx::query_as("SELECT override_token FROM commit_intents WHERE id = ?")
            .bind(commit_id.0.cast_signed())
            .fetch_one(pool)
            .await
            .unwrap();
    row.0
}

fn closure_incomplete_token() -> ForcePathToken {
    let mut bypass = BTreeSet::new();
    bypass.insert(BypassKind::ClosureIncomplete);
    ForcePathToken {
        actor: "ops@example.com".to_owned(),
        reason: "filesystem mount /srv/media offline; out-of-band confirmed".to_owned(),
        bypass,
    }
}

#[tokio::test]
async fn force_path_token_honored_through_prepare_and_authorize_against_failing_resolver() {
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    // Resolver fails on the seeded version — without the bypass this
    // would drive `BlockedByClosureIncomplete` in both phases.
    let resolver = FailingAliasResolver::new([seeded.version_id]);

    let token = closure_incomplete_token();
    let outcome = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: Some(token.clone()),
        },
        T0,
    )
    .await
    .unwrap();
    let intent = match outcome {
        PrepareOutcome::Pending(i) => i,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending with bypass honored, got Blocked({result:?})")
        }
    };
    // Pending row landed.
    let (state, abort_reason) = row_state(&pool, intent.commit_id).await;
    assert_eq!(state, "pending");
    assert!(abort_reason.is_none());
    // The override_token JSON is persisted atomically with the
    // intent insert; the bypass logic and the audit blob ship
    // together (no row landed without the column populated).
    let blob = override_token_column(&pool, intent.commit_id).await;
    assert!(
        blob.is_some(),
        "expected commit_intents.override_token populated on bypass path; got NULL"
    );
    let parsed: serde_json::Value = serde_json::from_str(&blob.unwrap()).unwrap();
    assert_eq!(parsed["actor"], "ops@example.com");
    assert_eq!(parsed["bypass"][0], "closure_incomplete");
    // `commit.forced_override` emitted exactly once on the same tx
    // as `commit.intent_recorded`. `commit.aborted_by_closure_incomplete`
    // did NOT fire — the bypass is the visible difference.
    assert_eq!(
        count_events(&pool, intent.commit_id, EventKind::CommitForcedOverride).await,
        1
    );
    assert_eq!(
        count_events(&pool, intent.commit_id, EventKind::CommitIntentRecorded).await,
        1
    );
    assert_eq!(
        count_events(
            &pool,
            intent.commit_id,
            EventKind::CommitAbortedByClosureIncomplete
        )
        .await,
        0
    );

    // Phase B: same resolver still fails, but the persisted token's
    // bypass is re-applied. Authorize lands `authorized`.
    let outcome = authorize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        intent.commit_id,
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let permit = match outcome {
        AuthorizeOutcome::Authorized(p) => p,
        AuthorizeOutcome::Blocked { result, .. } => {
            panic!("expected Authorized with bypass re-applied, got Blocked({result:?})")
        }
    };
    assert_eq!(permit.commit_id(), intent.commit_id);
    let (state, abort_reason) = row_state(&pool, intent.commit_id).await;
    assert_eq!(state, "authorized");
    assert!(abort_reason.is_none());
    // Authorize did NOT re-emit `commit.forced_override` — the
    // audit signal is single-shot per commit (recorded once at
    // prepare). `commit.authorized` did fire.
    assert_eq!(
        count_events(&pool, intent.commit_id, EventKind::CommitForcedOverride).await,
        1
    );
    assert_eq!(
        count_events(&pool, intent.commit_id, EventKind::CommitAuthorized).await,
        1
    );
}

#[tokio::test]
async fn pre_commit_10_behavior_preserved_without_token() {
    // Same failing resolver, same target, but `override_token = None`.
    // Phase A must abort with `BlockedByClosureIncomplete` and emit the
    // matching `commit.aborted_by_closure_incomplete` event. The
    // `commit.forced_override` event does NOT fire — the audit signal
    // and the bypass branch ship together atomically.
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new([seeded.version_id]);

    let outcome = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0,
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => {
            panic!("expected Blocked without bypass, got Pending({i:?})")
        }
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
    assert_eq!(
        count_events(&pool, commit_id, EventKind::CommitForcedOverride).await,
        0
    );
    // override_token column NULL on the aborted row.
    let blob = override_token_column(&pool, commit_id).await;
    assert!(
        blob.is_none(),
        "expected commit_intents.override_token NULL without bypass; got {blob:?}"
    );
}

#[tokio::test]
async fn force_path_bypass_does_not_silence_blocking_use_lease() {
    // A force-path token with `ClosureIncomplete` is scoped to the
    // closure-walk reachability check; it does NOT relax the
    // blocking-lease overlap check. With a blocking lease over the
    // closure's version, Phase A must still abort with
    // `BlockedByUseLease` and emit `commit.aborted_by_use_lease`.
    // `commit.forced_override` does not fire — the abort happens
    // before any pending row would have landed (two-tx pattern), so
    // the audit signal stays single-shot on the success path.
    let (pool, _tmp) = open_pool().await;
    let seeded = seed_location(&pool, "/srv/x").await;
    let leases = SqliteUseLeaseRepo::new(pool.clone());
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
    // Resolver does NOT fail here — the closure walk would have
    // succeeded; the lease is the dominant signal.
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let outcome = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(seeded.location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: Some(closure_incomplete_token()),
        },
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let (commit_id, result) = match outcome {
        PrepareOutcome::Blocked { commit_id, result } => (commit_id, result),
        PrepareOutcome::Pending(i) => panic!("expected Blocked by lease, got Pending({i:?})"),
    };
    assert!(matches!(result, CommitGateResult::BlockedByUseLease { .. }));
    let (state, abort_reason) = row_state(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("fresh_lease"));
    assert_eq!(
        count_events(&pool, commit_id, EventKind::CommitAbortedByUseLease).await,
        1
    );
    // No forced_override emitted on this path. The Phase A two-tx
    // abort lands the row before the override_token write would
    // have happened — the bypass logic and the audit event still
    // ship together atomically (both absent here).
    assert_eq!(
        count_events(&pool, commit_id, EventKind::CommitForcedOverride).await,
        0
    );
}
