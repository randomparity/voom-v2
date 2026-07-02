# 0019 — Lineage-commit safety-gate check runs in the prepare transaction

## Status

Accepted

## Context

The commit safety gate (`crates/voom-store/src/repo/media/commit_safety_gate/`)
is implemented and unit-tested but has zero production callers (#270). The
artifact commit path (`crates/voom-control-plane/src/artifact/commit/`) must
consult it so a fresh blocking use lease fails a commit end-to-end.

Two structural facts constrain the wiring:

1. **The gate's public lifecycle does not fit an additive commit.** The gate's
   `prepare_destructive_commit` / `authorize_destructive_commit` /
   `finalize_destructive_commit` protocol is built around
   `CommitTarget::{DeleteFileLocation, ReplaceFileLocation, MoveFileLocation}`,
   `commit_intents` rows, closure-drift epochs, an external `AliasResolver`, and
   accepted-evidence revalidation. The artifact commit is a **lineage /
   additive** operation: it creates a new `FileVersion` on the existing
   `FileAsset` and installs bytes at a **new** path (add-only, fails if the
   target exists). None of the destructive `CommitTarget` shapes describe it.

2. **The gate opens its own `BEGIN IMMEDIATE` transactions** (`begin_gate_tx`).
   The commit path already runs inside its own `begin_tx` transactions. Invoking
   a gate lifecycle entry point from inside the host commit transaction would
   nest a second connection/transaction against the first — on the
   single-connection test pools and in general a composition and deadlock
   hazard the gate's own docs call out.

3. **The commit's irreversible filesystem mutation sits between two host
   transactions.** prepare (tx) → promote (hard-link, no tx) → finalize (tx).
   The spec requires the lease check "inside the host-side transaction that
   records the commit … immediately before any irreversible filesystem
   mutation" (design §1187–1190). Only the **prepare** transaction runs before
   promote.

## Decision

Add one focused, clock-aware, public store-layer primitive to the
`commit_safety_gate` module and call it from the commit path's **prepare
transaction**, before the durable `pending` record and before promote.

- `check_lineage_commit_leases_in_tx(tx, identity_repo, file_asset_id,
  file_version_id, now)` builds the affected-scope closure for a lineage commit
  (asset + version + the version's live locations + the asset's bundles) and
  runs a single clock-aware blocking-lease overlap query on the **caller's**
  transaction. It does not open a gate `BEGIN IMMEDIATE` transaction.
- A live blocking lease that is not terminal and not TTL-expired fails the
  commit as a pre-mutation error (`VoomError::BlockedByUseLease`), reusing the
  existing `ArtifactCommitFailedPreMutation` event path. No `pending` record,
  no filesystem mutation, no orphan target.
- The leases the gate considered are threaded through `PreparedCommit` and
  recorded in `ArtifactCommitCompletedPayload.gate_evaluated_lease_ids`
  (additive `#[serde(default)]` field, ADR 0013 contract) at finalize.

Freshness follows design §1235–1241: terminal (`release_reason` set) and
TTL-expired (`ttl_bound = 1 AND expires_at < now`) leases do not block; manual
locks block until terminal.

## Consequences

- A blocking lease now fails commits on the primary path (remux, audio- and
  video-transcode all route through `commit_artifact`), satisfying the spec's
  central safety promise.
- The check is a plain in-tx query on the host transaction — no nested
  transactions, no new commit-failure state, no filesystem cleanup logic.
- The audit event records which leases were evaluated.
- The new clock-aware query is a small duplication of the destructive gate's
  overlap SQL. This is deliberate: the two paths differ in closure construction
  (a `CommitTarget` location vs. a source version) and in freshness handling,
  and keeping them separate leaves the heavily-tested destructive gate
  untouched (surgical-change rule).
- The window between the prepare-time check and promote is not closed by a
  recompute (the spec's recompute-under-isolation behavior). For this add-only
  commit that window is small and the mutation is non-destructive; closing it
  belongs with the destructive gate. Documented as a follow-up.

## Considered & rejected

- **Drive the full gate intent lifecycle
  (`prepare_/authorize_/finalize_destructive_commit`).** Rejected: semantic
  mismatch (no `CommitTarget` describes an additive commit) and it nests
  `BEGIN IMMEDIATE` gate transactions inside the host commit transaction.
- **Check inside the finalize transaction instead of prepare.** Rejected:
  promote (the irreversible hard-link) has already run by finalize, so blocking
  there orphans the installed target file and forces recovery-state semantics
  for what is a clean policy refusal. Prepare is strictly before the mutation.
- **Make the existing `blocking_lease_rows_in_tx` clock-aware and reuse it.**
  Rejected for this issue: it changes destructive-gate behavior and its large
  test surface, exceeding the issue's scope. The TTL-expiry gap there is filed
  as a separate follow-up.
- **Query `asset_use_leases` directly from the control-plane layer.** Rejected:
  voom-store owns that table; the control plane consults it through the
  store/gate API (crate layering).
