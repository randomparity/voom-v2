use super::authorize::read_pending_intent_in_tx;
use super::codecs::{decode_closure, decode_target};
use super::finalize::emit_aborted_pre_mutation_event;
use super::{
    AbortReason, CommitId, CommitIntentState, EventRepo, EvidenceId, OffsetDateTime,
    PendingCommitIntent, Row, SqlitePool, VoomError, begin_gate_tx, i64_from_u64, iso8601,
    parse_iso8601, u64_from_i64,
};

/// Outcome of `abort_destructive_commit`. Carries the now-aborted
/// `commit_id` and the post-update `epoch` for callers that want to
/// confirm the durable transition. The function never returns
/// "no-op" — a row that cannot be aborted surfaces as
/// `VoomError::Conflict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortOutcome {
    Aborted { commit_id: CommitId, epoch: u64 },
}

/// Caller-initiated abort of a `pending` `commit_intents` row. This is
/// the only sanctioned entry point for the "operator changed their mind
/// between `prepare` and `authorize`" transition. One IMMEDIATE tx:
/// 1. Read the `commit_intents` row by `commit_id`. Require
///    `state = 'pending'`. Missing, `authorized`, or any terminal state
///    surfaces as `VoomError::Conflict` — `authorized` rows are not
///    abortable through this entry; the only sanctioned post-authorize
///    termination is `finalize_destructive_commit(_,
///    MutationOutcome::NotPerformed, _)` (recovery contract).
/// 2. UPDATE the row to `state = 'aborted'`, set `aborted_at = now`,
///    write `reason`'s `snake_case` tag into `abort_reason`, bump the
///    `epoch`.
/// 3. Emit `commit.aborted_pre_mutation` with `prior_state = 'pending'`
///    and the reason tag. (The event kind is shared with the
///    `NotPerformed` branch of `finalize_destructive_commit`, which
///    emits with `prior_state = 'authorized'`.)
///
/// `reason` must be one of the pre-mutation `AbortReason` variants:
/// `OperatorCancel`, `MutationFailed`, or `Other(_)`. Gate-driven
/// variants (`ClosureGrew`, `FreshLease`, `ClosureIncomplete`,
/// `StaleEvidence`) route through their dedicated `commit.aborted_by_*`
/// event kinds inside the gate itself; `StaleTargetEpoch` is Phase-C
/// only and writes to `recovery_reason`, not `abort_reason`. Passing
/// any of these returns `VoomError::Config` without touching the row.
///
/// # Errors
///
/// - `VoomError::Config` if `reason` is not a sanctioned caller-supplied
///   variant for this entry point.
/// - `VoomError::Conflict` if the row does not exist or is in a state
///   other than `pending` (including `authorized` — recovery contract).
/// - `VoomError::Database` / `VoomError::Internal` on storage failures.
pub async fn abort_destructive_commit(
    pool: &SqlitePool,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
    reason: AbortReason,
    now: OffsetDateTime,
) -> Result<AbortOutcome, VoomError> {
    let reason_str = caller_abort_reason_str(&reason)?;

    let mut tx = begin_gate_tx(pool).await?;

    let row = read_pending_intent_in_tx(&mut tx, commit_id).await?;

    let aborted_iso = iso8601(now)?;
    let new_epoch = row.epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'aborted', aborted_at = ?, \
            abort_reason = ?, epoch = ? \
         WHERE id = ? AND state = 'pending' AND epoch = ?",
    )
    .bind(&aborted_iso)
    .bind(reason_str)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut *tx)
    .await
    .map_err(|e| VoomError::database_context("abort: UPDATE", e))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "abort: UPDATE on {commit_id} affected {} rows; concurrent state mutation",
            res.rows_affected()
        )));
    }

    emit_aborted_pre_mutation_event(event_repo, &mut tx, commit_id, "pending", reason_str, now)
        .await?;

    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("abort: commit", e))?;

    Ok(AbortOutcome::Aborted {
        commit_id,
        epoch: new_epoch,
    })
}

/// Validate that `reason` is a sanctioned caller-supplied
/// `AbortReason` for the pending-only abort entry point and return its
/// `snake_case` tag for the durable `abort_reason` column and the
/// `commit.aborted_pre_mutation` event payload. Gate-driven and
/// post-mutation variants are rejected with `VoomError::Config`
/// before any tx opens.
fn caller_abort_reason_str(reason: &AbortReason) -> Result<&'static str, VoomError> {
    match reason {
        AbortReason::OperatorCancel => Ok("operator_cancel"),
        AbortReason::MutationFailed => Ok("mutation_failed"),
        AbortReason::Other(_) => Ok("other"),
        AbortReason::ClosureGrew
        | AbortReason::FreshLease
        | AbortReason::ClosureIncomplete
        | AbortReason::StaleEvidence
        | AbortReason::StaleTargetEpoch => Err(VoomError::Config(format!(
            "abort: {reason:?} is a gate-driven or post-mutation variant; \
             callers may only pass OperatorCancel, MutationFailed, or Other(_)"
        ))),
    }
}

/// Read-only listing over in-flight `commit_intents` rows. Returns every row in
/// `state IN ('pending', 'authorized')`, ordered by `started_at ASC`
/// with `id ASC` as a tie-breaker for non-unique `started_at`. Terminal
/// states (`completed`, `aborted`, `recovery_required`) are excluded.
///
/// Pass `older_than = Some(cutoff)` to restrict the result to rows
/// whose `started_at` is strictly less than `cutoff` — the entry point
/// triage tooling uses to surface stale in-flight commits. `None`
/// disables the time filter. The query is shaped to ride the
/// `commit_intents_in_flight` partial index defined in migration 0005
/// (`(state, started_at) WHERE state IN ('pending','authorized')`).
///
/// Read-only and stateless: opens no transaction, emits no events,
/// mutates nothing. The closure / target / evidence JSON columns are
/// decoded through the same inverse wire mappers Phase B / Phase C use
/// so on-disk shape stays single-source.
///
/// # Errors
///
/// - `VoomError::Database` on storage failures or unparseable column
///   values written by an earlier phase (the wire decoders surface
///   their own `VoomError::Database` errors verbatim).
pub async fn list_pending_commit_intents(
    pool: &SqlitePool,
    older_than: Option<OffsetDateTime>,
) -> Result<Vec<PendingCommitIntent>, VoomError> {
    let rows = match older_than {
        Some(cutoff) => {
            let cutoff_iso = iso8601(cutoff)?;
            sqlx::query(
                "SELECT id, target, closure_initial, closure_authorized, \
                        accepted_evidence_ids, state, started_at, authorized_at \
                 FROM commit_intents \
                 WHERE state IN ('pending','authorized') AND started_at < ? \
                 ORDER BY started_at ASC, id ASC",
            )
            .bind(&cutoff_iso)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query(
                "SELECT id, target, closure_initial, closure_authorized, \
                        accepted_evidence_ids, state, started_at, authorized_at \
                 FROM commit_intents \
                 WHERE state IN ('pending','authorized') \
                 ORDER BY started_at ASC, id ASC",
            )
            .fetch_all(pool)
            .await
        }
    }
    .map_err(|e| VoomError::database_context("list_pending_commit_intents: query", e))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(decode_pending_commit_intent_row(&row)?);
    }
    Ok(out)
}

/// Map one `commit_intents` row (limited to `pending` / `authorized` by
/// the caller's `WHERE` clause) into the public `PendingCommitIntent`
/// shape. Decodes JSON columns through the canonical inverse wire
/// mappers and enforces the migration 0005 invariant that
/// `closure_authorized` / `authorized_at` are non-NULL exactly for
/// `state = 'authorized'`.
fn decode_pending_commit_intent_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PendingCommitIntent, VoomError> {
    let id_raw: i64 = row
        .try_get("id")
        .map_err(|e| VoomError::database_context("list_pending_commit_intents: read id", e))?;
    let commit_id = CommitId(u64_from_i64(id_raw));
    let state_str: String = row
        .try_get("state")
        .map_err(|e| VoomError::database_context("list_pending_commit_intents: read state", e))?;
    let state = parse_in_flight_state(&state_str, commit_id)?;
    let target_json: String = row
        .try_get("target")
        .map_err(|e| VoomError::database_context("list_pending_commit_intents: read target", e))?;
    let closure_initial_json: String = row.try_get("closure_initial").map_err(|e| {
        VoomError::database(format!(
            "list_pending_commit_intents: read closure_initial: {e}"
        ))
    })?;
    let closure_authorized_json: Option<String> =
        row.try_get("closure_authorized").map_err(|e| {
            VoomError::database(format!(
                "list_pending_commit_intents: read closure_authorized: {e}"
            ))
        })?;
    let accepted_evidence_ids_json: String = row.try_get("accepted_evidence_ids").map_err(|e| {
        VoomError::database(format!(
            "list_pending_commit_intents: read accepted_evidence_ids: {e}"
        ))
    })?;
    let started_at_str: String = row.try_get("started_at").map_err(|e| {
        VoomError::database_context("list_pending_commit_intents: read started_at", e)
    })?;
    let authorized_at_str: Option<String> = row.try_get("authorized_at").map_err(|e| {
        VoomError::database(format!(
            "list_pending_commit_intents: read authorized_at: {e}"
        ))
    })?;

    // Migration 0005 CHECK: closure_authorized IS NOT NULL iff
    // state = 'authorized', and authorized_at moves in lockstep with
    // closure_authorized. Cross-validate so a corrupt row surfaces
    // here rather than as a misleading `None` in the public shape.
    // The two terminal-state variants are excluded by both the SQL
    // `WHERE` clause and `parse_in_flight_state` above; this match is
    // exhaustive against the in-flight subset.
    let (closure_authorized, authorized_at) = match state {
        CommitIntentState::Pending => {
            if closure_authorized_json.is_some() || authorized_at_str.is_some() {
                return Err(VoomError::Internal(format!(
                    "list_pending_commit_intents: commit_intents row {commit_id} is pending but \
                     has closure_authorized or authorized_at set; migration 0005 CHECK should \
                     have prevented this"
                )));
            }
            (None, None)
        }
        CommitIntentState::Authorized => {
            let closure_json = closure_authorized_json.ok_or_else(|| {
                VoomError::Internal(format!(
                    "list_pending_commit_intents: commit_intents row {commit_id} is authorized \
                     but closure_authorized is NULL; migration 0005 CHECK should have prevented this"
                ))
            })?;
            let authorized_at_iso = authorized_at_str.ok_or_else(|| {
                VoomError::Internal(format!(
                    "list_pending_commit_intents: commit_intents row {commit_id} is authorized \
                     but authorized_at is NULL; migration 0005 CHECK should have prevented this"
                ))
            })?;
            (
                Some(decode_closure(&closure_json)?),
                Some(parse_iso8601(&authorized_at_iso)?),
            )
        }
        CommitIntentState::Completed
        | CommitIntentState::Aborted
        | CommitIntentState::RecoveryRequired => {
            // `parse_in_flight_state` only ever returns Pending /
            // Authorized; reaching this arm means that contract was
            // violated upstream. Treat as an invariant violation.
            return Err(VoomError::Internal(format!(
                "list_pending_commit_intents: commit_intents row {commit_id} surfaced terminal \
                 state {state_str:?}; parser should have rejected it"
            )));
        }
    };

    let target = decode_target(&target_json)?;
    let closure_initial = decode_closure(&closure_initial_json)?;
    let accepted_evidence_ids: Vec<EvidenceId> = serde_json::from_str(&accepted_evidence_ids_json)
        .map_err(|e| {
            VoomError::database(format!(
                "list_pending_commit_intents: decode accepted_evidence_ids: {e}"
            ))
        })?;
    let started_at = parse_iso8601(&started_at_str)?;

    Ok(PendingCommitIntent {
        commit_id,
        target,
        state,
        closure_initial,
        closure_authorized,
        accepted_evidence_ids,
        started_at,
        authorized_at,
    })
}

/// Parse a `commit_intents.state` string into `CommitIntentState`,
/// limited to the two in-flight values the listing query selects for.
/// Terminal states surface as `VoomError::Internal` because reaching
/// this parser with one would mean the SQL `WHERE` clause was
/// bypassed; an unknown string is `VoomError::Database` because that's
/// the on-disk corruption case (a CHECK constraint violation that
/// somehow landed).
fn parse_in_flight_state(s: &str, commit_id: CommitId) -> Result<CommitIntentState, VoomError> {
    match s {
        "pending" => Ok(CommitIntentState::Pending),
        "authorized" => Ok(CommitIntentState::Authorized),
        "completed" | "aborted" | "recovery_required" => Err(VoomError::Internal(format!(
            "list_pending_commit_intents: commit_intents row {commit_id} surfaced terminal \
             state {s:?}; WHERE clause should have excluded it"
        ))),
        other => Err(VoomError::database(format!(
            "list_pending_commit_intents: commit_intents row {commit_id} has unknown state {other:?}"
        ))),
    }
}
