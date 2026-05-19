---
title: VOOM Sprint 1 M3 Phase 2 — Commit Safety Gate Implementation Plan
status: draft
created: 2026-05-18
parent_spec: docs/superpowers/specs/2026-05-17-voom-sprint-1-m3-design.md
parent_sections: §2 Phase 2 (sub-slices 1–11), §3 exit criteria, §4.3 M2 touch-back, §5.1 lock-helper placement, §5.2 two-tx pattern
arch_spec: docs/specs/voom-control-plane-design.md (Commit Safety Gate, Ingest Identity Invariants rename exemption)
sprint_spec: docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md (§§9.1, 9.2, 9.3, 9.4, 12.1)
branch: feat/sprint-1-m3-phase-2
scope: sequencing plan for Phase 2 implementation — no design changes
---

# VOOM Sprint 1 M3 Phase 2 — Commit Safety Gate Implementation Plan

## 1. Purpose

The M3 sequencing doc lists eleven Phase 2 sub-slices in dependency
order. This plan turns that into a concrete branch + commit ordering
with green `just ci` at each commit. No design changes; when this plan
and the parent specs disagree, the parent specs win.

Phase 0 (migration `0004_use_leases_ancillary.sql`) and Phase 1 (all
seven `UseLeaseRepo` lifecycle methods + integration test +
control-plane wiring) are already merged on `main`. Sub-slice 10
(lease re-anchoring on rename) was wired into `reconcile_rename_in_tx`
as part of PR #21 (`c36757f`); this plan does not redo it.

## 2. Branch

`feat/sprint-1-m3-phase-2`, cut from `main` at the head that contains
PRs #21 through #37. All ten implementation commits land on this
branch; one PR opened against `main` at the end of the branch, after
one adversarial-review round per project convention.

## 3. Pre-decided judgment calls

Captured here so the per-commit plan below is unambiguous:

| Decision | Choice | Rationale |
|---|---|---|
| Skeleton placement | Minimal scaffold first (commit 1) | Cross-crate type additions and the empty module land together so the algorithmic commits don't drag `voom-core` deltas. Pragmatic deviation from sequencing doc §2 step 1 "types ride with their use-site". |
| `ForcePathToken` plumbing | All force-path plumbing in commit 10 | Define the struct stub in commit 1 for compile-stability, but do **not** thread it through prepare/authorize until commit 10. Commit 10 retrofits both signatures to accept `Option<ForcePathToken>`, ships the JSON serde, bypass-set validation, `commit.forced_override` emission, and the `closure_incomplete` bypass branches together. Revised from "stub-then-fill" after Codex finding #2 (bypass logic landing six commits ahead of audit/validation). |
| `ArchiveFileVersion` `CommitTarget` variant | Deferred to Sprint 5+ | `file_versions` (migration 0003) carries `retired_at` + `epoch` only — no `archived_at` column. Parent §9.3.1 already defers `ArchiveBundle` and `DeleteBundle` for the same reason ("the schema does not carry the soft-delete/archive columns those targets need"); `ArchiveFileVersion` was overlooked in that paragraph (see §9 follow-up). This plan mirrors the existing precedent and omits the variant entirely. Revised after Codex finding #1. |
| Per-member epoch guard | Snapshot at Phase B (persisted in DB), verify at Phase C | `commit_intents.target_row_epochs TEXT` (JSON array of `[kind, row_id, epoch]` triples) added by migration 0005; Phase B writes it atomically with the `state = 'authorized'` transition. `CommitPermit` is opaque (`pub(crate)` fields, accessor-only public surface) and carries no per-member epochs; Phase C re-reads them from DB by `commit_id` inside the finalize tx and dispatches with `expected_epoch` arguments. The stale-target-epoch trip-wire is authoritative against the DB (no caller-controlled bypass) and the permit is reconstructible after a process crash between authorize and finalize. Revised after Codex round-3 durability concern and round-4 finding #1; the original plan held the snapshot inline on `CommitPermit`. |
| Recovery-required reason storage | Dedicated `commit_intents.recovery_reason TEXT` column | Migration 0004's CHECK constraint required `abort_reason IS NULL` for `state = 'recovery_required'`, so the original `AbortReason::StaleTargetEpoch` path could not be stored in `abort_reason`. Splitting the column at the schema layer (keeping `AbortReason` as a single Rust enum) gives recovery-tooling a single-column query (`WHERE recovery_reason IS NOT NULL`) and keeps the semantic split — "we cancelled before mutating" vs "we mutated but need operator intervention" — visible in the row shape. Migration 0005 ties the column to `state = 'recovery_required'` via CHECK. Revised after Codex round-4 finding #2. |
| Branch scope | One branch, ~10 commits | Matches PR #21's pattern of carrying all of Phase 1 on a single branch. One adversarial-review round at end. |
| `FailureClass::into_error_code` remap | Rides commit 1 | `StaleIdentityEvidence` / `ClosureResolutionIncomplete` / `BlockedByActiveUseLease` currently route to `ApprovalRequired` as a placeholder. The proper `ErrorCode` variants land in commit 1, so the mapping switches in the same commit. Small M1 cleanup riding inside a Phase 2 commit, approved as part of branch scope. |
| `CommitTarget::MoveFileLocation` vs `ReplaceFileLocation` | Same `IdentityRepo` mutation | Parent §9.3.2 Phase C step 4 dispatches both to `replace_file_location_in_tx`. The gate distinguishes them only for `target` payload audit. |
| `FileLocationProposal` shape | Dedicated struct without `file_version_id` | The version is inferred from the retired row inside Phase C. Makes the cross-version replacement bug unrepresentable at the type level. Revised after Codex round-2 finding #1 (alias allowed retiring location on version A and inserting under version B). |
| `AffectedScopeClosure` collections | `BTreeSet<_>` for every ID set | Canonical ordering + dedup at the type level. Snapshots compare correctly regardless of SQL-result order; `commit_intent_scope_members` writes derived from the closure cannot emit duplicates. `resolution_warnings` stays `Vec` (audit ordering matters; duplicates allowed). The `BlockedByClosureGrew` variant carries a typed `ClosureMemberDelta` so the 8-set shape has a single source of truth. Revised after Codex round-2 finding #2 and round-3 finding on warnings-in-equality. |
| `BlockedByPendingCommit` payload | Inline `offending_scope` into the variant; remove the parallel `Detail` structs | Variants self-document the offending scope so blocked callers can make scoped wait/takeover decisions without a race-prone re-query. `BlockedByPendingCommitDetail` and `BlockedByClosureGrewDetail` are deleted — both were dead-code duplicates of the inline variant shapes. Revised after Codex round-2 finding #3 + parallel duplication on `BlockedByClosureGrew`. |
| Closure drift comparison | Dedicated `AffectedScopeClosure::id_member_delta` returning `ClosureMemberDelta` | Derived `PartialEq/Eq` on `AffectedScopeClosure` includes `resolution_warnings`, which would falsely trigger `BlockedByClosureGrew` when only audit warnings differ between Phase A and Phase B snapshots. The explicit delta method excludes warnings by construction. Derived equality stays for the five embedder types (`CommitIntent`, `CommitPermit`, `CommitGateOutcome`, `PendingCommitIntent`, `MutationOutcome`) so test/general equality still works. Revised after Codex round-3 finding (medium). |

## 4. Commit-by-commit plan

Every commit must end `just ci` green. Integration tests land
alongside the slice that introduces the code path under test.

### Commit 1 — Cross-crate scaffold

**`voom-core::ids`**
- Add `CommitId` via `define_id!`.

**`voom-core::error`**
- Add `ErrorCode` variants: `BlockedByUseLease`, `BlockedByPendingCommit`, `BlockedByClosureGrew`, `StaleIdentityEvidence`, `ClosureResolutionIncomplete`.
- Add matching `VoomError(String)` variants.
- Extend `ErrorCode::as_str` and `VoomError::error_code` mappings.

**`voom-core::failure`**
- Rewire `FailureClass::into_error_code`:
  - `StaleIdentityEvidence` → `ErrorCode::StaleIdentityEvidence` (was `ApprovalRequired`).
  - `ClosureResolutionIncomplete` → `ErrorCode::ClosureResolutionIncomplete` (was `ApprovalRequired`).
  - `BlockedByActiveUseLease` → `ErrorCode::BlockedByUseLease` (was `ApprovalRequired`).
- Update `failure_test.rs` assertions.

**`voom-store::repo::commit_safety_gate`** (new module, registered in `repo/mod.rs`)
- Public type stubs only — no algorithms yet:
  - `CommitTarget` — variants for Sprint 1: `DeleteFileLocation`, `DeleteFileVersion`, `ReplaceFileLocation`, `MoveFileLocation`. (`ArchiveFileVersion` is omitted; deferred to Sprint 5+ per §3 — schema has no `archived_at` column.)
  - `AffectedScopeClosure`, `DestructiveCommit`, `CommitIntent`, `CommitPermit`, `CommitGateOutcome`, `CommitGateResult`, `CommitIntentState`, `MutationOutcome`, `AbortReason`, `TargetMemberKind`.
  - `CommitPermit` is opaque: fields are `pub(crate)`; external consumers reach state via `commit_id()` / `authorized_at()` / `closure_authorized()` / `evaluated_lease_ids()` / `revalidated_evidence()` / `epoch()` accessors. The permit carries NO per-member epoch snapshot — that snapshot is persisted to `commit_intents.target_row_epochs` (migration 0005) and re-read by Phase C from the DB by `commit_id`. `TargetMemberKind` derives `Serialize, Deserialize` with `#[serde(rename_all = "snake_case")]` so the JSON column form matches the gate's existing vocabulary. Revised after Codex round-4 finding #1 — the original scaffold's all-pub `target_row_epochs: Vec<(TargetMemberKind, u64, u64)>` field allowed callers to bypass the stale-epoch trip-wire and lost the snapshot on process crash between authorize and finalize.
  - `CommitGateResult` variants for Sprint 1: `Allowed`, `BlockedByUseLease`, `BlockedByPendingCommit`, `BlockedByClosureGrew`, `BlockedByClosureIncomplete`, `BlockedByStaleEvidence`, `BlockedByStaleTargetEpoch { drift }`, `CancelledAfterAuthorize`.
  - `ForcePathToken { actor: String, reason: String, bypass: BTreeSet<BypassKind> }`, with `BypassKind` enum carrying just `ClosureIncomplete` for Sprint 1. **Stub only** — defined for compile-stability, but no caller in commits 1–9. Commit 10 retrofits `prepare_destructive_commit` and `authorize_destructive_commit` to accept `Option<ForcePathToken>` and ships the serde + validation + bypass branches in one slice.
  - `ClosureWarning`, `ClosureFailure`, `EvidenceDrift`, `EvidenceRevalidationResult`, `PendingCommitIntent`. (Previously enumerated `BlockedByPendingCommitDetail` / `BlockedByClosureGrewDetail` parallel structs are deleted per §3 — the inline variant bodies on `CommitGateResult` are now the single source of truth for both shapes.)

**Sibling test** `commit_safety_gate_test.rs`: constructor + Debug round-trip smoke for the public types.

**Exit:** `just ci` green; M1 sibling tests still pass against the rewired `FailureClass` mapping; the new types are reachable from `voom-store` consumers but no algorithm uses them yet.

### Commit 2 — `AliasResolver` trait + impls (sub-slice 2)

- Add `AliasResolver` trait + `AliasResolutionError` enum to `commit_safety_gate`.
- Add `SqliteAliasResolver` impl: returns every live `FileLocation` row on the supplied `FileVersionId`.
- Add `FailingAliasResolver` to `voom-store::test_support` under the existing test-support feature gate: takes a configured set of `FileVersionId`s and returns `AliasResolutionError::Unreachable` for them.

**Sibling tests** cover `SqliteAliasResolver` returns the live set; `FailingAliasResolver` returns `Unreachable` for configured IDs and the empty set otherwise.

**Exit:** `just ci` green; both resolvers exercised in unit tests; integration tests still empty (need Phase A/B to consume them).

### Commit 3 — `IdentityRepo` destructive `_in_tx` mutations (sub-slice 3)

Three new methods on `IdentityRepo` trait + `SqliteIdentityRepo` impl. Pure additions — no callers yet. Every signature takes `expected_epoch: u64` to match the existing M2 retire-method convention (`identity.rs:557–624`):

- `retire_file_location_in_tx(tx, location_id, retired_at, expected_epoch) -> Result<(), VoomError>` — guarded UPDATE on `file_locations` (`WHERE id = ? AND epoch = ?`); sets `retired_at`, bumps `epoch`. Returns `VoomError::Conflict` on a row already terminal **or** on `expected_epoch` mismatch (matching M2 soft-delete semantics; not idempotent).
- `retire_file_version_in_tx(tx, version_id, retired_at, expected_epoch) -> Result<(), VoomError>` — guarded UPDATE on `file_versions`; sets `retired_at`, bumps `epoch`. Live `FileLocation` rows under the version remain (Phase C's caller decides whether to cascade-retire via `replace_file_location_in_tx`).
- `replace_file_location_in_tx(tx, retired_id, retired_expected_epoch, new_proposal, retired_at) -> Result<FileLocationId, VoomError>` — atomically retires `retired_id` under its `expected_epoch` guard and inserts a new `FileLocation` on the same `FileVersion`. Single tx; either both steps land or neither.

(`ArchiveFileVersion` was previously a fourth method; removed because the schema has no `archived_at` column. See §3 deferral and §9 follow-up.)

**Sibling tests** in `identity_test.rs`: each method asserts the column shape, epoch bump, and the Conflict path on a row already in the target state. **Additionally**, each method gets a dedicated sibling-test row asserting the `expected_epoch` mismatch path returns `Conflict` — including the case where the row is live (not terminal) but the caller's snapshot epoch is stale. This is the guard Phase C will rely on.

**Exit:** `just ci` green; mutations sibling-tested for both terminal-row and stale-epoch Conflict paths; no integration test (no caller yet).

### Commit 4 — `prepare_destructive_commit` (Phase A) (sub-slice 4)

**`voom-events`**
- Add `SubjectType::CommitIntent`.
- Add `EventKind` variants: `commit.intent_recorded`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete`.
- Add `Event` payload variants + `voom-events::kind_test.rs` / `payload_test.rs` round-trip coverage.

**`voom-store::repo::commit_safety_gate`**
- `phase_a_gate_abort_with_event(...)` helper encoding the two-tx pattern (sequencing doc §5.2): named to reflect narrow scope so the pattern cannot accidentally leak into Phases B/C.
- `DestructiveCommit` input shape in commit 4 does **not** carry an `override_token` field. The force path lands in commit 10 (see §3 decision row) which retrofits the signature.
- `prepare_destructive_commit` implementation:
  1. Closure walk: target → `FileVersion`(s) → live `FileLocation`s + `AliasResolver.aliases_for_version` + owning `AssetBundle`(s). On `Unreachable`, abort unconditionally with `BlockedByClosureIncomplete` + `commit.aborted_by_closure_incomplete` (two-tx pattern). No bypass branch exists in this commit; commit 10 adds the `closure_incomplete` bypass.
  2. Blocking-lease check via UNION over four `scope_*_id` columns against the closure.
  3. Accepted-evidence revalidation: compare pinned `FileVersion` IDs, hashes, locations against current state.
  4. Insert `commit_intents` row (`state = 'pending'`, `target`, `closure_initial`, `accepted_evidence_ids`, `override_token = NULL`, `started_at = now`) + expand `commit_intent_scope_members` across all four granularities. The `override_token` column is reserved (commit 10 starts populating it).
  5. Emit `commit.intent_recorded`. COMMIT.

**Sibling tests** cover each Phase A `Blocked*` exit (use lease, stale evidence, closure incomplete) and the success path. Each test asserts both the durable row state (or absence) and the matching event row in the same call.

**Integration test** `commit_safety_gate.rs` (new file in `crates/voom-store/tests/`): parametrized over Phase A outcomes — each `Blocked*` variant fires through prepare; success leaves a `pending` intent + `commit_intent_scope_members` rows + the `commit.intent_recorded` event. Disk-mode parity via the M1 harness.

**Exit:** `just ci` green; every Phase A `CommitGateResult` variant triggered in the integration test; two-tx pattern lives only inside `phase_a_gate_abort_with_event` (grep-checked).

### Commit 5 — Pending-commit lock retrofit (sub-slice 5)

**`voom-store::repo::commit_safety_gate`**
- `pub(crate) async fn consult_pending_commit_lock_in_tx(tx, scope: LeaseScope) -> Result<Option<(CommitId, LeaseScope)>, VoomError>` helper. Returns the `(commit_id, offending_scope)` pair so callers can construct `CommitGateResult::BlockedByPendingCommit { commit_id, offending_scope }` directly. Single source of truth per sequencing doc §5.1.

**Wire it into the two locked entry points:**
- `UseLeaseRepo::acquire_in_tx` — replace the TODO at `crates/voom-store/src/repo/use_leases.rs:649`. The scope translated from the caller's `LeaseScope` enum is the single column to consult.
- `IdentityRepo::record_discovered_file_in_tx::AliasAttached` branch at `crates/voom-store/src/repo/identity.rs:745`. Consult with `LeaseScope::Version(file_version_id)` before persisting the new alias `FileLocation`. (No TODO marker exists today; the lock simply wasn't required until M3 Phase 2.)

**Architectural exemption (deliberate non-call):**
- `IdentityRepo::reconcile_rename_in_tx` does **not** consult the lock (arch spec lines 697–708; sprint spec §8.7, §9.2). Commit 5 adds a fixture asserting a rename succeeds against an in-flight commit on the affected `FileVersion`.

**Extend existing M2 sibling tests** for both retrofitted call sites: assert rejection (`BlockedByPendingCommit`) when an in-flight intent covers the scope; assert unchanged behavior under no in-flight commits.

**Integration test** `commit_safety_gate_pending_lock.rs` (new): blocking + advisory leases both rejected for `UseLeaseRepo::acquire`; alias-attach rejected for `IdentityRepo::record_discovered_file_in_tx`; rename proceeds against the same in-flight commit. Disk-mode parity.

**Exit:** `just ci` green; both lock retrofits exercised; rename exemption explicitly tested; M2 sibling tests for `record_discovered_file_in_tx` and `reconcile_rename_in_tx` extended (not rewritten) and still pass.

### Commit 6 — `authorize_destructive_commit` (Phase B) (sub-slice 6)

**`voom-events`**
- `commit.authorized`, `commit.aborted_by_closure_grew` event kinds + payload structs + round-trip coverage.

**`voom-store::repo::commit_safety_gate`**
- `authorize_destructive_commit` implementation. One IMMEDIATE tx:
  1. Read `commit_intents` row: require `state = 'pending'`; carry `state`, `closure_initial`, `accepted_evidence_ids`, `epoch` back in one round-trip. (`override_token` is reserved for commit 10; this commit does not read it.)
  2. Recompute `closure_authorized` against current DB + `AliasResolver`. On `Unreachable`, abort unconditionally with `BlockedByClosureIncomplete` + `commit.aborted_by_closure_incomplete`. No bypass branch exists in this commit; commit 10 adds the `closure_incomplete` bypass to both prepare and authorize.
  3. Compute delta vs `closure_initial` across all four granularities — any non-empty delta → `BlockedByClosureGrew`, transition to `aborted` with `abort_reason = 'closure_grew'`, emit `commit.aborted_by_closure_grew`. COMMIT. Return.
  4. Re-evaluate blocking-lease check against `closure_authorized`. Match → `BlockedByUseLease`, transition with `abort_reason = 'fresh_lease'`, emit `commit.aborted_by_use_lease` (with `payload.phase = 'authorize'`). COMMIT. Return.
  5. Re-validate accepted evidence → `BlockedByStaleEvidence` if drift.
  6. **Snapshot per-member epochs into the DB.** Inside the same tx, run four parallel `SELECT id, epoch FROM <table> WHERE id IN (...)` reads keyed off the granularity-specific FK lists in `closure_authorized` (`file_locations`, `file_versions`, `asset_bundles`, `file_assets`). Encode the result as a JSON array of `[kind, row_id, epoch]` triples and write it to `commit_intents.target_row_epochs` atomically with the `state = 'authorized'` UPDATE in the next step. The permit returned to the caller does NOT carry these epochs.
  7. Reconcile `commit_intent_scope_members` with `closure_authorized` (delete removed, insert added rows). Update intent row to `state = 'authorized'`, set `closure_authorized`, `target_row_epochs`, `authorized_at = now`, bump `epoch`. Emit `commit.authorized`. COMMIT. Return opaque `CommitPermit` (carries `commit_id` plus the authorized closure / evaluated leases / revalidated evidence for caller introspection).
- Phase B aborts commit in-tx (no two-tx pattern; sequencing doc §5.2).
- The triples persisted in `commit_intents.target_row_epochs` capture the per-row epoch of every member of `closure_authorized` at the moment authorize commits. Phase C re-reads them from the DB by `commit_id` and uses them as `expected_epoch` arguments to each `IdentityRepo` destructive mutation; any drift between Phase B and Phase C is caught by the M2 epoch guard already present on those rows. Holding the snapshot in the DB rather than on the caller's `CommitPermit` keeps the trip-wire authoritative and makes the permit reconstructible after a process crash between authorize and finalize.

**Sibling tests** cover each Phase B outcome and assert `target_row_epochs` is populated for every member of `closure_authorized` on the success path.

**Integration test** `commit_safety_gate_after_rename.rs` (new): the §9.4 e2e. Sequence: prepare against a `FileVersion` with one location → external rename lands (records via `reconcile_rename_in_tx`, exempt from the lock) → authorize observes non-empty `removed_locations` (prior) and `added_locations` (new) → `BlockedByClosureGrew`. Re-asserts the existing re-anchoring still fires for any leases on the retired location.

**Exit:** `just ci` green; every Phase B `CommitGateResult` variant exercised; rename × authorize e2e green.

### Commit 7 — `finalize_destructive_commit` (Phase C) (sub-slice 7)

**`voom-events`**
- `commit.completed`, `commit.aborted_post_mutation`, `commit.aborted_pre_mutation` (carries `prior_state` ∈ `{'pending', 'authorized'}`), `commit.recovery_required` event kinds + payloads.
- `commit.aborted_post_mutation` payload follows the unified schema from sprint spec §9.3.2 Phase C step 3: carries `reason` ∈ `{'closure_grew', 'fresh_lease', 'closure_grew_and_fresh_lease', 'stale_target_epoch'}`, both `added_*`/`removed_*` arrays, `fresh_lease_ids`, and `target_epoch_drift` (a list of `(kind, id, expected, observed)` triples, present only when `reason = 'stale_target_epoch'` or carries both drift kinds in combination).

**`voom-store::repo::commit_safety_gate`**
- `finalize_destructive_commit` implementation. One IMMEDIATE tx:
  1. Read `commit_intents` row: require `state = 'authorized'` and `epoch == permit.epoch()`. Wrong state or epoch → `Conflict` without writing.
  2. `MutationOutcome::NotPerformed` branch → transition to `aborted`, `abort_reason = 'operator_cancel'`, emit `commit.aborted_pre_mutation` with `prior_state = 'authorized'`. Return `Ok(CommitGateOutcome { result: CancelledAfterAuthorize, .. })` (the `closure_final` carries the authorized closure unchanged — no recheck).
  3. **Source `expected_epoch` from the DB.** Inside the same tx, read `target_row_epochs` JSON from `commit_intents` by `permit.commit_id()` and decode the `[kind, row_id, epoch]` triples. These are the `expected_epoch` arguments to each `IdentityRepo` destructive mutation later in the step. A NULL or unparseable `target_row_epochs` for a row in `state = 'authorized'` is an invariant violation (migration 0005's CHECK prevents it; surface as `VoomError::Internal`).
  4. `Applied { observed }` branch — defensive trip-wire: recompute `closure_final`, re-evaluate leases, **and** compare every member's current `epoch` against the snapshot decoded in step 3. Four sub-branches per sprint spec §9.3.2 Phase C step 3 plus the per-row epoch guard added under §3. Each `recovery_required` transition writes the reason to `commit_intents.recovery_reason` (NOT `abort_reason`); migration 0005's CHECK enforces this split.
     - Closure grew/shifted (delta non-empty vs `closure_authorized`, no fresh lease, no epoch drift) → `recovery_required` with `recovery_reason = 'closure_grew'`, emit `commit.aborted_post_mutation` with `reason='closure_grew'`. Return `BlockedByClosureGrew`.
     - Fresh blocking lease (delta empty, fresh lease overlaps, no epoch drift) → `recovery_required` with `recovery_reason = 'fresh_lease'`, emit with `reason='fresh_lease'`. Return `BlockedByUseLease`.
     - Closure grew and fresh lease both fire (no epoch drift) → `recovery_required` with `recovery_reason = 'closure_grew_and_fresh_lease'`, emit one event with `reason='closure_grew_and_fresh_lease'` and both populated arrays. Return `BlockedByClosureGrew`.
     - **Stale target epoch** (any member's current `epoch` differs from the snapshot value, regardless of whether the other two trip-wires also fire) → `recovery_required` with `recovery_reason = 'stale_target_epoch'`, emit `commit.aborted_post_mutation` with `reason='stale_target_epoch'` and a `target_epoch_drift` payload field listing the drifted `(kind, id, expected, observed)` triples. Do **not** apply the durable mutation. Return `BlockedByStaleTargetEpoch { drift }` (variant defined in commit 1).
  5. Silent dispatch trip-wire → look up each target member's snapshotted epoch (decoded in step 3) and pass it as `expected_epoch` to the matching `IdentityRepo` mutation:
     - `DeleteFileLocation` → `retire_file_location_in_tx(tx, location_id, now, expected_epoch)`.
     - `DeleteFileVersion` → `retire_file_version_in_tx(tx, version_id, now, expected_epoch)`.
     - `ReplaceFileLocation` / `MoveFileLocation` → `replace_file_location_in_tx(tx, retired_id, retired_expected_epoch, new_proposal, now)`.
     (`ArchiveFileVersion` dispatch removed — variant does not exist in Sprint 1.)
  6. Update row to `completed`, `finalized_at = now`, bump `epoch`. Emit `commit.completed` with `closure_final` carrying the just-recomputed silent-path closure. COMMIT. Return `Allowed`.

**Sibling tests** for the state/epoch check, `NotPerformed`, each of the four trip-wire sub-branches (including a dedicated test that bumps a target member's `epoch` between authorize and finalize to drive the `stale_target_epoch` path), and the silent path × each `CommitTarget` variant (each sourcing `expected_epoch` from the DB snapshot read in step 3). The trip-wire sibling tests also assert that the `recovery_required` row carries the reason in `recovery_reason` and that `abort_reason IS NULL`.

**Integration test** `commit_safety_gate_recovery_required.rs` (new): all four trip-wire sub-branches end in `recovery_required` with the matching event payload. The `stale_target_epoch` case bumps a member's `epoch` via a direct UPDATE between authorize and finalize. Disk-mode parity.

**Exit:** `just ci` green; every Phase C `CommitGateResult` variant exercised; defensive trip-wire payloads carry the unified schema.

### Commit 8 — `abort_destructive_commit` (pending-only) (sub-slice 8)

**`voom-store::repo::commit_safety_gate`**
- `abort_destructive_commit` implementation. One IMMEDIATE tx:
  1. Read `commit_intents` row: require `state = 'pending'`. Missing, `authorized`, or any terminal state → `Conflict`.
  2. Update to `state = 'aborted'`, `aborted_at = now`, `abort_reason = reason`.
  3. Emit `commit.aborted_pre_mutation` with `prior_state = 'pending'`. COMMIT.

**Sibling tests** cover each `state` precondition (pending succeeds, authorized rejects with `Conflict`, terminal states reject with `Conflict`, missing row rejects with `Conflict`).

**Recovery-contract integration test** (added to `commit_safety_gate.rs` or new file): insert an `authorized` intent row + the matching `commit.authorized` event; assert `abort_destructive_commit` returns `Conflict`. Encodes the architectural invariant that the only sanctioned post-authorize termination is `finalize(_, NotPerformed)`.

**Exit:** `just ci` green; abort entry point sibling- and integration-tested; recovery-contract Conflict assertion in place.

### Commit 9 — `list_pending_commit_intents` (sub-slice 9)

**`voom-store::repo::commit_safety_gate`**
- `list_pending_commit_intents(pool, older_than: Option<OffsetDateTime>) -> Result<Vec<PendingCommitIntent>, VoomError>`. Reads rows in `state IN ('pending', 'authorized')`, optional `started_at < older_than` filter; uses the `commit_intents_in_flight` partial index.
- Each `PendingCommitIntent` carries `state`, `closure_initial`, `closure_authorized: Option<_>` (Some when `state = 'authorized'`), `accepted_evidence_ids`, `started_at`, `authorized_at`.

**Sibling tests** cover: empty result; only `pending` rows; mix of `pending` + `authorized`; terminal states excluded; `older_than` cutoff.

**Exit:** `just ci` green; read-only list path exercised.

### Commit 10 — Force path + retrofit (sub-slice 11)

This is the single landing point for all force-path plumbing. The `ForcePathToken` stub has lived in the module since commit 1 for compile-stability, but no upstream caller threads it through prepare/authorize. Commit 10 retrofits both signatures, ships the serde + validation + emission, and wires the `closure_incomplete` bypass branches in both Phase A and Phase B atomically.

**`voom-events`**
- `commit.forced_override` event kind + payload (`actor`, `reason`, `bypass`) + round-trip coverage.

**`voom-store::repo::commit_safety_gate`**
- Signature retrofit: `prepare_destructive_commit` and `authorize_destructive_commit` both grow an `Option<ForcePathToken>` parameter. `DestructiveCommit` gains an `override_token: Option<ForcePathToken>` field. Authorize re-reads the token from `commit_intents.override_token` (a serialized blob populated by prepare in this same commit).
- `ForcePathToken` JSON serde — canonical impl used both to write the `commit_intents.override_token` column and to round-trip on read.
- `ForcePathToken::validate_bypass(...)` — rejects any non-`ClosureIncomplete` bit with `VoomError::Config("force-path bypass not supported: <name>")`. Called by `prepare_destructive_commit` before the gate runs (validation precedes the closure walk; an invalid token never writes a row).
- `closure_incomplete` bypass branches added to both Phase A and Phase B: if the closure walk surfaces an `Unreachable` and the token has `BypassKind::ClosureIncomplete`, the abort path is skipped and the walk falls through with the partial closure.
- `prepare_destructive_commit` emits `commit.forced_override` when `override_token.is_some()`, after validation and before the closure walk. The column blob is populated atomically with the `commit.intent_recorded` insert.

**Integration test** `commit_safety_gate_force_path.rs` (new): valid token honored through to authorize (`closure_incomplete` bypass exercised against `FailingAliasResolver` in both phases); invalid bypass bits surface the `Config` error envelope without a `commit_intents` row materializing. The test also asserts that pre-commit-10 callers (which pass no token) continue to abort on `Unreachable`, encoding the property that **bypass logic and audit event ship together atomically** — no in-tree caller has access to a bypass branch without the corresponding `commit.forced_override` audit trail.

**Exit:** `just ci` green; force path end-to-end exercised in both phases; invalid bypass bits rejected before any state change; pre-retrofit code paths still abort unconditionally on `Unreachable`.

## 5. Phase 2 exit gate (matches sequencing doc §3)

After commit 10:

- Every `CommitGateResult` variant triggered in at least one integration test in the correct phase.
- Pending-commit lock asserted to block `UseLeaseRepo::acquire` and the `AliasAttached` branch; asserted to **not** block `reconcile_rename_in_tx`.
- Stale target epoch at finalize is rejected with `recovery_required` + `commit.aborted_post_mutation { reason='stale_target_epoch' }` — covered by a sibling test that bumps a member's `epoch` between authorize and finalize.
- `ArchiveFileVersion` is not implemented; the `CommitTarget` enum does not carry the variant. Deferred to Sprint 5+ per parent-spec precedent for `ArchiveBundle`/`DeleteBundle` (see §9 follow-up).
- M2 sibling tests for `record_discovered_file_in_tx` and `reconcile_rename_in_tx` extended (not rewritten) and still pass.
- Two-tx pattern is used only for Phase A gate-check aborts (grep-asserted).
- `just ci` green.

## 6. Cross-cutting deltas per commit (consolidated)

| Commit | `voom-core` | `voom-events` | `voom-store` | Tests |
|---|---|---|---|---|
| 1 | `CommitId`; 5 `ErrorCode` + `VoomError` variants; `FailureClass` remap | — | `commit_safety_gate` module + type stubs incl. opaque `CommitPermit` (accessors only) and `CommitGateResult::BlockedByStaleTargetEpoch`. **Migration 0005** (commit-intent persistent permit + `recovery_reason` column) folded into this commit's round-4 fix; see §3. | sibling smoke + extended `failure_test.rs` |
| 2 | — | — | `AliasResolver` trait + `SqliteAliasResolver` + `FailingAliasResolver` | sibling for both resolvers |
| 3 | — | — | 3 new `IdentityRepo` `_in_tx` mutations, all guarded by `expected_epoch` | sibling for each (incl. dedicated `expected_epoch` mismatch row) |
| 4 | — | `SubjectType::CommitIntent`; `commit.intent_recorded`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete` | `prepare_destructive_commit` + `phase_a_gate_abort_with_event` | sibling + integration `commit_safety_gate.rs` (Phase A) |
| 5 | — | — | `consult_pending_commit_lock_in_tx` + 2 callers retrofitted | extended M2 sibling + integration `commit_safety_gate_pending_lock.rs` |
| 6 | — | `commit.authorized`, `.aborted_by_closure_grew` | `authorize_destructive_commit` (Phase B persists `target_row_epochs` JSON into `commit_intents` atomically with `state = 'authorized'`) | sibling + integration `commit_safety_gate_after_rename.rs` |
| 7 | — | `commit.completed`, `.aborted_post_mutation` (now incl. `reason='stale_target_epoch'` + `target_epoch_drift` payload field), `.aborted_pre_mutation`, `.recovery_required` | `finalize_destructive_commit` (re-reads `target_row_epochs` from `commit_intents` by `commit_id`; recovery-required writes go to `recovery_reason` column) | sibling + integration `commit_safety_gate_recovery_required.rs` |
| 8 | — | — | `abort_destructive_commit` | sibling + recovery-contract integration |
| 9 | — | — | `list_pending_commit_intents` | sibling |
| 10 | — | `commit.forced_override` | `prepare_destructive_commit` + `authorize_destructive_commit` signature retrofit; `ForcePathToken` serde + validation + emission; `closure_incomplete` bypass branches in both phases | integration `commit_safety_gate_force_path.rs` |

## 7. Touch-back into M1/M2 code

Restated from sequencing doc §4.3 for this sub-plan:

- **Commit 1** rewires `FailureClass::into_error_code` mappings for three classes (`StaleIdentityEvidence`, `ClosureResolutionIncomplete`, `BlockedByActiveUseLease`) currently routing to `ApprovalRequired`. M1 sibling tests asserting the old mapping are updated in the same commit (no separate cleanup PR).
- **Commit 5** modifies two M2 code paths: `IdentityRepo::record_discovered_file_in_tx::AliasAttached` branch gets the lock retrofit; `IdentityRepo::reconcile_rename_in_tx` deliberately does not (architectural exemption asserted via test). Existing M2 sibling tests are extended.

No M1 or M2 code is otherwise touched.

## 8. Out of scope (re-stated from sequencing doc §7)

- No filesystem-aware recovery worker — Sprint 5+.
- No `ArchiveBundle` / `DeleteBundle` `CommitTarget` variants — Sprint 5.
- No CLI write commands — Sprint 1 CLI is read-only; the CLI inspection surface for `commit-intent` lands in Phase 4.
- No `voom-api` deliverables — Sprint 1 has no API server.
- No worker process integration — Sprint 2+.

## 9. Open follow-ups

None blocking. Three items to remember at PR time:

- Adversarial review round before opening the PR (per project convention; sprint-spec iteration memory).
- PR description names commits 1–10 individually and points at the parent spec sections each commit satisfies.
- **Parent-spec inconsistency to surface upstream.** Parent §9.3.1 lists the Sprint 1 `CommitTarget` variants as `DeleteFileLocation`, `DeleteFileVersion`, `ReplaceFileLocation`, `MoveFileLocation`, **and** `ArchiveFileVersion` — while the same paragraph defers `ArchiveBundle` and `DeleteBundle` to Sprint 5 with the explicit rationale "the schema does not carry the soft-delete/archive columns those targets need." The `file_versions` schema (migration 0003) has no `archived_at` column either, so the same rationale applies to `ArchiveFileVersion` but was not invoked. This plan resolves the inconsistency by omitting `ArchiveFileVersion`; the parent spec should either remove it from §9.3.1's enum list or add the corresponding `archived_at` schema column with explicit recoverable semantics. Tag for the next sprint-1 spec adversarial round.
- **Optional cleanup (non-blocking):** `NewFileLocation` derives `PartialEq, Eq` (added in commit 1 for the previous `FileLocationProposal = NewFileLocation` alias). With the dedicated `FileLocationProposal` struct in place those derives are no longer strictly required, but they're additive and harmless. Revisit only if a sprint-cleanup commit touches `identity.rs` for another reason.
- **Sprint design spec drift to flag upstream.** `docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md` §9.3.2 still defines `CommitPermit` with all-pub fields and no `target_row_epochs` column on `commit_intents`. The patched plan now diverges in three places: opaque permit, durable `target_row_epochs` JSON column (migration 0005), dedicated `recovery_reason` column. Tag for the next sprint-1 spec adversarial round.
