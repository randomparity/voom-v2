# ADR 0074: Fenced node-local verification and commit intents

## Status

Accepted

## Context

Issue #422 closes the staged-commit leg of the transitional control-plane
filesystem-promotion path (ADR 0050, AGENTS.md). Today
`crates/voom-control-plane/src/artifact/commit/` observes staging facts,
copies bytes, hard-link-installs, and fsyncs — all on the SQLite host — and
`recovery.rs` re-opens those local paths when re-driving a stuck commit. The
control-plane design spec (`docs/specs/voom-control-plane-design.md`
§1433–1457) already prescribes the target protocol: a durable, fenced
commit intent where the storage-owner node verifies and promotes bytes and
the byte-blind control plane retains authorization and catalog authority.
The pull-based prerequisites have shipped: HTTPS API (#416), the polling
node agent (#417), provider-relative locations (#418), owner-locality
scheduling (#420).

Two adjacent vocabularies must not be conflated: the existing `commit_intents`
table (migrations 0004/0005 pre-squash) is the *destructive identity-mutation*
safety gate. This ADR adds a separate artifact byte-promotion intent for the
add-only staged-commit path.

## Decision

### One durable state machine per staged commit

Migration 0038 adds `artifact_commit_intents`, 1:1 with its
`artifact_commit_records` row. States follow the design-spec vocabulary:
`pending -> authorized | aborted`, `authorized -> completed |
recovery_required`, `recovery_required -> completed | aborted`; terminal
states never reopen — a retry prepares a successor generation. The row pins
the full authorized scope at creation: artifact handle, source file version,
verification id, staging location id + epoch, target root id +
`root_epoch`, target provider-relative locator, expected file facts
(`{size_bytes, content_hash}` as a typed JSON column under the ADR 0013
deny-unknown-fields contract), and the resolved owner node. Authorization
additionally records the owner incarnation id and a one-time opaque 32-byte
`commit_fence`. Node-reported receipts (`not_started`, `applying`,
`applied`, `mismatched`, `outcome_unknown`) with observed facts land in a
typed JSON receipt column. Every transition is a compare-and-set on an
optimistic `epoch` column, mirroring `commit_safety_gate`.

### Authorization is the control plane's fail-closed gate, re-run late

The node requests authorization for a pending intent over an authenticated
route. Inside one transaction the control plane re-evaluates the lineage
commit safety gate (ADR 0019 semantics), revalidates every pinned epoch
(staging location, root), confirms the requesting node still owns the root,
holds an active incarnation, and is live, then transitions `pending ->
authorized`, mints the fence, and returns the fenced payload (locators,
expected facts, fence). Any drift aborts the intent fail-closed; the caller
sees `Conflict`. While an intent is `pending`/`authorized`, new blocking
use leases on its pinned scope are refused by lease acquisition, exactly as
`consult_pending_commit_lock_in_tx` does for destructive intents, so a
conflicting lease can never enter an authorized scope after the gate passes.

### The node verifies, promotes add-only, and journals before mutating

voom-node-agent gains a coordinator task that polls pending intents for the
roots it owns. For each: it reports `applying` first and performs no
mutation if that journal write cannot succeed; verifies observed staging
facts against the pinned expected facts (`mismatched` evidence and no
mutation on drift); copies to a unique temp sibling and installs without
replacement (semantics ported verbatim from the retired host-side promote);
fsyncs; then reports `applied` with observed target facts. Receipts ride
authenticated routes guarded by `require_remote_incarnation_fence_in_tx`;
every receipt and completion revalidates the pinned epochs, so a stale node
epoch, location epoch, or superseded root ownership prevents any mutation or
finalization after the fact.

### Completion consumes the fence once

Completion requires the exact `commit_fence` plus matching applied evidence.
The control plane validates fence and facts in the finalize transaction that
creates the result version/location, retires staging, and marks the record
`committed`; the intent ends `completed` and the fence is consumed.
Authorize and complete replay idempotently through the existing
`remote_idempotency_keys` mechanism: a replayed authorize returns the
original fenced outcome, a replayed complete the original report — neither
re-mints, re-finalizes, nor creates rows.

### Ambiguity converges, never succeeds silently

A lost response, agent crash between `applying` and reporting, or expiry
after authorization leaves the record `recovery_required` with typed
evidence. Recovery classifies from node receipts: a receipt-less authorized
intent is safe to abort because the `applying` journal is the mutation gate —
the node mutates only after its `applying` receipt is durably recorded, so a
late report fails the compare-and-set and the node stands down; an intent
carrying any receipt (`applying` or later) is never self-aborted — it becomes
`recovery_required` and classifies as `applied` with matching facts
(finalize directly; promotion already happened), `mismatched` or
`outcome_unknown` (operator-required: the record stays `recovery_required`
carrying the evidence for a human). An ambiguous promotion can therefore
never surface as an untracked successful commit.

### Pending intents expire fail-closed

A `pending` intent holds no fence, so aborting it is always safe. The
recovery path aborts a pending intent whose owner node is stale or retired
(the same liveness rule the execution routes apply), and `commit_artifact`'s
bounded wait surfaces the timeout to its caller meanwhile. Abort releases
the lease-refusal the intent held, so one dead node cannot freeze a lease
scope indefinitely. Authorized intents never expire into abort — they enter
`recovery_required` with the fence still blocking, per the classification
above.

### Host-side promotion is deleted, not demoted

`ControlPlane::commit_artifact` becomes prepare + authorize issuance and a
bounded wait for terminal convergence; `recover_commit` drives the evidence
classification above. The host filesystem-promotion code in the commit path
(`promote.rs` byte operations, local observation in prepare/recovery) is
removed from the control plane. `voom-api` gains the authenticated node
routes; `voom-events` gains typed payloads for intent recorded/authorized
and each receipt kind (never carrying the fence value). New JSON columns are
registered in `docs/payload-contract-inventory.md` and
`scripts/payload-contract-scope.txt`.

## Consequences

- The control plane never opens staging or target bytes on the commit path;
  byte work lives behind the fence on the storage-owner node (ADR 0050).
- Commit completion latency now includes a node poll cycle; callers wait on
  durable state instead of in-process filesystem work.
- A node crash anywhere in the sequence lands in a distinguishable recovery
  state with typed evidence, replacing today's host-local path probing.
- The fence and epoch pins make stale authorization inert: mutations
  attempted under a superseded scope fail closed at receipt, completion, or
  finalization.
- Rollback is a schema-preserving revert of migration 0038-era code; the
  additive table does not obstruct the prior binary.

## Alternatives considered

- **Reuse the destructive `commit_intents` table for byte promotion.**
  Rejected: verified — its closure/evidence columns model identity-mutation
  scopes (`target_row_epochs`, accepted evidence ids) that do not exist on
  the add-only staged-commit path, and its transitions are consulted by the
  destructive-gate machinery; conflation would couple two different safety
  semantics (issue triage; `crates/voom-store/src/repo/media/commit_safety_gate.rs`).
- **Keep promotion on the control plane but wrap it in the intent state
  machine.** Rejected: judgment — it preserves the single-host assumption
  ADR 0050 retires and leaves the control plane byte-aware, which the issue
  forbids outright ("the control plane never opens staging or target bytes").
- **Push execution orders from the control plane to nodes over a new
  node-facing server.** Rejected: verified — the node agent exposes no
  inbound control-plane endpoint and speaks only as an HTTP client
  (`crates/voom-node-agent/src/client.rs`); a push channel would add an
  unauthenticated-surface-bearing server the pull model makes unnecessary.
- **Do nothing — keep today's host-local promotion.** Rejected: verified —
  the issue's expected behavior states "the control plane never opens
  staging or target bytes", and ADR 0050 retires the single-host assumption
  this path embodies; AGENTS.md marks the path transitional pending #422.
- **Carry promotion through the owner-local byte-work ticket chain (ADRs
  0069–0073).** Rejected: judgment — that chain is scheduler-dispatched and
  scoped to the worker-lease lifecycle, which cannot express a
  control-plane-minted one-time fence, a 1:1 coupling to an
  `artifact_commit_records` row, or authorization re-validated outside a
  scheduler decision.
- **Pin only the target locator, not lineage/location epochs.** Rejected:
  judgment — without epoch pins a staging swap or root reassignment between
  authorize and apply would let bytes from a different generation be
  promoted under the old authorization, defeating criterion 1.
