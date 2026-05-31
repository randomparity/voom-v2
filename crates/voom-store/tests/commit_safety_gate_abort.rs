#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! `abort_destructive_commit` recovery-contract e2e — sub-slice 8.
//!
//! Encodes the architectural invariant from sprint spec §9.3.2 that the
//! only sanctioned post-authorize termination is
//! `finalize_destructive_commit(_, MutationOutcome::NotPerformed, _)`.
//! `abort_destructive_commit` is the *pending-only* entry point; it must
//! reject `authorized` rows with `Conflict` without touching their state
//! or epoch. Authorized rows that drift past the gate's invariants must
//! flow through Phase C's defensive trip-wires (`recovery_required`) or
//! through `finalize(_, NotPerformed)` (aborted with
//! `prior_state='authorized'`), never through this entry.
//!
//! Disk-mode parity via the M1 harness (`fresh_initialized_pool_at`).

use sqlx::SqlitePool;
use tempfile::NamedTempFile;
use time::Duration;
use voom_core::{FileLocationId, FileVersionId, VoomError};
use voom_events::EventKind;
use voom_store::repo::commit_safety_gate::{
    AbortOutcome, AbortReason, AliasResolver, AuthorizeOutcome, CommitGateContext, CommitTarget,
    DestructiveCommit, PrepareOutcome, abort_destructive_commit, authorize_destructive_commit,
    prepare_destructive_commit,
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

async fn seed_location(pool: &SqlitePool, value: &str) -> FileLocationId {
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
    location.id
}

#[tokio::test]
async fn abort_authorized_row_rejects_with_conflict_encoding_recovery_contract() {
    let (pool, _tmp) = open_pool().await;
    let location_id = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    // Prepare → authorize: row state is now 'authorized', and a
    // commit.authorized event is on the log alongside the
    // commit.intent_recorded event from prepare.
    let prepared = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match prepared {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };
    let authorize_outcome = authorize_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        commit_id,
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    let _permit = match authorize_outcome {
        AuthorizeOutcome::Authorized(p) => p,
        AuthorizeOutcome::Blocked { result, .. } => {
            panic!("expected Authorized, got Blocked({result:?})")
        }
    };

    // Capture pre-abort row body for the post-abort no-mutation assertion.
    let (state_before, abort_reason_before, epoch_before) = read_row(&pool, commit_id).await;
    assert_eq!(state_before, "authorized");
    assert!(abort_reason_before.is_none());

    // Caller-initiated abort against an authorized row must Conflict.
    let err = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + Duration::seconds(2),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");

    // Row state, epoch, and abort_reason must be untouched.
    let (state_after, abort_reason_after, epoch_after) = read_row(&pool, commit_id).await;
    assert_eq!(state_after, "authorized");
    assert!(abort_reason_after.is_none());
    assert_eq!(epoch_after, epoch_before);

    // No commit.aborted_pre_mutation event was written.
    let events_repo = SqliteEventRepo::new(pool.clone());
    let page = events_repo
        .list(
            EventFilter {
                kind: Some(EventKind::CommitAbortedPreMutation),
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
    assert!(
        page.items.is_empty(),
        "no CommitAbortedPreMutation event must materialize on a rejected abort, got {}",
        page.items.len()
    );

    // The authorized row is still reachable through the sanctioned exit:
    // an explicit caller cancellation flows through finalize(_, NotPerformed)
    // (covered by the Phase C sibling test). Asserting the row is still
    // 'authorized' here is sufficient to encode the contract at this layer.
}

#[tokio::test]
async fn abort_pending_succeeds_end_to_end_with_event_payload() {
    // Companion test: the sanctioned pending → aborted path must work
    // end-to-end through the public API, producing both the durable row
    // transition and the commit.aborted_pre_mutation event with
    // prior_state='pending'.
    let (pool, _tmp) = open_pool().await;
    let location_id = seed_location(&pool, "/srv/x").await;
    let identity = SqliteIdentityRepo::new(pool.clone());
    let events = SqliteEventRepo::new(pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());

    let prepared = prepare_destructive_commit(
        gate(&pool, &identity, &events, &resolver),
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0,
    )
    .await
    .unwrap();
    let commit_id = match prepared {
        PrepareOutcome::Pending(i) => i.commit_id,
        PrepareOutcome::Blocked { result, .. } => {
            panic!("expected Pending, got Blocked({result:?})")
        }
    };

    let outcome = abort_destructive_commit(
        &pool,
        &events,
        commit_id,
        AbortReason::OperatorCancel,
        T0 + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, AbortOutcome::Aborted { commit_id: c, .. } if c == commit_id));

    let (state, abort_reason, _epoch) = read_row(&pool, commit_id).await;
    assert_eq!(state, "aborted");
    assert_eq!(abort_reason.as_deref(), Some("operator_cancel"));

    let events_repo = SqliteEventRepo::new(pool.clone());
    let page = events_repo
        .list(
            EventFilter {
                kind: Some(EventKind::CommitAbortedPreMutation),
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
    assert_eq!(
        page.items.len(),
        1,
        "expected one CommitAbortedPreMutation event"
    );
    match &page.items[0].envelope.payload {
        voom_events::Event::CommitAbortedPreMutation(p) => {
            assert_eq!(p.prior_state, "pending");
            assert_eq!(p.reason, "operator_cancel");
        }
        other => panic!("expected CommitAbortedPreMutation payload, got {other:?}"),
    }
}

async fn read_row(
    pool: &SqlitePool,
    commit_id: voom_core::CommitId,
) -> (String, Option<String>, u64) {
    let row: (String, Option<String>, i64) =
        sqlx::query_as("SELECT state, abort_reason, epoch FROM commit_intents WHERE id = ?")
            .bind(commit_id.0.cast_signed())
            .fetch_one(pool)
            .await
            .unwrap();
    (row.0, row.1, row.2.cast_unsigned())
}
