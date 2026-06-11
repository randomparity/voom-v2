use super::codecs::{
    TargetRowEpochTriple, decode_closure, decode_force_path_token, decode_target, encode_closure,
    encode_target_row_epochs,
};
use super::prepare::{GatePhase, PhaseAAbort, evidence_drift_str};
use super::scope::{
    build_closure, first_blocking_overlap_in_tx, first_evidence_drift, list_blocking_leases_in_tx,
    revalidate_evidence_in_tx,
};
use super::{
    AffectedScopeClosure, AliasResolver, BTreeSet, BypassKind, ClosureFailure, ClosureMemberDelta,
    ClosureWarning, CommitAbortedByClosureGrewPayload, CommitAbortedByClosureIncompletePayload,
    CommitAbortedByStaleEvidencePayload, CommitAbortedByUseLeasePayload, CommitAuthorizedPayload,
    CommitGateContext, CommitGateResult, CommitId, CommitPermit, CommitTarget, Event,
    EventEnvelope, EventRepo, EvidenceDrift, EvidenceId, EvidenceRevalidationResult,
    ForcePathToken, IdentityRepo, LeaseScope, OffsetDateTime, Row, SubjectType, TargetMemberKind,
    UseLeaseId, VoomError, begin_gate_tx, i64_from_u64, iso8601, u64_from_i64,
};

// ============================================================================
// Phase B entry point — `authorize_destructive_commit`
// ============================================================================

/// Disposition of an `authorize_destructive_commit` call.
///
/// `Authorized` carries the opaque `CommitPermit` — the intent row landed
/// in `state = 'authorized'` with `closure_authorized` and
/// `target_row_epochs` persisted atomically. `Blocked` carries the
/// Phase B abort outcome — a `commit_intents` row was transitioned to
/// `aborted` with `commit_id` referring to that row, and the matching
/// `commit.aborted_by_*` event row sits alongside it in `events`. Phase B
/// commits the abort in-tx (no two-tx pattern; that pattern is reserved
/// for Phase A gate-check aborts only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeOutcome {
    Authorized(CommitPermit),
    Blocked {
        commit_id: CommitId,
        result: CommitGateResult,
    },
}

/// Phase B of the destructive-commit gate. Reads the `commit_intents`
/// row in `state = 'pending'`, recomputes the affected-scope closure
/// against current DB state, runs the three Phase B trip-wires (closure
/// drift, fresh blocking lease, accepted-evidence drift), snapshots
/// per-member epochs into the `target_row_epochs` JSON column, and
/// transitions the row to `state = 'authorized'`. All work runs inside
/// one IMMEDIATE tx; Phase B aborts in-tx.
///
/// `alias_resolver` covers **external** (non-DB) alias sources only.
/// DB-internal alias enumeration goes through
/// `IdentityRepo::list_live_file_locations_by_version_in_tx` on the
/// gate's tx handle, preserving the gate snapshot and avoiding nested
/// connection waits.
///
/// On success the durable row state is:
/// - `state = 'authorized'`
/// - `closure_authorized` = JSON-encoded recomputed closure
/// - `target_row_epochs` = JSON array of `[kind, row_id, epoch]`
///   triples covering every member of the authorized closure
/// - `authorized_at` = `now`, `epoch` bumped
///
/// The returned `CommitPermit` carries the same `commit_id`, the
/// authorized closure, the lease IDs evaluated against it, the
/// evidence revalidation results, and the row's post-update `epoch`.
/// The per-member epoch snapshot is NOT carried on the permit; Phase C
/// re-reads it from `commit_intents.target_row_epochs`.
///
/// # Errors
///
/// - `VoomError::Database` / `VoomError::Internal` on storage failures
///   (including `AliasResolutionError::Database` from an external
///   alias source).
/// - `VoomError::Conflict` if the row does not exist, is in a state
///   other than `pending`, or has had its `epoch` bumped between
///   `prepare` and `authorize` (race against a concurrent operator
///   action). Phase B trip-wires return `Ok(Blocked)` rather than
///   `Err` — `Err` is reserved for genuine storage failures and
///   precondition violations the caller cannot reason about.
pub async fn authorize_destructive_commit(
    context: CommitGateContext<'_>,
    commit_id: CommitId,
    now: OffsetDateTime,
) -> Result<AuthorizeOutcome, VoomError> {
    let CommitGateContext {
        pool,
        identity_repo,
        event_repo,
        alias_resolver,
    } = context;
    let mut tx = begin_gate_tx(pool).await?;

    let row = read_pending_intent_in_tx(&mut tx, commit_id).await?;
    let walk_outcome = run_phase_b_gate_in_tx(
        &mut tx,
        identity_repo,
        event_repo,
        alias_resolver,
        &row,
        now,
    )
    .await?;
    let walk = match walk_outcome {
        Ok(w) => w,
        Err(result) => {
            tx.commit()
                .await
                .map_err(|e| VoomError::database_context("authorize: commit abort", e))?;
            return Ok(AuthorizeOutcome::Blocked { commit_id, result });
        }
    };

    let permit = finalize_phase_b_authorize_in_tx(
        &mut tx,
        event_repo,
        commit_id,
        row.epoch,
        &row.closure_initial,
        walk,
        now,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("authorize: commit success", e))?;
    Ok(AuthorizeOutcome::Authorized(permit))
}

/// Phase B success-path state carried out of the trip-wire gate into
/// `finalize_phase_b_authorize_in_tx`. Holds the recomputed closure
/// plus the lease IDs and evidence revalidation results evaluated
/// against it (both for `CommitPermit` construction on success).
struct PhaseBGatePass {
    closure_authorized: AffectedScopeClosure,
    evaluated_lease_ids: Vec<UseLeaseId>,
    revalidated_evidence: Vec<EvidenceRevalidationResult>,
}

/// Run the three Phase B trip-wires (closure recompute / drift,
/// blocking-lease re-evaluation, accepted-evidence revalidation)
/// inside the open tx. `Ok(Ok(_))` on a passing walk (caller proceeds
/// to write the authorized row); `Ok(Err(_))` on a trip-wire abort
/// (the helper UPDATEs the row to `aborted` + emits the event in-tx;
/// caller commits the tx); `Err(_)` on a genuine storage failure.
async fn run_phase_b_gate_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    row: &PendingIntentRow,
    now: OffsetDateTime,
) -> Result<Result<PhaseBGatePass, CommitGateResult>, VoomError> {
    // Re-apply the prepare-side bypass set. The token JSON was
    // validated at prepare time; we trust the persisted value here
    // (the column write was atomic with the `pending` insert and the
    // intervening row state cannot mutate it — only prepare writes
    // this column).
    let bypass: BTreeSet<BypassKind> = row
        .override_token
        .as_ref()
        .map(|t| t.bypass.clone())
        .unwrap_or_default();
    let closure_authorized = match build_closure(
        tx,
        identity_repo,
        alias_resolver,
        &row.target,
        GatePhase::Authorize,
        &bypass,
    )
    .await?
    {
        Ok((closure, _)) => closure,
        Err(PhaseAAbort::ClosureIncomplete { message }) => {
            let result = abort_pending_intent_in_tx(
                tx,
                event_repo,
                row,
                now,
                PhaseBAbort::ClosureIncomplete { message },
            )
            .await?;
            return Ok(Err(result));
        }
        Err(other) => {
            return Err(VoomError::Internal(format!(
                "authorize: unexpected closure-walk abort kind: {other:?}"
            )));
        }
    };

    let delta = row.closure_initial.id_member_delta(&closure_authorized);
    if !delta.is_empty() {
        let result = abort_pending_intent_in_tx(
            tx,
            event_repo,
            row,
            now,
            PhaseBAbort::ClosureGrew { delta },
        )
        .await?;
        return Ok(Err(result));
    }

    let evaluated_lease_ids = list_blocking_leases_in_tx(tx, &closure_authorized).await?;
    if let Some((lease_id, lease_scope)) =
        first_blocking_overlap_in_tx(tx, &closure_authorized).await?
    {
        let result = abort_pending_intent_in_tx(
            tx,
            event_repo,
            row,
            now,
            PhaseBAbort::UseLease {
                lease_id,
                lease_scope,
            },
        )
        .await?;
        return Ok(Err(result));
    }

    let revalidated_evidence =
        revalidate_evidence_in_tx(tx, identity_repo, &row.accepted_evidence_ids).await?;
    if let Some((evidence_id, drift)) = first_evidence_drift(&revalidated_evidence) {
        let drift = drift.clone();
        let result = abort_pending_intent_in_tx(
            tx,
            event_repo,
            row,
            now,
            PhaseBAbort::StaleEvidence { evidence_id, drift },
        )
        .await?;
        return Ok(Err(result));
    }

    Ok(Ok(PhaseBGatePass {
        closure_authorized,
        evaluated_lease_ids,
        revalidated_evidence,
    }))
}

/// Phase B success path inside the open tx: snapshot per-member
/// epochs, reconcile `scope_members`, transition the row to
/// `authorized`, and emit the `commit.authorized` event. Returns the
/// `CommitPermit` the caller surfaces on `AuthorizeOutcome::Authorized`.
async fn finalize_phase_b_authorize_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
    expected_epoch: u64,
    closure_initial: &AffectedScopeClosure,
    walk: PhaseBGatePass,
    now: OffsetDateTime,
) -> Result<CommitPermit, VoomError> {
    let PhaseBGatePass {
        closure_authorized,
        evaluated_lease_ids,
        revalidated_evidence,
    } = walk;
    let triples = snapshot_target_row_epochs_in_tx(tx, &closure_authorized).await?;
    let target_row_epochs_json = encode_target_row_epochs(&triples)?;
    let closure_authorized_json = encode_closure(&closure_authorized)?;
    reconcile_scope_members(tx, commit_id, closure_initial, &closure_authorized).await?;
    let new_epoch = transition_pending_to_authorized_in_tx(
        tx,
        commit_id,
        expected_epoch,
        &closure_authorized_json,
        &target_row_epochs_json,
        now,
    )
    .await?;
    emit_authorized_event(
        event_repo,
        tx,
        commit_id,
        &closure_authorized,
        u32::try_from(triples.len()).unwrap_or(u32::MAX),
        now,
    )
    .await?;
    Ok(CommitPermit {
        commit_id,
        authorized_at: now,
        closure_authorized,
        evaluated_lease_ids,
        revalidated_evidence,
        epoch: new_epoch,
    })
}

/// Snapshot of the durable `commit_intents` row body Phase B carries
/// across in-tx steps. Loaded once before the closure recompute so the
/// trip-wire branches all bind the same column values.
pub(super) struct PendingIntentRow {
    pub(super) commit_id: CommitId,
    pub(super) target: CommitTarget,
    pub(super) closure_initial: AffectedScopeClosure,
    pub(super) accepted_evidence_ids: Vec<EvidenceId>,
    /// Decoded `commit_intents.override_token` JSON column. `None`
    /// when the column is NULL (default path); `Some(token)` when
    /// `prepare_destructive_commit` persisted a force-path token.
    /// Phase B re-applies the same `BypassKind` set the prepare-side
    /// walk used so the closure-incomplete bypass is honored
    /// identically across phases. Phase B does NOT re-emit
    /// `commit.forced_override` — the audit signal is single-shot per
    /// commit (recorded once at prepare).
    pub(super) override_token: Option<ForcePathToken>,
    pub(super) epoch: u64,
}

/// Read the `commit_intents` row for `commit_id`, require `state =
/// 'pending'`, decode the JSON columns, and return the in-memory shape
/// Phase B operates on. Any state other than `pending` is `Conflict` —
/// callers must `prepare` first.
pub(super) async fn read_pending_intent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
) -> Result<PendingIntentRow, VoomError> {
    let row = sqlx::query(
        "SELECT state, target, closure_initial, accepted_evidence_ids, override_token, epoch \
         FROM commit_intents WHERE id = ?",
    )
    .bind(i64_from_u64(commit_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("authorize: read intent", e))?;
    let row = row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "authorize: commit_intents row {commit_id} not found"
        ))
    })?;
    let state: String = row
        .try_get("state")
        .map_err(|e| VoomError::database_context("authorize: read state", e))?;
    if state != "pending" {
        return Err(VoomError::Conflict(format!(
            "authorize: commit_intents row {commit_id} is in state {state:?}, expected 'pending'"
        )));
    }
    let target_json: String = row
        .try_get("target")
        .map_err(|e| VoomError::database_context("authorize: read target", e))?;
    let closure_initial_json: String = row
        .try_get("closure_initial")
        .map_err(|e| VoomError::database_context("authorize: read closure_initial", e))?;
    let accepted_evidence_ids_json: String = row
        .try_get("accepted_evidence_ids")
        .map_err(|e| VoomError::database_context("authorize: read accepted_evidence_ids", e))?;
    let override_token_json: Option<String> = row
        .try_get("override_token")
        .map_err(|e| VoomError::database_context("authorize: read override_token", e))?;
    let epoch_raw: i64 = row
        .try_get("epoch")
        .map_err(|e| VoomError::database_context("authorize: read epoch", e))?;
    let target = decode_target(&target_json)?;
    let closure_initial = decode_closure(&closure_initial_json)?;
    let accepted_evidence_ids: Vec<EvidenceId> = serde_json::from_str(&accepted_evidence_ids_json)
        .map_err(|e| VoomError::database_context("authorize: decode accepted_evidence_ids", e))?;
    let override_token = match override_token_json {
        None => None,
        Some(json) => Some(decode_force_path_token(&json)?),
    };
    Ok(PendingIntentRow {
        commit_id,
        target,
        closure_initial,
        accepted_evidence_ids,
        override_token,
        epoch: u64_from_i64(epoch_raw),
    })
}

/// Phase B trip-wire bundle. Each variant carries the data needed to
/// drive both the durable row transition (`abort_reason` column) and
/// the matching `commit.aborted_by_*` event payload.
#[derive(Debug, Clone)]
enum PhaseBAbort {
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
    ClosureGrew {
        delta: ClosureMemberDelta,
    },
}

impl PhaseBAbort {
    fn abort_reason_str(&self) -> &'static str {
        match self {
            Self::UseLease { .. } => "fresh_lease",
            Self::StaleEvidence { .. } => "stale_evidence",
            Self::ClosureIncomplete { .. } => "closure_incomplete",
            Self::ClosureGrew { .. } => "closure_grew",
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
            Self::ClosureGrew { delta } => CommitGateResult::BlockedByClosureGrew { delta },
        }
    }
}

/// Abort a `pending` intent in-tx: UPDATE to `state='aborted'` +
/// `abort_reason`, emit the matching event, and return the
/// `CommitGateResult` the caller surfaces to its consumer. Does NOT
/// commit — the caller commits the tx once. Phase B's in-tx abort
/// pattern is deliberately distinct from Phase A's two-tx helper: the
/// two-tx pattern is reserved for Phase A gate-check aborts that fire
/// BEFORE a `pending` row would have landed.
async fn abort_pending_intent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    row: &PendingIntentRow,
    aborted_at: OffsetDateTime,
    abort: PhaseBAbort,
) -> Result<CommitGateResult, VoomError> {
    let aborted_iso = iso8601(aborted_at)?;
    let reason_str = abort.abort_reason_str();
    let commit_id = row.commit_id;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'aborted', aborted_at = ?, \
            abort_reason = ?, epoch = epoch + 1 \
         WHERE id = ? AND state = 'pending' AND epoch = ?",
    )
    .bind(&aborted_iso)
    .bind(reason_str)
    .bind(i64_from_u64(commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("authorize: abort UPDATE", e))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "authorize: abort UPDATE on {commit_id} affected {} rows; concurrent state mutation",
            res.rows_affected()
        )));
    }

    emit_phase_b_abort_event(event_repo, tx, commit_id, &abort, aborted_at).await?;
    Ok(abort.into_gate_result())
}

fn phase_b_abort_event(
    commit_id: CommitId,
    aborted_at: OffsetDateTime,
    abort: &PhaseBAbort,
) -> Event {
    match abort {
        PhaseBAbort::UseLease {
            lease_id,
            lease_scope,
        } => Event::CommitAbortedByUseLease(CommitAbortedByUseLeasePayload {
            commit_id,
            lease_id: *lease_id,
            lease_scope_type: lease_scope.type_str().to_owned(),
            lease_scope_id: lease_scope.id_u64(),
            phase: "authorize".to_owned(),
            aborted_at,
        }),
        PhaseBAbort::StaleEvidence { evidence_id, drift } => {
            Event::CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload {
                commit_id,
                evidence_id: *evidence_id,
                drift_kind: evidence_drift_str(drift).to_owned(),
                phase: "authorize".to_owned(),
                aborted_at,
            })
        }
        PhaseBAbort::ClosureIncomplete { message } => {
            Event::CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload {
                commit_id,
                phase: "authorize".to_owned(),
                message: message.clone(),
                aborted_at,
            })
        }
        PhaseBAbort::ClosureGrew { delta } => {
            Event::CommitAbortedByClosureGrew(CommitAbortedByClosureGrewPayload {
                commit_id,
                added_asset_count: u32::try_from(delta.added_assets.len()).unwrap_or(u32::MAX),
                added_bundle_count: u32::try_from(delta.added_bundles.len()).unwrap_or(u32::MAX),
                added_version_count: u32::try_from(delta.added_versions.len()).unwrap_or(u32::MAX),
                added_location_count: u32::try_from(delta.added_locations.len())
                    .unwrap_or(u32::MAX),
                removed_asset_count: u32::try_from(delta.removed_assets.len()).unwrap_or(u32::MAX),
                removed_bundle_count: u32::try_from(delta.removed_bundles.len())
                    .unwrap_or(u32::MAX),
                removed_version_count: u32::try_from(delta.removed_versions.len())
                    .unwrap_or(u32::MAX),
                removed_location_count: u32::try_from(delta.removed_locations.len())
                    .unwrap_or(u32::MAX),
                phase: "authorize".to_owned(),
                aborted_at,
            })
        }
    }
}

async fn emit_phase_b_abort_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    abort: &PhaseBAbort,
    aborted_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = phase_b_abort_event(commit_id, aborted_at, abort);
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

/// Snapshot per-member epochs for every member of `closure` inside the
/// gate's IMMEDIATE tx. Returns the `[kind, row_id, epoch]` triples
/// Phase B writes atomically to `commit_intents.target_row_epochs`.
/// One SELECT per granularity; the granularity-tagged result is the
/// authoritative source Phase C re-reads.
async fn snapshot_target_row_epochs_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Vec<TargetRowEpochTriple>, VoomError> {
    let mut triples: Vec<TargetRowEpochTriple> = Vec::new();
    let asset_ids: Vec<i64> = closure
        .file_assets
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "file_assets",
        TargetMemberKind::FileAsset,
        &asset_ids,
        &mut triples,
    )
    .await?;
    let version_ids: Vec<i64> = closure
        .file_versions
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "file_versions",
        TargetMemberKind::FileVersion,
        &version_ids,
        &mut triples,
    )
    .await?;
    let location_ids: Vec<i64> = closure
        .file_locations
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "file_locations",
        TargetMemberKind::FileLocation,
        &location_ids,
        &mut triples,
    )
    .await?;
    let bundle_ids: Vec<i64> = closure
        .bundles
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "asset_bundles",
        TargetMemberKind::Bundle,
        &bundle_ids,
        &mut triples,
    )
    .await?;
    Ok(triples)
}

async fn snapshot_one_granularity_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &'static str,
    kind: TargetMemberKind,
    ids: &[i64],
    out: &mut Vec<TargetRowEpochTriple>,
) -> Result<(), VoomError> {
    if ids.is_empty() {
        return Ok(());
    }
    let ids_json = serde_json::to_string(ids)
        .map_err(|e| VoomError::Internal(format!("encode {table} id snapshot: {e}")))?;
    // `table` is a static internal string — never caller-supplied — so
    // a `format!` SQL stitch is safe here (sqlx does not expose runtime
    // table-name binding).
    let sql = format!("SELECT id, epoch FROM {table} WHERE id IN (SELECT value FROM json_each(?))");
    let rows = sqlx::query(&sql)
        .bind(&ids_json)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context(format!("snapshot {table}"), e))?;
    for row in &rows {
        let id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::database_context(format!("snapshot {table} row id"), e))?;
        let epoch: i64 = row
            .try_get("epoch")
            .map_err(|e| VoomError::database_context(format!("snapshot {table} row epoch"), e))?;
        out.push(TargetRowEpochTriple(
            kind,
            u64_from_i64(id),
            u64_from_i64(epoch),
        ));
    }
    Ok(())
}

/// Reconcile `commit_intent_scope_members` with the recomputed closure:
/// DELETE rows whose scope_*_id is no longer in the authorized closure,
/// INSERT new rows for added members. Compares the Phase A
/// `closure_initial` against the Phase B `closure_authorized` to derive
/// the delta — the row deletes and inserts are keyed off the four
/// granularity-specific delta sets so a no-op closure produces zero
/// writes.
async fn reconcile_scope_members(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    initial: &AffectedScopeClosure,
    authorized: &AffectedScopeClosure,
) -> Result<(), VoomError> {
    let cid = i64_from_u64(commit_id.0);
    // Removed members → DELETE matching scope_*_id rows.
    for asset in initial.file_assets.difference(&authorized.file_assets) {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_asset_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(asset.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members asset delete", e))?;
    }
    for bundle in initial.bundles.difference(&authorized.bundles) {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_bundle_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(bundle.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members bundle delete", e))?;
    }
    for version in initial.file_versions.difference(&authorized.file_versions) {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_version_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(version.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members version delete", e))?;
    }
    for location in initial
        .file_locations
        .difference(&authorized.file_locations)
    {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_location_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(location.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members location delete", e))?;
    }

    // Added members → INSERT new rows.
    for asset in authorized.file_assets.difference(&initial.file_assets) {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_asset_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(asset.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members asset insert", e))?;
    }
    for bundle in authorized.bundles.difference(&initial.bundles) {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_bundle_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(bundle.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members bundle insert", e))?;
    }
    for version in authorized.file_versions.difference(&initial.file_versions) {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_version_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(version.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members version insert", e))?;
    }
    for location in authorized
        .file_locations
        .difference(&initial.file_locations)
    {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_location_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(location.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("scope_members location insert", e))?;
    }
    Ok(())
}

/// Transition the `pending` row to `authorized`. Guards on
/// `(id, state='pending', epoch=row.epoch)` so a concurrent operator
/// action (abort, racing authorize) cannot land a half-written row.
/// Bumps the epoch and returns the new value the caller carries in
/// the returned `CommitPermit`.
async fn transition_pending_to_authorized_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    expected_epoch: u64,
    closure_authorized_json: &str,
    target_row_epochs_json: &str,
    authorized_at: OffsetDateTime,
) -> Result<u64, VoomError> {
    let authorized_iso = iso8601(authorized_at)?;
    let new_epoch = expected_epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET \
            state = 'authorized', \
            closure_authorized = ?, \
            target_row_epochs = ?, \
            authorized_at = ?, \
            epoch = ? \
         WHERE id = ? AND state = 'pending' AND epoch = ?",
    )
    .bind(closure_authorized_json)
    .bind(target_row_epochs_json)
    .bind(&authorized_iso)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(commit_id.0))
    .bind(i64_from_u64(expected_epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("authorize: UPDATE to authorized", e))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "authorize: UPDATE to authorized on {commit_id} affected {} rows; \
             concurrent state mutation between read and write",
            res.rows_affected()
        )));
    }
    Ok(new_epoch)
}

async fn emit_authorized_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    closure: &AffectedScopeClosure,
    target_row_epoch_count: u32,
    authorized_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitAuthorized(CommitAuthorizedPayload {
        commit_id,
        closure_asset_count: u32::try_from(closure.file_assets.len()).unwrap_or(u32::MAX),
        closure_bundle_count: u32::try_from(closure.bundles.len()).unwrap_or(u32::MAX),
        closure_version_count: u32::try_from(closure.file_versions.len()).unwrap_or(u32::MAX),
        closure_location_count: u32::try_from(closure.file_locations.len()).unwrap_or(u32::MAX),
        target_row_epoch_count,
        authorized_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: authorized_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}
