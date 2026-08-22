# ADR 0073: Terminal-safe owner-local acquisition replay

## Status

Accepted

## Context

Issue #479 finishes the owner-local acquisition lifecycle. ADR 0071 persisted
the owner-local proof on plans and decisions; ADR 0072 made the acquisition
atomic and bound the selected decision to its lease. One completed acquire is
therefore described twice: by durable rows (lease, `UNIQUE(lease_id)` plan,
selected decision with `selected_lease_id`) and by the completed
`remote_idempotency_keys` response whose `leased` dispatch carries every
identity together.

Two facts remain unsatisfactory:

- **Replay trusts the cache blindly.** The replay branch of
  `remote_acquire` decodes the stored response and returns it. A row that is
  valid JSON for the current binary but semantically corrupt — wrong
  ownership bindings, zero IDs, evidence disagreeing with the plan row, a
  decision pointing at another lease — replays as a success. Criterion 4 of
  #479 makes such corruption a database error.
- **Completion evidence is untyped.** `validated_artifact_complete_evidence`
  inspects `JsonValue` fields, so an echo carrying extra or misplaced fields
  validates as long as the checked keys agree. Criterion 5 requires exact
  typed consumption evidence.

Completed idempotent replay must also stay valid after completion or failure
legitimately mutates lease and plan state (criterion 3), and replay must
never create another lease, plan, event, or scheduling decision (criterion 2).

### Binding constraints (issue #479)

1. Successful acquisition stores strict immutable evidence binding decision,
   ticket, worker, owner, normalized operation, lease identity, canonical
   access plan.
2. Completed replay validates that evidence and returns the original outcome
   without creating anything.
3. Replay remains valid after both terminal outcomes.
4. Serde-valid but semantically corrupt replay evidence fails as a database
   error.
5. Completion requires exact typed consumption evidence; consumption is
   atomic with terminal mutation; failure never claims consumption.
6. API, control-plane, node-agent, fake, and conformance coverage.

## Decision

### The stored completed acquire response *is* the immutable evidence

No new schema and no migration 0039: migration 0037's shapes already express
every fact this slice needs — the same conclusion ADR 0072 reached for 0038.
Criterion 1 is satisfied by the existing atomic write (#477/#478) plus one
validation rule set defined here; nothing durable is added, so nothing can
drift out of sync with the rows it binds.

### Replay validation inside the reservation transaction

The acquire replay arm gains an explicit post-decode validator: a dedicated
function receiving the decoded outcome and the still-open transaction, run in
the replay branch before `finish_replay_in_tx` commits. The decode closure is
untouched, so the poison-repoint contract keeps its exact scope: responses
that no longer *decode* — shape drift, malformed JSON, unknown outcome
variants, or embedded evidence failing `OwnerAccessEvidence`'s validating
deserialization (unknown fields, non-canonical ordering, epoch-set mismatch)
— are unreadable results and stay on the existing repoint path. This slice's
validator covers only corruption that decodes cleanly but disagrees with
durable rows.

- **Idle / no-candidate replays** require a non-zero decision id naming an
  existing decision row with matching kind and outcome (`Idle`/`Idle`,
  `NoCandidate`/`NoEligibleCandidate`) and requesting worker.
- **Leased replays** require non-zero identities throughout; a lease row in
  **any** state with matching ticket and worker; the named plan row matching
  the dispatch on lease, ticket, worker, node, owner pair, and
  `access_evidence` compared as serialized canonical JSON; a selected
  `LeaseAcquire` decision whose `selected_lease_id` equals the dispatch
  lease; the ticket kind normalizing to exactly the dispatched operation;
  and `dispatch_payload` equal to the ticket payload.

Mutable state — lease state, plan status, TTLs, heartbeats — is deliberately
outside the rule set: completion, failure, expiry, force-release, and
heartbeats legitimately change it after the acquisition became terminal, and
criterion 3 forbids replay depending on it. Identity is what replay proves;
state belongs to the terminal operations' own contracts.

Every violation raises `VoomError::Database` naming the disagreement,
following the repo-wide rule that corrupt storage is a database error, not a
domain result. Semantic corruption does **not** repoint the stored response:
the poison-repoint contract exists for results the running binary can no
longer decode, where every future replay would fail identically anyway and
keeping the bytes has no diagnostic value. A semantic mismatch is different —
the response decodes fine and disagrees with the rows; it is the operator's
primary evidence of what was originally acquired, so it is preserved and the
database error recurs deterministically until the data is repaired.

Replay stays read-only: zero leases, plans, events, decisions (criterion 2).

### Exact typed consumption evidence

`remote_complete` deserializes the echoed `artifact_access` block into
`ValidatedArtifactAccess { validated: bool, owner_node_id: Option<u64>,
access_evidence: Option<OwnerAccessEvidence> }` with
`#[serde(deny_unknown_fields)]`. Unknown fields are rejected — the legacy
synthetic/shared-mount echo shape (`mode`, `inputs_consumed`,
`outputs_declared`) stops validating, honoring the removed-field rule: a
representation that left the wire in #477 no longer passes completion. The
agreement semantics keep today's outcomes: byte-work plans require the same
owner and equal evidence; declaration-free plans require neither; mismatches
stay `Conflict`. Error precedence deliberately changes in one respect: an
unknown echo field now fails typed decode before the agreement checks run,
where the previous JSON inspection silently ignored it. Consumption
(`mark_status_in_tx` → `Consumed`) and lease release commit in one
transaction as today; `remote_fail` maps plans to `Failed`/`Rejected` only
and can never claim `Consumed`.

### No other surface changes

`RemoteLeaseDispatch`/`RemoteArtifactAccessPlan` keep their wire shape, so
the node-agent mirror, fakes runner, fake-support validator, and conformance
echo worker need no contract change; their suites gain coverage, not
migration. No scheduler vocabulary, suppression-key, or fallback behavior
changes.

## Consequences

- A replayed `leased` outcome is proven against durable rows on every replay;
  corruption surfaces as a database error instead of a phantom success.
- Replay survives both terminal outcomes because validation reads identity,
  never mutable state.
- Workers echoing legacy access-plan fields fail completion with a clear
  conflict instead of silently ignoring unknown keys.
- Corruption does not trigger poison-repoint; the stored success response is
  retained for diagnosis and the error is deterministic across retries.
- Rollback is `git revert` clean: no schema, payload-column, or wire-shape
  change ships.

## Alternatives considered

- **Persist a dedicated immutable-evidence row per acquisition (migration
  0039).** Rejected: it would duplicate identities already durably bound by
  #477/#478; two projections of one fact can disagree, which is precisely
  the corruption this slice detects.
- **Validate only on first replay, then mark the row verified.** Rejected:
  a verified flag is mutable state replay would depend on — the exact
  dependency criterion 3 removes — and corruption appearing later would go
  unnoticed.
- **Repoint semantically corrupt responses to terminal errors like decode
  failures.** Rejected above: masks success and destroys the original
  outcome.
- **Treat echoed evidence leniently (ignore unknown fields).** Rejected:
  criterion 5 says *exact*; leniency re-admits the legacy shape #477
  removed.
