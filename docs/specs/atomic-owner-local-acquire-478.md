# Spec: Atomically acquire owner-local byte work (#478)

Governing ADR: `docs/adr/0072-atomically-acquire-owner-local-byte-work.md`.
Predecessors: ADR 0069 (declaration vocabulary, one ticket-kind normalization),
ADR 0070 (resolution + gate), ADR 0071 (persisted owner-local scheduling
evidence), ADR 0068 Consequences (#475 requirement).

## Problem

After #476/#477 the remote-acquire path resolves owner locality inside its
single writer transaction, persists rejection evidence, and writes an
owner-evidence plan row. Two gaps remain against issue #478:

1. **Post-selection guard failures are errors, not documented outcomes.**
   The store-level guarded acquisition (`SqliteLeaseRepo::acquire_guarded`)
   rechecks ticket readiness and worker eligibility/capacity when it creates
   the lease, but every changed-gate result surfaces as a raised
   `VoomError` that aborts the whole acquire: no durable scheduler decision
   records *why*, and no stable reason is produced. The selected scheduler
   decision is also created *before* the lease exists, so a guard failure
   would leave a selected decision row that never leased.
2. **Dispatch carries the raw ticket-kind token.** `RemoteLeaseDispatch`
   sends `ticket.kind.into_string()`. For a workflow-rendered ticket that is
   the reserved-namespaced form (`synthetic.workflow.operation.transcode_video`),
   which `OperationKind::from_wire` rejects in the node agent
   (`crates/voom-node-agent/src/runtime.rs` `dispatch_to_child`). An eligible
   namespaced byte operation cannot execute even though the canonical
   encoding of the same operation would. Tests work around this by registering
   worker capabilities under *both* encodings.

## Design

### Structured changed-gate outcomes (`voom-store`)

`LeaseAcquireOutcome` becomes a total description of the guarded rechecks; a
changed gate is data, not an error:

- `Acquired(Lease)` — unchanged.
- `CapacityFull(WorkerCapacitySaturation)` — unchanged.
- `TicketNotReady { ticket_id }` — the conditional readiness `UPDATE`
  (`state = 'ready' AND next_eligible_at <= now AND attempt < max_attempts`
  and parent job open) matched zero rows.
- `WorkerIneligible { worker_id, operation, reason: LeaseIneligibilityReason }`
  with the closed reason set `WorkerMissing | WorkerStale | WorkerRetired |
  OperationDenied | MissingCapability | MissingGrant`, derived from the same
  facts `require_operation_eligibility` classifies today.

`LeaseAcquireOutcome::into_lease_result` keeps today's exact error mapping
(messages included) for the standalone/local callers
(`SqliteLeaseRepo::acquire`, `ControlPlane::acquire_lease`,
`ControlPlane::try_acquire_lease`); their observable behavior is unchanged.
An unknown-namespaced ticket kind stays a `VoomError::Database` (corruption,
never an eligibility result — ADR 0069). All outcomes other than `Acquired`
roll back the acquire savepoint, so a changed gate mutates nothing.

### Post-selection gates produce documented stable reasons (`voom-control-plane`)

On the selected path the order changes to lease → plan → decision:

1. `recheck_selected_remote_capacity_in_tx` runs first (unchanged).
2. The lease is acquired through the structured outcome API.
3. Every non-`Acquired` outcome maps to **one** durable no-candidate decision
   using the existing twelve-code reason vocabulary, with `ticket_id` set to
   the selected ticket, `candidate_count = 1`, and a suppression key whose
   ticket segment agrees with the row:
   - `TicketNotReady` → `no_ready_ticket`
   - `WorkerStale` / `WorkerRetired` → `worker_not_executable`
   - `OperationDenied` → `operation_denied`
   - `MissingCapability` → `missing_capability`
   - `MissingGrant` → `missing_grant`
   - `CapacityFull` → `worker_capacity_full`
   - `WorkerMissing` → internal error (the same transaction read the worker
     at preflight; vanishing is an invariant violation, not a gate).
   No lease row and no bound access-plan row exist at this point, and none is
   written: the mutation-free property holds by construction.
4. Only on `Acquired` does the control plane write the artifact-access plan
   (owner evidence or explicit absence pair, exactly as #477 defined) and then
   create the selected scheduler decision with `selected_lease_id` bound at
   creation. `link_selected_lease_in_tx` loses its last caller and is deleted
   together with its standalone wrapper — no deprecated path remains.
   A selected decision row therefore always names an existing lease, and any
   changed gate produces the documented stable reason instead of an orphaned
   selection.

### Normalized operation before node-agent dispatch

`RemoteLeaseDispatch.operation` becomes
`ticket.kind.normalize().matching_token().into_string()`:

- a known operation dispatches under its bare wire token whichever encoding
  the ticket kind used, so an eligible namespaced byte operation executes
  exactly like its canonical operation;
- an exact custom local operation keeps its exact token;
- an unknown-namespaced kind can never reach this point (the store raises
  first).

The wire shape of `RemoteLeaseDispatch` is unchanged, so the node agent
client mirror, fakes runner, fake-support validator, and conformance echo
worker need no contract change — they already accept the bare token. The
node agent's `OperationKind::from_wire` guard stays: it now receives tokens
it can actually admit. Custom local operations still fail remote child
dispatch (out of scope, #423); the requirement is token fidelity, not new
execution support.

### Binding exactness (no schema change)

One successful acquisition creates exactly one lease; the plan row binds it
to ticket, worker, acquiring node, owner, and canonical access evidence
(`UNIQUE(lease_id)`); the selected decision binds decision ↔ lease. All of
this already has schema support from migration 0037 — **migration 0038 is not
needed** and none is written.

### Nothing synthesized

No code path gains a fallback: a non-owner or unresolvable declaration is
rejected exactly as #476/#477 decided, no path/shared-mount proof or transfer
ticket appears anywhere, and the absent-pair plan shape stays the only way to
record "no declared byte work".

## Test matrix (sibling `*_test.rs` / integration suites, real SQLite, ManualClock)

1. **Store outcomes** (`voom-store` leases tests): each structured outcome is
   produced by seeded state — not-ready ticket (attempts exhausted / job
   closed / state moved), stale worker, denied, missing capability, missing
   grant — and `into_lease_result` preserves today's exact error strings;
   savepoint rollback leaves zero lease rows and the ticket state untouched.
2. **Changed-gate decisions** (control-plane acquire tests): one test per
   mapped reason asserting the durable decision row (kind, outcome, reason,
   ticket_id, candidate_count, suppression-key ticket segment) and `COUNT(*)`
   zero on `leases` and `artifact_access_plans`.
3. **Successful owner-local binding**: one acquisition proves the atomic
   triple — exactly one lease, one plan row whose `owner_node_id` +
   `access_evidence` equal the resolution the gate proved, one selected
   decision with `selected_lease_id` = that lease and equal typed evidence.
4. **Normalized dispatch**: control-plane unit/integration coverage that a
   namespaced byte-work ticket dispatches with the bare token while an exact
   custom local token passes through unchanged.
5. **API route** (`voom-api/tests/remote_execution_route.rs`): the leased
   response over HTTP carries the normalized operation and the plan identity
   (`id`, `owner_node_id`, `access_evidence`) for a namespaced byte-work
   ticket.
6. **Node agent** (`voom-node-agent/src/runtime_test.rs`): a coordinator
   handed a leased dispatch containing an owner-local plan forwards the
   normalized operation to the child process request.
7. **Fake worker end-to-end** (`voom-fakes/src/remote_runner_test.rs`): a
   workflow-namespaced byte-work ticket is acquired, dispatched, heartbeated,
   and completed by the synthetic runner — identical outcome to the canonical
   encoding.
8. **Conformance** (`voom-conformance/tests/echo_worker.rs`): the echo worker
   executes the protocol-level dispatch of a known operation regardless of the
   upstream ticket-kind encoding that produced it — pinning that
   normalization happens before dispatch and never reaches the wire as a
   namespaced token.
