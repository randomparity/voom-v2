# Spec: Fenced node-local verification and commit intents (#422)

ADR: [0074](../../adr/0074-fenced-node-local-commit-intents.md) ·
Issue: randomparity/voom-v2#422 · Branch:
`feat/fenced-node-local-commit-intents-422` · Base: `main`

## Goal

Staged artifact commits execute on the storage-owner node behind a durable,
fenced authorization. The control plane prepares, authorizes, finalizes, and
recovers; it never opens staging or target bytes.

## Normative guarantees

Each traces to an issue acceptance criterion (AC1–AC6) or a necessary
consequence recorded in ADR 0074:

- G1 (AC1): A stale lease, root owner, node epoch/incarnation, location
  epoch, or wrong/absent commit fence prevents mutation and finalization.
- G2 (AC2): While an intent is `pending`/`authorized`, conflicting blocking
  use leases are refused on its pinned scope, and alias/location changes
  cannot join the authorized scope (the scope is exactly the pinned rows;
  pinned-row drift fails authorization and any later receipt/finalize).
- G3 (AC3): Replayed authorize and complete calls return the original
  outcome idempotently.
- G4 (AC4): An ambiguous promotion never becomes an untracked successful
  commit; completion requires exact fence + matching evidence.
- G5 (AC5): Recovery distinguishes not-started, promoted, mismatched, and
  operator-required states from node receipts.
- G6 (AC6): Add-only install, backup, verification, audit-event, and
  use-lease guarantees are preserved (no-replace install; gate re-evaluated
  at authorize; every state transition emits its event).

## Architecture

Flow for one staged commit (all steps durable):

1. **Prepare** (control plane tx): as today — source facts, target root +
   locator resolution, verified staging location + verification pin,
   lineage safety-gate evaluation, pending `artifact_commit_records` row.
   Additionally creates the pending `artifact_commit_intents` row pinning:
   handle, source file version, verification id, staging location id +
   epoch, target root id + `root_epoch`, target locator, expected facts
   `{size_bytes, content_hash}` taken from the pinned verification, owner
   node resolved from the root's `owner_node_id`. Local byte observation in
   prepare is removed; freshness moves to the node's pre-mutation verify.
2. **Authorize** (node pull): the agent polls a fetch route returning
   pending intents for roots it owns; it requests authorization per intent.
   In one control-plane tx: re-run the lineage gate; revalidate pinned
   epochs unchanged; confirm requesting node = current root owner with an
   active incarnation and fresh heartbeat; transition `pending ->
   authorized` (CAS on intent `epoch`); mint a random 32-byte fence; store
   it; return the fenced payload (staging/target locators, expected facts,
   fence). Drift or a live blocking lease aborts fail-closed (`Conflict`).
3. **Applying journal** (node → route): before touching bytes the node
   reports `applying`; the route records the receipt durably. The node
   mutates nothing if this report cannot succeed. Route guards:
   `require_remote_incarnation_fence_in_tx` + intent `authorized`.
4. **Verify** (node local): observe staging bytes; compare size + hash to
   expected facts. Drift → report `mismatched` evidence; no mutation.
5. **Promote** (node local): copy to unique temp sibling; install without
   replacement (add-only semantics ported from the retired host promote);
   fsync file parent directories; observe target facts.
6. **Complete** (node → route): report `applied` + observed target facts +
   fence. Control plane validates fence (exact match, unconsumed), applied
   receipt present, epochs still pinned, then runs the existing finalize
   transaction (result version/location, retire staging, mark committed)
   and marks the intent `completed`, consuming the fence. Emits completion
   events. Idempotent replay via `remote_idempotency_keys`.
7. **Recovery**: lost responses/crashes/stale authorization land the record
   in `recovery_required`. `recover_commit` classifies from receipts:
   a receipt-less authorized intent is safe to abort and re-prepare a
   successor generation — the `applying` journal is the mutation gate (the
   node mutates only after its `applying` receipt is durably accepted; a
   late report fails the CAS and the node stands down); an intent with any
   receipt classifies as `applied` + matching target facts → finalize
   directly; `mismatched` / `outcome_unknown` / stale-authorization drift →
   operator-required (record stays `recovery_required`, evidence carried).
   Pending intents whose owner node is stale or retired abort fail-closed.

## Components and files

| Crate | Change |
|---|---|
| `migrations/0038_artifact_commit_intents.sql` | New STRICT table, CHECK-coherent states, json_valid columns |
| `crates/voom-store` | Migrator entry + schema-test bump; `repo/media/artifact_commit_intents.rs` repo (+ tests) |
| `crates/voom-events` | Payloads: intent recorded/authorized, receipt reported (kinds `not_started`/`applying`/`applied`/`mismatched`/`outcome_unknown`); no fence value ever serialized |
| `crates/voom-control-plane` | Commit path rework: prepare pins intent; authorize/complete/receipt case functions; recovery classification; delete host promotion code |
| `crates/voom-api` | `commit.rs` routes under `/v1/artifact/commit/…` following `execution.rs` handler pattern |
| `crates/voom-node-agent` | Client methods (`RetryRequest`), coordinator task polling intents, node-side verify+promote module (ported add-only install) |
| docs/scripts | ADR 0074 + index row; payload-contract inventory/scope entries |

### Node API routes

- `POST /v1/artifact/commit/{intent_id}/authorize`
- `POST /v1/artifact/commit/{intent_id}/applying`
- `POST /v1/artifact/commit/{intent_id}/complete`
- `POST /v1/artifact/commit/{intent_id}/mismatched` (typed failure evidence)
- `GET`-free design: pending-intent discovery rides `authorize` responses of
  a `POST /v1/artifact/commit/pending` listing scoped to the caller's owned
  active roots.

All requests carry bearer node token + `X-Voom-Idempotency-Key`; bodies are
`deny_unknown_fields`; envelopes/errors identical to `execution.rs`.

## Threat model

- **Boundaries added**: four new authenticated node-facing HTTP routes
  (widening the existing `/v1/execution` + `/v1/scan` authenticated node
  surface). Actor: an authenticated remote node (possesses a node token);
  anonymous internet is rejected at the bearer check as today.
- **Controls**: every route authenticates via the shared
  `require_remote_incarnation_fence_in_tx` primitive (token hash compare,
  remote-node kind, active incarnation, optional worker binding) plus
  per-request liveness; authorization to mutate one intent additionally
  requires the requester to be the root's current owner and the fence to
  match at completion. Bodies are typed `deny_unknown_fields`; locators are
  provider-relative strings validated by the existing locator checks; the
  node resolves paths only within roots it owns (containment mirrors the
  verify worker's staging-root rule). Fence values never enter events,
  logs, or replay payloads (replay stores only outcomes).
- **Out of scope**: malicious storage-owner node corrupting bytes it
  already owns (the node is the byte authority per ADR 0050; integrity is
  bounded by the content-hash pinning at verification and finalize);
  TLS/transport security (owned by the API server config, ADR 0054).

## Testing

- Store: migration applies; repo CAS transitions, fence mint/consume,
  receipt writes, replay behavior.
- Control plane: authorize fail-closed on each drift axis (lease, epochs,
  incarnation, ownership); complete fence validation; recovery
  classification across the four evidence states; idempotent replays;
  existing staged-flow/gate suites migrated to drive the node half through
  the case functions.
- API: route auth (401/404/409 paths), envelope shape, replay headers.
- Node agent: verify+promote against temp dirs (match, mismatch, existing
  target); add-only no-replace preserved; journal-before-mutate ordering.
- Guardrails: `just ci` green before push.

## Failure modes mapped to tests

| Failure | Expected outcome |
|---|---|
| Blocking lease acquired between prepare and authorize | Authorize fails; intent aborted; no mutation |
| Root reassigned / epoch bumped after authorize | Receipt + complete rejected; record recovery_required |
| Staging bytes drift before promote | `mismatched` receipt; no mutation |
| Crash after applying, before reporting | Stale applying → operator-required evidence |
| Promote done, completion lost | Node replays complete; finalize converges once |
| Replayed authorize/complete | Original outcome returned; no new rows/events |
