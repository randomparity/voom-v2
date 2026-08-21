# ADR 0072: Atomically acquire owner-local byte work

## Status

Accepted

## Context

Issue #478 closes the acquisition gap in the owner-local byte-work chain.
ADR 0069 gave byte-work tickets a canonical declaration and one
ticket-kind normalization; ADR 0070 resolved that declaration against storage
state and gated remote acquire on a single common owner; ADR 0071 persisted
the resulting evidence on plans and decisions. Three facts about the fresh
acquisition path remain unsatisfactory:

- **Changed post-selection gates raise instead of deciding.** The guarded
  lease acquisition (`SqliteLeaseRepo::acquire_guarded`) rechecks ticket
  readiness, worker eligibility, and capacity when it creates the lease — but
  a changed gate surfaces as a raised `VoomError` that aborts the entire
  acquire request. No durable scheduler decision records the stable reason,
  so the recheck is invisible to operators exactly where it matters. The
  selected scheduler decision is also written *before* the lease exists, so
  any lease-time failure would leave a selected row pointing at no lease.
- **Dispatch carries the raw ticket-kind token.** `RemoteLeaseDispatch`
  sends `ticket.kind.into_string()`. For workflow-rendered tickets that is
  the reserved-namespaced form (`synthetic.workflow.operation.<op>`), which
  `OperationKind::from_wire` rejects in the node agent's `dispatch_to_child`.
  An eligible namespaced byte operation therefore cannot execute, and tests
  compensate by registering worker capabilities under both encodings.
- **The binding was not proven atomic end to end.** Criterion 3 of #478
  requires one lease bound to the exact scheduler decision, ticket, worker,
  owner, and canonical access evidence in one transaction; nothing asserted
  the full triple together.

### Binding constraints (issue #478)

1. Recheck ticket readiness, worker capability/grants/liveness/capacity,
   node/incarnation state, owner locality, active roots, and current epochs
   inside the writer transaction.
2. Any changed gate produces the documented stable reason with zero lease and
   zero bound access-plan mutation.
3. A successful acquisition atomically creates one lease and binds it to the
   exact scheduler decision, ticket, worker, owner, and canonical access
   evidence.
4. Namespaced supported operations are normalized before node-agent dispatch;
   exact custom local operations retain their exact token.
5. No non-owner fallback, path-based proof, shared-mount proof, or transfer
   ticket is synthesized.
   The readiness recheck deliberately folds three distinguishable facts —
   ticket left `ready`, parent job closed, attempt budget exhausted — into one
   documented reason, because the twelve-code vocabulary offers no finer code
   and the pre-change conflict message conflated them identically; operators
   read the distinguishing detail from the row's summary and explanation
   columns. Finer attribution is a future vocabulary extension, not this
   slice.

Completed idempotent replay and terminal completion/failure plan semantics
are owned by #479 and excluded.

## Decision

### Changed gates are outcomes, not errors (`voom-store`)

`LeaseAcquireOutcome` becomes a total description of the guarded rechecks:
`Acquired`, `CapacityFull`, plus new `TicketNotReady { ticket_id }` (the
conditional readiness `UPDATE` matched zero rows) and `WorkerIneligible {
worker_id, operation, reason: LeaseIneligibilityReason }` with the closed set
`WorkerMissing | WorkerStale | WorkerRetired | OperationDenied |
MissingCapability | MissingGrant`. Every non-acquired outcome rolls back the
acquire savepoint, so a changed gate mutates nothing. An unknown-namespaced
ticket kind stays a database error — corruption is never an eligibility
result (ADR 0069). `into_lease_result` preserves today's exact errors for the
standalone and local callers, whose observable behavior is unchanged.

### Post-selection gates decide with documented stable reasons

The selected path reorders to capacity recheck → lease → plan → decision:

- A structured non-acquired outcome maps onto the existing twelve-code
  scheduler reason vocabulary and writes **one** durable no-candidate
  decision per attempt: `no_ready_ticket`, `worker_not_executable`,
  `operation_denied`, `missing_capability`, `missing_grant`,
  `worker_capacity_full`. The row names the selected ticket
  (`ticket_id` set, `candidate_count = 1`) and its suppression key carries a
  matching ticket segment. Zero leases and zero bound access-plan rows exist
  at that point; none is written afterwards. `WorkerMissing` remains an
  internal error: the same transaction read the worker at preflight, so
  vanishing mid-transaction is an invariant violation, not a gate.
- Only on success does the control plane write the artifact-access plan
  (#477 shape unchanged) and then create the selected scheduler decision with
  `selected_lease_id` bound at creation. `link_selected_lease_in_tx` loses
  its last caller and is deleted outright — a selected decision row always
  names an existing lease, and no deprecated linking path remains.

The rechecks themselves stay inside the single writer transaction
(`BEGIN IMMEDIATE`): the incarnation fence and node-liveness validation run
at entry, owner locality and active roots/epochs are resolved once in-transaction
and reused for the evidence (ADR 0071's "one resolution, one point in time"),
and readiness/eligibility/capacity are re-derived by the guarded lease write.
Criterion 1 is satisfied by transaction composition, not by new queries.

### Normalize the dispatched operation

`RemoteLeaseDispatch.operation` becomes
`ticket.kind.normalize().matching_token().into_string()`: a known operation
dispatches under its bare wire token whichever encoding arrived; an exact
custom local operation keeps its exact token; an unknown-namespaced kind can
never reach dispatch because the store raises first. The wire shape is
unchanged, so the node-agent client mirror, fakes runner, fake-support
validator, and conformance echo worker keep their contracts; the node agent's
`OperationKind::from_wire` admission guard stays and now receives tokens it
can admit. Custom local operations still cannot execute on a remote child
(#423 owns protocol changes); this decision fixes token fidelity only.

### No schema change

Migration 0037's shapes already express every durable fact this slice adds:
plan uniqueness binds the lease, the decision table already accepts
`selected_lease_id` on selected rows and ticket-naming no-candidate rows. No
migration 0038 is written.

## Consequences

- Every post-selection gate failure is durably observable with its documented
  stable reason, and the acquire response stays replayable through the
  existing idempotency machinery.
- A selected scheduler decision now implies a lease existed at creation;
  inspection surfaces never see a leaseless selection from this path.
- Namespaced byte-work tickets execute identically to canonical ones, and the
  dual-encoding capability workaround disappears from tests and operator
  guidance.
- Local and standalone lease acquisition keep their error-based API; only the
  remote acquire path consumes the structured outcomes.
- Rollback is `git revert` clean: no schema, payload, or wire-shape change
  ships in this slice.

## Alternatives considered

- **Keep raising errors on changed gates.** Rejected: criterion 2 requires
  the documented stable reason; an aborted request records nothing durable.
- **Re-run every preflight check explicitly before the lease.** Rejected as
  duplication: the guarded lease write already rechecks readiness,
  eligibility, and capacity against the same transaction snapshot; mapping
  its outcomes gives the same guarantee with one implementation of each rule.
- **Normalize capabilities and grants to matching tokens at candidate
  building too.** Tempting — it would remove the dual-encoding seeding — but
  ADR 0069 deliberately keeps the candidate-loop lookups total on unmodified
  tokens; changing that is a scheduling-semantics change outside #478's
  scope.
- **Teach the node agent to accept namespaced tokens.** Rejected: two
  accepted wire encodings for one fact is precisely what the normalization
  decision forbids; normalization belongs before dispatch.
- **Persist the normalized token on the ticket or plan.** Rejected: the
  normalized token is derivable from persisted facts alone; storing it adds a
  second representation of one fact.
