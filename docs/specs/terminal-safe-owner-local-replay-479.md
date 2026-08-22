# Spec: Terminal-safe owner-local acquisition replay (#479)

Governing ADR: `docs/adr/0073-terminal-safe-owner-local-acquisition-replay.md`.
Predecessors: ADR 0069 (canonical declaration), ADR 0070 (resolution + gate),
ADR 0071 (persisted owner-local scheduling evidence), ADR 0072 (atomic
acquire), ADR 0068 Consequences (#475 requirement).

## Problem

After #477/#478 a successful remote acquisition atomically creates one lease,
one owner-evidence access plan (`UNIQUE(lease_id)`), and one selected
scheduler decision carrying `selected_lease_id`, and the completed
`remote_idempotency_keys` row stores the full `leased` dispatch as replayable
JSON. Two gaps remain against issue #479:

1. **Replay trusts the cached response blindly.** `finish_replay_in_tx`
   decodes the stored outcome and returns it. Nothing proves the cached
   dispatch still corresponds to durable rows: a corrupted or semantically
   inconsistent row (wrong ownership, zero IDs, evidence that disagrees with
   the plan, a decision pointing at another lease) replays as a success.
2. **Completion evidence is ad-hoc JSON inspection.**
   `validated_artifact_complete_evidence` pokes at `JsonValue` fields instead
   of deserializing an exact typed shape, so extra or malformed fields inside
   the echoed `artifact_access` block are silently ignored.

## Design

### Immutable acquisition evidence (no schema change)

The immutable evidence binding scheduler decision, ticket, worker, owner,
normalized operation, lease identity, and canonical access plan already
exists as two durable projections written in one transaction:
- rows: `scheduler_decisions` (selected with `selected_lease_id`),
  `artifact_access_plans` (`UNIQUE(lease_id)`, owner/evidence pair), and
  `leases` (ticket/worker identity);
- the completed acquire idempotency row whose `leased` response carries all
  identities together.

Migration 0039 is **not** written: migration 0037's shapes already express
every fact (the same conclusion #478 reached for 0038).

### Replay validates the evidence, creates nothing

The acquire replay arm gains an explicit post-decode validation step: a
dedicated validator that receives the decoded outcome and the still-open
transaction and runs before `finish_replay_in_tx` commits. The decode closure
itself is untouched, so the existing poison-repoint contract for undecodable
stored responses (shape drift, malformed JSON, unknown outcome variants) is
unchanged — corruption that fails typed deserialization of the embedded
evidence (unknown fields, non-ascending epochs, wrong targets) lands on that
existing path and stays repoint behavior. This slice's validator covers only
corruption that *decodes* but disagrees with durable rows.

The acquire replay branch gains a validation step between decode and return,
running inside the still-open transaction:

- **Idle / no-candidate replays:** `scheduler_decision_id` is non-zero and
  names an existing decision row with the matching kind and outcome
  (`Idle`/`Idle`, `NoCandidate`/`NoEligibleCandidate`) and requesting worker.
- **Leased replays:** every identity field is non-zero; the lease row exists
  in **any** state with matching ticket and worker (state deliberately
  ignored — completion, failure, expiry, and force-release legitimately
  mutate it); the plan row named by `artifact_access_plan.id` matches the
  dispatch on lease, ticket, worker, node, `owner_node_id`, and
  `access_evidence` (compared as serialized canonical JSON); the decision row
  named by `scheduler_decision_id` is a selected `LeaseAcquire` decision whose
  `selected_lease_id` equals the dispatch lease; the ticket row's kind
  normalizes to exactly `dispatch.operation`; `dispatch_payload` equals the
  ticket payload.

Any violation is a `VoomError::Database` naming the disagreement — corruption
is a database error, never a replayed success (AGENTS.md untrusted-persisted-
data rule). Unlike shape-drift decode failures, semantic corruption does
**not** repoint the stored response: repointing masks a success as a permanent
error and destroys the only surviving copy of the original outcome. The
database error recurs deterministically until an operator repairs the data.

Replay still creates zero leases, plans, events, and decisions — the branch
was read-only and stays read-only.

### Exact typed consumption evidence on completion

A new control-plane type deserializes the echoed `artifact_access` block:

```text
ValidatedArtifactAccess {
    validated: bool,                       // must deserialize to true
    owner_node_id: Option<u64>,
    access_evidence: Option<OwnerAccessEvidence>,
}
```

`#[serde(deny_unknown_fields)]`: unknown fields in the echo are rejected, so
legacy `mode`/`inputs_consumed`/`outputs_declared` echoes fail. The type
lives beside the other remote-execution wire types in
`remote_execution/mod.rs`, inside the payload-contract gate's scope. The
agreement semantics are preserved (byte-work plans require the same owner and
equal evidence; declaration-free plans require neither; mismatches stay
`Conflict`), but error precedence deliberately changes: an unknown echo field
now fails decode before the agreement checks fire, where the old JSON poking
ignored it. The consumed value is stored in `artifact_access_plans.evidence`
as before.

Failure (`remote_fail`) validates the plan binding via the same
by-lease lookup and maps status to `Failed`/`Rejected` only — it can never
claim `Consumed`.

## Test matrix

   again after `remote_complete`; again after `remote_fail`; corruption that
   decodes cleanly — zero lease/decision/plan id, wrong owner, evidence
   disagreeing with the plan row, decision pointing at another lease, deleted
   plan row, altered operation/payload — each fails with a database error,
   does not repoint the stored response, and creates nothing; corruption that
   fails typed evidence deserialization instead stays on the existing
   poison-repoint path, unchanged.
2. Control-plane: completion requires the exact typed echo — unknown fields,
   missing `validated`, wrong owner, wrong evidence, and claims on
   declaration-free plans are rejected; fail paths never produce
   `consumed`.
3. API route: acquire → complete → re-acquire with the same key replays the
   original leased dispatch over HTTP; a corrupted store makes the replay
   return a database-error envelope; complete rejects a legacy-shape echo.
4. Node agent / fakes / conformance: fresh acquisition, exact replay of the
   same acquire request (identical dispatch, single execution), and replay
   after both terminal outcomes hold end-to-end through the protocol mirror.

## Exclusions

No wire-shape change (`RemoteLeaseDispatch`/`RemoteArtifactAccessPlan` keep
their fields); no scheduler vocabulary change; no fallback synthesis; no
migration; no deletion of completed idempotency rows.
