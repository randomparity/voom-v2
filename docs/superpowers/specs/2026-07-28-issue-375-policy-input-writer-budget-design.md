# Policy Input Aggregate Writer Budget

Issue: #375

## Goal

Bound the work performed while SQLite's single writer lock is held during
policy-input creation. The maximum supported aggregate must persist atomically
while an unrelated writer waits within the configured 30-second busy timeout.
Oversized drafts must fail before database access with a stable public error
and no durable policy-input or event state.

## Current State

`ValidatedPolicyInputSetDraft::new` checks semantic shape but has no aggregate
size contract. `ControlPlane::create_policy_input_set` validates before opening
`BEGIN IMMEDIATE`, then performs one set-based snapshot provenance read and
passes the draft to `SqlitePolicyInputRepo::create_input_set_in_tx`.

Persistence inserts the root, labels, synthetic targets, snapshots, evidence,
bundle targets, quality profiles, and issues one row at a time. It then
rehydrates the complete aggregate before commit. Whole-library and root-scoped
scan builders use this same path, so both the number of statements and the
amount of JSON/string work are currently unbounded while the writer lock is
held.

The SQLite pool waits up to 30 seconds for a writer lock and up to 45 seconds
for a pooled connection. An aggregate that can hold the writer for longer than
the busy timeout makes unrelated lease, heartbeat, cancellation, and other
control-plane writes fail as `DB_UNREACHABLE`.

## Decisions

### Dual aggregate budget

Every `PolicyInputSetDraft` is limited to:

- 10,000 persisted aggregate members; and
- 32 MiB (33,554,432 bytes) in its serialized JSON representation.

The member count is the saturating sum of all persisted child collections:
fixture labels, synthetic targets, media snapshots, identity evidence, bundle
targets, quality profiles, and issues. The root row is not a member.

The serialized byte count is `serde_json::to_vec` over the complete draft. It
therefore covers root metadata, every string, every JSON value, list
punctuation, and escaping. A small row count cannot bypass the writer budget
with one enormous description or nested payload.

The row limit allows an ordinary whole-scan draft with its one fixture label
to contain 9,999 video snapshots. That covers a multi-terabyte library even at
an unusually small average of 250 MiB per video, while preserving a finite
statement bound. The byte limit allows about 3.2 KiB of serialized facts per
member at the row ceiling. Libraries with larger average stream inventories
can use root-scoped input sets. Both limits are intentionally well below
SQLite's storage limits: they are an operational writer-latency contract.

### Validation and public error contract

Budget validation runs after existing semantic validation and before any
database transaction or provenance read. Existing invalid-draft error
precedence remains unchanged.

Over-budget drafts return the existing `POLICY_VALIDATION_ERROR` code with one
of these messages:

- `policy input aggregate has <actual> members; maximum is 10000`
- `policy input aggregate serializes to <actual> bytes; maximum is 33554432`

The error is actionable without adding a new CLI flag or DSL form: split a
manual draft, or create scan-derived input sets per library root.

The same `ValidatedPolicyInputSetDraft` proof is required by direct store
creation and every control-plane builder, so no write path can bypass the
budget. No schema or persisted wire shape changes.

### Atomic persistence remains

The aggregate remains one transaction. Splitting one logical input set across
independently committed chunks would require a new durable construction state,
reader filtering, recovery semantics, and migration. That is unnecessary once
the accepted aggregate has a measured finite upper bound.

The existing transaction rollback remains the all-or-none boundary. Budget
rejection occurs earlier and therefore creates no root, child, or event rows.

## Verification

1. Domain tests accept exactly 10,000 members and reject 10,001 with the exact
   member-budget message.
2. Domain tests accept exactly 33,554,432 serialized bytes and reject one byte
   more with the exact byte-budget message.
3. A control-plane over-budget test inspects every policy-input table and the
   event log, proving no durable changes.
4. A store test persists an exact-member-boundary aggregate while a separate
   job writer waits behind the same `BEGIN IMMEDIATE` transaction. The
   aggregate commits and the unrelated writer succeeds within the configured
   busy timeout.
5. Existing generic, single-scan, whole-scan, and root-scoped creation tests
   remain green.
6. Focused tests, strict clippy, and `just ci` pass.

## Adversarial Review

- **Count only media snapshots:** rejected. Manual and fixture inputs can put
  unbounded work in every other child table.
- **Count rows but ignore bytes:** rejected. One JSON value or string could
  still monopolize serialization and storage work.
- **Measure only JSON-valued columns:** rejected. Large ordinary strings also
  consume writer time and database pages; complete-draft serialization is a
  clearer contract.
- **Check after `BEGIN IMMEDIATE`:** rejected. An oversized request must not
  reserve the writer while discovering that it cannot proceed.
- **Chunk commits without construction state:** rejected. Readers could observe
  partial aggregates, and rollback would no longer remove the whole input set.
- **Use only a wall-clock assertion:** rejected. Timing thresholds are flaky on
  shared CI. The contention test instead proves the maximum row-boundary write
  completes before SQLite's real busy timeout rejects the waiting writer.
- **Treat the documented empty whole-scan mismatch as part of this issue:**
  rejected. It is an independent semantic contract gap tracked by #391.

## Implementation Plan

1. Add red domain tests for exact and over-boundary member and byte contracts.
2. Add red control-plane/store tests for no durable state and maximum-boundary
   concurrent writer success.
3. Add public budget constants and budget errors to `voom-policy`; preserve
   existing validation precedence and error messages.
4. Route every existing validation-to-`VoomError` conversion through the
   stable budget message helper.
5. Document the operator limit and root-scoped split guidance.
6. Run focused policy/store/control-plane tests, strict clippy, review,
   simplification, and `just ci`.
