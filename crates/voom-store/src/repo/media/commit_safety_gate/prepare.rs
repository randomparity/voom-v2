use super::codecs::{encode_closure, encode_evidence_ids, encode_force_path_token, encode_target};
use super::scope::{
    build_closure, expand_scope_members, first_blocking_overlap_in_tx, first_evidence_drift,
    list_blocking_leases_in_tx, revalidate_evidence_in_tx,
};
use super::{
    AbortReason, AffectedScopeClosure, AliasResolver, BTreeSet, BypassKind, ClosureFailure,
    ClosureWarning, CommitAbortedByClosureIncompletePayload, CommitAbortedByPendingCommitPayload,
    CommitAbortedByStaleEvidencePayload, CommitAbortedByUseLeasePayload,
    CommitForcedOverridePayload, CommitGateContext, CommitGateResult, CommitId, CommitIntent,
    CommitIntentRecordedPayload, CommitTarget, DestructiveCommit, Event, EventEnvelope, EventRepo,
    EvidenceDrift, EvidenceId, EvidenceRevalidationResult, ForcePathToken, IdentityRepo,
    LeaseScope, OffsetDateTime, SqlitePool, SubjectType, UseLeaseId, VoomError, begin_gate_tx,
    bypass_kind_str, consult_pending_commit_lock_in_tx, iso8601, u64_from_i64, validate_bypass,
};

// ============================================================================
// Phase A entry point — `prepare_destructive_commit` + abort helper
// ============================================================================

/// Disposition of a `prepare_destructive_commit` call.
///
/// `Pending` carries the durable `CommitIntent` (row landed in
/// `state = 'pending'`). `Blocked` carries the abort outcome — a
/// `commit_intents` row landed in `state = 'aborted'` with `commit_id`
/// referring to that row, and the matching `commit.aborted_by_*` event
/// row sits alongside it in `events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareOutcome {
    Pending(CommitIntent),
    Blocked {
        commit_id: CommitId,
        result: CommitGateResult,
    },
}

/// Reason a Phase A gate-check aborted before any durable mutation could
/// land. The helper translates one of these into the matching
/// `AbortReason` row value AND the matching `commit.aborted_by_*` event.
#[derive(Debug, Clone)]
pub(super) enum PhaseAAbort {
    UseLease {
        lease_id: UseLeaseId,
        lease_scope: LeaseScope,
    },
    StaleEvidence {
        evidence_id: EvidenceId,
        drift: EvidenceDrift,
    },
    ClosureIncomplete {
        message: String,
    },
    /// Another in-flight `commit_intents` row (`state IN
    /// ('pending','authorized')`) already covers a scope member of the
    /// new commit's `closure_initial`. Carries the
    /// existing commit's `commit_id` and the offending scope so the
    /// blocked caller can wait / take-over without re-querying.
    PendingCommit {
        pending_commit_id: CommitId,
        offending_scope: LeaseScope,
    },
}

impl PhaseAAbort {
    fn abort_reason(&self) -> AbortReason {
        match self {
            Self::UseLease { .. } => AbortReason::FreshLease,
            Self::StaleEvidence { .. } => AbortReason::StaleEvidence,
            Self::ClosureIncomplete { .. } => AbortReason::ClosureIncomplete,
            // The pending-commit abort reuses `AbortReason::Other`
            // because the existing variant set is closed; the
            // `"pending_commit"` string is the durable column value.
            Self::PendingCommit { .. } => AbortReason::Other("pending_commit".to_owned()),
        }
    }

    fn abort_reason_str(&self) -> &'static str {
        match self {
            Self::UseLease { .. } => "fresh_lease",
            Self::StaleEvidence { .. } => "stale_evidence",
            Self::ClosureIncomplete { .. } => "closure_incomplete",
            Self::PendingCommit { .. } => "pending_commit",
        }
    }

    fn into_gate_result(self) -> CommitGateResult {
        match self {
            Self::UseLease {
                lease_id,
                lease_scope,
            } => CommitGateResult::BlockedByUseLease {
                lease_id,
                lease_scope,
            },
            Self::StaleEvidence { evidence_id, drift } => {
                CommitGateResult::BlockedByStaleEvidence { evidence_id, drift }
            }
            Self::ClosureIncomplete { message } => CommitGateResult::BlockedByClosureIncomplete {
                reason: ClosureFailure::AliasUnreachable {
                    message: message.clone(),
                },
                unreachable: vec![ClosureWarning { message }],
            },
            Self::PendingCommit {
                pending_commit_id,
                offending_scope,
            } => CommitGateResult::BlockedByPendingCommit {
                commit_id: pending_commit_id,
                offending_scope,
            },
        }
    }
}

/// Snapshot of the JSON-encoded `commit_intents` row body, captured
/// once before the closure walk so the gate's IMMEDIATE tx and the
/// two-tx abort helper bind the same column values. The four fields
/// mirror the four `commit_intents` columns the Phase A entry point
/// populates regardless of outcome.
struct CommitIntentRowBody<'a> {
    target_json: &'a str,
    closure_initial_json: &'a str,
    accepted_evidence_ids_json: &'a str,
    started_at: OffsetDateTime,
}

/// Phase A gate-check abort using the two-tx pattern (sequencing doc
/// §5.2). The two-tx pattern is **only** used for Phase A gate-check
/// aborts (raised before the `commit_intents` row would land in
/// `'pending'`). Phase B aborts, Phase C trip-wire aborts, and the
/// dedicated `abort_destructive_commit` entry point all commit the
/// intent-state transition and the event row in a single IMMEDIATE
/// transaction.
///
/// Tx 1 inserts the `commit_intents` row directly in `state = 'aborted'`
/// (no prior `pending` write — the gate check tripped before any
/// closure-bearing state landed). Tx 2 emits the matching
/// `commit.aborted_by_*` event. The split keeps the in-tx event-append
/// composition the rest of the codebase uses inaccessible from Phase A
/// abort paths, which would otherwise need to materialize an empty
/// closure into the durable `closure_initial` column under a tx the
/// gate's later phases never own.
///
/// Returns the durable `CommitId` of the aborted row.
async fn phase_a_gate_abort_with_event(
    pool: &SqlitePool,
    event_repo: &dyn EventRepo,
    row: &CommitIntentRowBody<'_>,
    aborted_at: OffsetDateTime,
    abort: PhaseAAbort,
) -> Result<CommitId, VoomError> {
    // The first leg of the two-tx abort pattern inserts the aborted
    // row. Both legs route through `begin_gate_tx` so the gate's
    // BEGIN IMMEDIATE invariant holds even on the abort path.
    let started_iso = iso8601(row.started_at)?;
    let aborted_iso = iso8601(aborted_at)?;
    let mut tx1 = begin_gate_tx(pool).await?;
    let insert = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at, \
          aborted_at, abort_reason) \
         VALUES (?, ?, ?, 'aborted', ?, ?, ?)",
    )
    .bind(row.target_json)
    .bind(row.closure_initial_json)
    .bind(row.accepted_evidence_ids_json)
    .bind(&started_iso)
    .bind(&aborted_iso)
    .bind(abort.abort_reason_str())
    .execute(&mut *tx1)
    .await
    .map_err(|e| VoomError::Database(format!("commit_intents abort insert: {e}")))?;
    let commit_id = CommitId(u64_from_i64(insert.last_insert_rowid()));
    tx1.commit()
        .await
        .map_err(|e| VoomError::Database(format!("phase A abort tx1 commit: {e}")))?;

    // two-tx: tx 2 emits the matching event.
    let payload = phase_a_abort_event(commit_id, aborted_at, &abort);
    let mut tx2 = begin_gate_tx(pool).await?;
    event_repo
        .append_in_tx(
            &mut tx2,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    tx2.commit()
        .await
        .map_err(|e| VoomError::Database(format!("phase A abort tx2 commit: {e}")))?;

    // Reference fields once so `PhaseAAbort` does not need additional
    // accessors. The abort_reason call also pins the
    // `AbortReason` ↔ `PhaseAAbort` mapping (used here for audit /
    // debug; the durable column carries the snake_case string).
    let _ = abort.abort_reason();
    Ok(commit_id)
}

fn phase_a_abort_event(
    commit_id: CommitId,
    aborted_at: OffsetDateTime,
    abort: &PhaseAAbort,
) -> Event {
    match abort {
        PhaseAAbort::UseLease {
            lease_id,
            lease_scope,
        } => Event::CommitAbortedByUseLease(CommitAbortedByUseLeasePayload {
            commit_id,
            lease_id: *lease_id,
            lease_scope_type: lease_scope.type_str().to_owned(),
            lease_scope_id: lease_scope.id_u64(),
            phase: "prepare".to_owned(),
            aborted_at,
        }),
        PhaseAAbort::StaleEvidence { evidence_id, drift } => {
            Event::CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload {
                commit_id,
                evidence_id: *evidence_id,
                drift_kind: evidence_drift_str(drift).to_owned(),
                phase: "prepare".to_owned(),
                aborted_at,
            })
        }
        PhaseAAbort::ClosureIncomplete { message } => {
            Event::CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload {
                commit_id,
                phase: "prepare".to_owned(),
                message: message.clone(),
                aborted_at,
            })
        }
        PhaseAAbort::PendingCommit {
            pending_commit_id,
            offending_scope,
        } => Event::CommitAbortedByPendingCommit(CommitAbortedByPendingCommitPayload {
            commit_id,
            pending_commit_id: *pending_commit_id,
            scope_type: offending_scope.type_str().to_owned(),
            scope_id: offending_scope.id_u64(),
            phase: "prepare".to_owned(),
            aborted_at,
        }),
    }
}

pub(super) fn evidence_drift_str(d: &EvidenceDrift) -> &'static str {
    match d {
        EvidenceDrift::PinnedFileVersionRetired => "pinned_file_version_retired",
        EvidenceDrift::PinnedHashDiffers => "pinned_hash_differs",
        EvidenceDrift::PinnedLocationRetired => "pinned_location_retired",
    }
}

pub(super) fn commit_target_kind_str(t: &CommitTarget) -> &'static str {
    match t {
        CommitTarget::DeleteFileLocation(_) => "delete_file_location",
        CommitTarget::ReplaceFileLocation { .. } => "replace_file_location",
        CommitTarget::MoveFileLocation { .. } => "move_file_location",
    }
}

// ----- Phase A main entry point --------------------------------------------

/// Phase A of the destructive-commit gate — sub-slice 4 of the M3 Phase 2
/// plan. Computes the affected-scope closure, evaluates the three Phase A
/// gate checks (blocking use-lease, accepted-evidence drift,
/// closure-walk reachability), and persists either a `state = 'pending'`
/// `commit_intents` row (success) or a `state = 'aborted'` row (gate
/// check tripped) along with the matching event.
///
/// The success path runs inside one IMMEDIATE transaction:
/// closure-walk → lease check → evidence revalidation → INSERT pending
/// row → expand `commit_intent_scope_members` → emit
/// `commit.intent_recorded` → COMMIT. The abort paths rollback the
/// gate's IMMEDIATE tx and use `phase_a_gate_abort_with_event` to land
/// the aborted row and event in two sequential transactions (sequencing
/// doc §5.2).
///
/// `alias_resolver` covers **external** (non-DB) alias sources only.
/// DB-internal alias enumeration goes through
/// `IdentityRepo::list_live_file_locations_by_version_in_tx` on the
/// gate's tx handle, preserving the gate snapshot and avoiding nested
/// connection waits.
///
/// `input.override_token` is the sanctioned force-path bypass (commit
/// 10). `None` (the default) routes any `AliasResolutionError::Unreachable`
/// from the closure walker straight to `BlockedByClosureIncomplete`.
/// `Some(token)` after `validate_bypass` accepts the token funnels the
/// matching `Unreachable` into the bypass branch (the closure walk
/// proceeds with whatever DB-internal aliases were already enumerated;
/// the external resolver's contribution is lost). The token JSON is
/// persisted to `commit_intents.override_token` atomically with the
/// `commit.intent_recorded` insert; `commit.forced_override` is emitted
/// once at prepare time (authorize does not re-emit). The audit signal
/// and the bypass logic ship together in this same tx — no in-tree
/// caller has access to a bypass branch without the matching audit row.
///
/// # Errors
///
/// `VoomError::Config` if `input.override_token = Some(token)` and the
/// token's bypass set contains an unsupported `BypassKind` (validation
/// runs before any tx opens; no row materializes). `VoomError::Database`
/// / `VoomError::Internal` on storage failures (including
/// `AliasResolutionError::Database` from an external alias source).
/// Gate-check failures return `Ok(PrepareOutcome::Blocked)` rather than
/// `Err` — `Err` is reserved for genuine storage failures that the
/// caller cannot reason about.
pub async fn prepare_destructive_commit(
    context: CommitGateContext<'_>,
    input: DestructiveCommit,
    now: OffsetDateTime,
) -> Result<PrepareOutcome, VoomError> {
    let CommitGateContext {
        pool,
        identity_repo,
        event_repo,
        alias_resolver,
    } = context;
    let DestructiveCommit {
        target,
        accepted_evidence_ids,
        override_token,
    } = input;

    // Validate the token before opening any tx — an invalid bypass bit
    // never lands a commit_intents row.
    if let Some(token) = &override_token {
        validate_bypass(token)?;
    }

    let target_json = encode_target(&target)?;
    let accepted_evidence_ids_json = encode_evidence_ids(&accepted_evidence_ids)?;
    let override_token_json = match &override_token {
        None => None,
        Some(token) => Some(encode_force_path_token(token)?),
    };
    let bypass_set: BTreeSet<BypassKind> = override_token
        .as_ref()
        .map(|t| t.bypass.clone())
        .unwrap_or_default();

    let mut tx = begin_gate_tx(pool).await?;

    let walk_outcome = run_phase_a_gate_in_tx(
        &mut tx,
        identity_repo,
        alias_resolver,
        &target,
        &accepted_evidence_ids,
        &bypass_set,
    )
    .await;
    let walk = match walk_outcome {
        Ok(Ok(w)) => w,
        Ok(Err(abort_outcome)) => {
            tx.rollback()
                .await
                .map_err(|e| VoomError::Database(format!("prepare: rollback: {e}")))?;
            let closure_initial_json = encode_closure(&abort_outcome.closure_initial)?;
            let row = CommitIntentRowBody {
                target_json: &target_json,
                closure_initial_json: &closure_initial_json,
                accepted_evidence_ids_json: &accepted_evidence_ids_json,
                started_at: now,
            };
            let commit_id = phase_a_gate_abort_with_event(
                pool,
                event_repo,
                &row,
                now,
                abort_outcome.abort.clone(),
            )
            .await?;
            return Ok(PrepareOutcome::Blocked {
                commit_id,
                result: abort_outcome.abort.into_gate_result(),
            });
        }
        Err(e) => return Err(e),
    };

    let closure_initial_json = encode_closure(&walk.closure)?;
    let commit_id = insert_pending_intent(
        &mut tx,
        &target_json,
        &closure_initial_json,
        &accepted_evidence_ids_json,
        override_token_json.as_deref(),
        now,
    )
    .await?;
    expand_scope_members(&mut tx, commit_id, &walk.closure).await?;
    emit_intent_recorded(
        event_repo,
        &mut tx,
        commit_id,
        &target,
        &walk.closure,
        accepted_evidence_ids.len(),
        now,
    )
    .await?;
    if let Some(token) = &override_token {
        emit_forced_override(event_repo, &mut tx, commit_id, token, now).await?;
    }
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("prepare: commit: {e}")))?;

    Ok(PrepareOutcome::Pending(CommitIntent {
        commit_id,
        closure_initial: walk.closure,
        evaluated_lease_ids: walk.evaluated_lease_ids,
        revalidated_evidence: walk.revalidated_evidence,
        epoch: 0,
    }))
}

/// Which gate phase is driving the closure walk. The walker's
/// precondition checks differ between Phase A (which surfaces
/// stale-target handles as `ClosureIncomplete`) and Phase B (which
/// treats the same condition as drift and lets the recompute fall
/// through to the closure-grew trip-wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GatePhase {
    Prepare,
    Authorize,
}

struct GateWalkOk {
    closure: AffectedScopeClosure,
    evaluated_lease_ids: Vec<UseLeaseId>,
    revalidated_evidence: Vec<EvidenceRevalidationResult>,
}

struct GateWalkAbort {
    closure_initial: AffectedScopeClosure,
    abort: PhaseAAbort,
}

/// Run all three Phase A gate checks inside the gate's IMMEDIATE tx.
/// Returns `Ok(Ok(_))` on a passing walk (caller proceeds to insert
/// the `pending` row); `Ok(Err(_))` on a gate-check abort (caller
/// rolls back and runs the two-tx abort helper); `Err(_)` on a storage
/// failure the caller cannot reason about. `bypass` carries the
/// caller's sanctioned `BypassKind` set — `ClosureIncomplete` here
/// silences the `Unreachable` abort path and lets the walk proceed
/// with the DB-internal closure (commit 10).
async fn run_phase_a_gate_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    target: &CommitTarget,
    accepted_evidence_ids: &[EvidenceId],
    bypass: &BTreeSet<BypassKind>,
) -> Result<Result<GateWalkOk, GateWalkAbort>, VoomError> {
    // Step 1: closure walk on the gate's IMMEDIATE tx.
    let closure = match build_closure(
        tx,
        identity_repo,
        alias_resolver,
        target,
        GatePhase::Prepare,
        bypass,
    )
    .await?
    {
        Ok((c, _)) => c,
        Err(abort) => {
            return Ok(Err(GateWalkAbort {
                closure_initial: AffectedScopeClosure::default(),
                abort,
            }));
        }
    };

    // Step 2: blocking-lease check.
    let evaluated_lease_ids = list_blocking_leases_in_tx(tx, &closure).await?;
    if let Some((lease_id, lease_scope)) = first_blocking_overlap_in_tx(tx, &closure).await? {
        return Ok(Err(GateWalkAbort {
            closure_initial: closure,
            abort: PhaseAAbort::UseLease {
                lease_id,
                lease_scope,
            },
        }));
    }

    // Step 3: accepted-evidence revalidation.
    let revalidated_evidence =
        revalidate_evidence_in_tx(tx, identity_repo, accepted_evidence_ids).await?;
    if let Some((evidence_id, drift)) = first_evidence_drift(&revalidated_evidence) {
        return Ok(Err(GateWalkAbort {
            closure_initial: closure,
            abort: PhaseAAbort::StaleEvidence {
                evidence_id,
                drift: drift.clone(),
            },
        }));
    }

    // Step 4: overlapping-prepare check. Consult the pending-commit
    // lock for every scope member of `closure` BEFORE
    // landing the new `pending` row. Without this, two operators
    // preparing destructive commits on overlapping scope both end up
    // with `pending` (and later `authorized`) intents. Iterate from
    // fine-grained to coarse (location → version → bundle → asset) so
    // the most specific offending scope wins the report. First match
    // aborts via the two-tx pattern; the caller turns it into
    // `BlockedByPendingCommit { commit_id, offending_scope }`.
    if let Some((pending_commit_id, offending_scope)) =
        first_pending_commit_overlap_in_tx(tx, &closure).await?
    {
        return Ok(Err(GateWalkAbort {
            closure_initial: closure,
            abort: PhaseAAbort::PendingCommit {
                pending_commit_id,
                offending_scope,
            },
        }));
    }

    Ok(Ok(GateWalkOk {
        closure,
        evaluated_lease_ids,
        revalidated_evidence,
    }))
}

/// First overlap between an in-flight `commit_intents` row
/// (`state IN ('pending','authorized')`) and `closure`. Probed via
/// `consult_pending_commit_lock_in_tx` for every member of the closure,
/// ordered from finest to coarsest granularity so the most specific
/// offending scope wins the report. Returns the first hit as
/// `(pending_commit_id, offending_scope)` or `None` if no in-flight
/// commit covers any member.
async fn first_pending_commit_overlap_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Option<(CommitId, LeaseScope)>, VoomError> {
    for id in &closure.file_locations {
        let scope = LeaseScope::Location(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    for id in &closure.file_versions {
        let scope = LeaseScope::Version(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    for id in &closure.bundles {
        let scope = LeaseScope::Bundle(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    for id in &closure.file_assets {
        let scope = LeaseScope::Asset(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    Ok(None)
}

async fn insert_pending_intent(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target_json: &str,
    closure_initial_json: &str,
    accepted_evidence_ids_json: &str,
    override_token_json: Option<&str>,
    started_at: OffsetDateTime,
) -> Result<CommitId, VoomError> {
    let started_iso = iso8601(started_at)?;
    let res = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, override_token, state, started_at) \
         VALUES (?, ?, ?, ?, 'pending', ?)",
    )
    .bind(target_json)
    .bind(closure_initial_json)
    .bind(accepted_evidence_ids_json)
    .bind(override_token_json)
    .bind(&started_iso)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("commit_intents pending insert: {e}")))?;
    Ok(CommitId(u64_from_i64(res.last_insert_rowid())))
}

/// Emit `commit.forced_override` once at prepare time, atomically
/// with the `commit.intent_recorded` insert and the
/// `commit_intents.override_token` column write. Authorize does not
/// re-emit — the audit signal is single-shot per commit. The payload
/// carries every `BypassKind` bit the operator supplied as
/// `snake_case` strings; Sprint 1 ships exactly `"closure_incomplete"`.
async fn emit_forced_override(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    token: &ForcePathToken,
    recorded_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let bypass: Vec<String> = token
        .bypass
        .iter()
        .map(|k| bypass_kind_str(*k).to_owned())
        .collect();
    let payload = Event::CommitForcedOverride(CommitForcedOverridePayload {
        commit_id,
        actor: token.actor.clone(),
        reason: token.reason.clone(),
        bypass,
        recorded_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: recorded_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

async fn emit_intent_recorded(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    target: &CommitTarget,
    closure: &AffectedScopeClosure,
    accepted_evidence_count: usize,
    started_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitIntentRecorded(CommitIntentRecordedPayload {
        commit_id,
        target_kind: commit_target_kind_str(target).to_owned(),
        closure_asset_count: u32::try_from(closure.file_assets.len()).unwrap_or(u32::MAX),
        closure_bundle_count: u32::try_from(closure.bundles.len()).unwrap_or(u32::MAX),
        closure_version_count: u32::try_from(closure.file_versions.len()).unwrap_or(u32::MAX),
        closure_location_count: u32::try_from(closure.file_locations.len()).unwrap_or(u32::MAX),
        accepted_evidence_count: u32::try_from(accepted_evidence_count).unwrap_or(u32::MAX),
        started_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: started_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}
