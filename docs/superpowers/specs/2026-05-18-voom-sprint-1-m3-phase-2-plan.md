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
| `ForcePathToken` plumbing | Stub-then-fill | Define the struct in commit 1; plumb `Option<ForcePathToken>` through prepare/authorize from commit 4. Commit 10 then adds parsing, bypass-set validation, and the `commit.forced_override` event. No retrofit of signatures. |
| Branch scope | One branch, ~10 commits | Matches PR #21's pattern of carrying all of Phase 1 on a single branch. One adversarial-review round at end. |
| `FailureClass::into_error_code` remap | Rides commit 1 | `StaleIdentityEvidence` / `ClosureResolutionIncomplete` / `BlockedByActiveUseLease` currently route to `ApprovalRequired` as a placeholder. The proper `ErrorCode` variants land in commit 1, so the mapping switches in the same commit. Small M1 cleanup riding inside a Phase 2 commit, approved as part of branch scope. |
| `CommitTarget::MoveFileLocation` vs `ReplaceFileLocation` | Same `IdentityRepo` mutation | Parent §9.3.2 Phase C step 4 dispatches both to `replace_file_location_in_tx`. The gate distinguishes them only for `target` payload audit. |

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
  - `CommitTarget`, `AffectedScopeClosure`, `DestructiveCommit`, `CommitIntent`, `CommitPermit`, `CommitGateOutcome`, `CommitGateResult`, `CommitIntentState`, `MutationOutcome`, `AbortReason`.
  - `ForcePathToken { actor: String, reason: String, bypass: BTreeSet<BypassKind> }`, with `BypassKind` enum carrying just `ClosureIncomplete` for Sprint 1.
  - `BlockedByPendingCommitDetail`, `BlockedByClosureGrewDetail`, `ClosureWarning`, `ClosureFailure`, `EvidenceDrift`, `EvidenceRevalidationResult`, `PendingCommitIntent`.

**Sibling test** `commit_safety_gate_test.rs`: constructor + Debug round-trip smoke for the public types.

**Exit:** `just ci` green; M1 sibling tests still pass against the rewired `FailureClass` mapping; the new types are reachable from `voom-store` consumers but no algorithm uses them yet.

### Commit 2 — `AliasResolver` trait + impls (sub-slice 2)

- Add `AliasResolver` trait + `AliasResolutionError` enum to `commit_safety_gate`.
- Add `SqliteAliasResolver` impl: returns every live `FileLocation` row on the supplied `FileVersionId`.
- Add `FailingAliasResolver` to `voom-store::test_support` under the existing test-support feature gate: takes a configured set of `FileVersionId`s and returns `AliasResolutionError::Unreachable` for them.

**Sibling tests** cover `SqliteAliasResolver` returns the live set; `FailingAliasResolver` returns `Unreachable` for configured IDs and the empty set otherwise.

**Exit:** `just ci` green; both resolvers exercised in unit tests; integration tests still empty (need Phase A/B to consume them).

### Commit 3 — `IdentityRepo` destructive `_in_tx` mutations (sub-slice 3)

Four new methods on `IdentityRepo` trait + `SqliteIdentityRepo` impl. Pure additions — no callers yet:

- `retire_file_location_in_tx(tx, location_id, now)` — sets `retired_at = now`, bumps `epoch`. Returns `VoomError::Conflict` on a row already terminal (matching M2 soft-delete semantics; not idempotent).
- `retire_file_version_in_tx(tx, version_id, now)` — sets `retired_at = now` on the version row, bumps `epoch`. Live `FileLocation` rows under the version remain (Phase C's caller decides whether to cascade-retire via `replace_file_location_in_tx`).
- `archive_file_version_in_tx(tx, version_id, now)` — sets `archived_at = now`, bumps `epoch`. Distinct from retire (archive is recoverable; retire is terminal).
- `replace_file_location_in_tx(tx, retired, new_proposal, now)` — atomically retires `retired` and inserts a new `FileLocation` on the same `FileVersion`.

**Sibling tests** in `identity_test.rs`: each method asserts the column shape, epoch bump, and the Conflict path on a row already in the target state.

**Exit:** `just ci` green; mutations sibling-tested; no integration test (no caller yet).

### Commit 4 — `prepare_destructive_commit` (Phase A) (sub-slice 4)

**`voom-events`**
- Add `SubjectType::CommitIntent`.
- Add `EventKind` variants: `commit.intent_recorded`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete`.
- Add `Event` payload variants + `voom-events::kind_test.rs` / `payload_test.rs` round-trip coverage.

**`voom-store::repo::commit_safety_gate`**
- `phase_a_gate_abort_with_event(...)` helper encoding the two-tx pattern (sequencing doc §5.2): named to reflect narrow scope so the pattern cannot accidentally leak into Phases B/C.
- `prepare_destructive_commit` implementation:
  1. Closure walk: target → `FileVersion`(s) → live `FileLocation`s + `AliasResolver.aliases_for_version` + owning `AssetBundle`(s).
  2. Blocking-lease check via UNION over four `scope_*_id` columns against the closure.
  3. Accepted-evidence revalidation: compare pinned `FileVersion` IDs, hashes, locations against current state.
  4. Insert `commit_intents` row (`state = 'pending'`, `target`, `closure_initial`, `accepted_evidence_ids`, `override_token = <serialized | NULL>`, `started_at = now`) + expand `commit_intent_scope_members` across all four granularities.
  5. Emit `commit.intent_recorded`. COMMIT.
- `ForcePathToken` plumbed as `Option<_>` through `DestructiveCommit`. The `closure_incomplete` bypass branch is honored here: if the closure walk surfaces an `Unreachable` and `input.override_token.as_ref().is_some_and(|t| t.bypass.contains(&BypassKind::ClosureIncomplete))`, the abort is skipped. Sibling tests construct `ForcePathToken { actor, reason, bypass: {ClosureIncomplete} }` directly to cover this branch. Commit 10 adds upstream validation (rejecting other bypass bits before the gate runs) and the `commit.forced_override` event — no change to the prepare-time bypass logic itself.

**Sibling tests** cover each Phase A `Blocked*` exit (use lease, stale evidence, closure incomplete) and the success path. Each test asserts both the durable row state (or absence) and the matching event row in the same call.

**Integration test** `commit_safety_gate.rs` (new file in `crates/voom-store/tests/`): parametrized over Phase A outcomes — each `Blocked*` variant fires through prepare; success leaves a `pending` intent + `commit_intent_scope_members` rows + the `commit.intent_recorded` event. Disk-mode parity via the M1 harness.

**Exit:** `just ci` green; every Phase A `CommitGateResult` variant triggered in the integration test; two-tx pattern lives only inside `phase_a_gate_abort_with_event` (grep-checked).

### Commit 5 — Pending-commit lock retrofit (sub-slice 5)

**`voom-store::repo::commit_safety_gate`**
- `pub(crate) async fn consult_pending_commit_lock_in_tx(tx, scope: LeaseScope) -> Result<Option<BlockedByPendingCommitDetail>, VoomError>` helper. Single source of truth per sequencing doc §5.1.

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
  1. Read `commit_intents` row: require `state = 'pending'`; carry `state`, `closure_initial`, `accepted_evidence_ids`, `epoch`, **and** `override_token` back in one round-trip.
  2. Recompute `closure_authorized` against current DB + `AliasResolver`. Honor `override_token`'s `closure_incomplete` bypass.
  3. Compute delta vs `closure_initial` across all four granularities — any non-empty delta → `BlockedByClosureGrew`, transition to `aborted` with `abort_reason = 'closure_grew'`, emit `commit.aborted_by_closure_grew`. COMMIT. Return.
  4. Re-evaluate blocking-lease check against `closure_authorized`. Match → `BlockedByUseLease`, transition with `abort_reason = 'fresh_lease'`, emit `commit.aborted_by_use_lease` (with `payload.phase = 'authorize'`). COMMIT. Return.
  5. Re-validate accepted evidence → `BlockedByStaleEvidence` if drift.
  6. Reconcile `commit_intent_scope_members` with `closure_authorized` (delete removed, insert added rows). Update intent row to `state = 'authorized'`, set `closure_authorized`, `authorized_at = now`, bump `epoch`. Emit `commit.authorized`. COMMIT. Return `CommitPermit`.
- Phase B aborts commit in-tx (no two-tx pattern; sequencing doc §5.2).

**Sibling tests** cover each Phase B outcome including the `override_token` honor path.

**Integration test** `commit_safety_gate_after_rename.rs` (new): the §9.4 e2e. Sequence: prepare against a `FileVersion` with one location → external rename lands (records via `reconcile_rename_in_tx`, exempt from the lock) → authorize observes non-empty `removed_locations` (prior) and `added_locations` (new) → `BlockedByClosureGrew`. Re-asserts the existing re-anchoring still fires for any leases on the retired location.

**Exit:** `just ci` green; every Phase B `CommitGateResult` variant exercised; rename × authorize e2e green.

### Commit 7 — `finalize_destructive_commit` (Phase C) (sub-slice 7)

**`voom-events`**
- `commit.completed`, `commit.aborted_post_mutation`, `commit.aborted_pre_mutation` (carries `prior_state` ∈ `{'pending', 'authorized'}`), `commit.recovery_required` event kinds + payloads.
- `commit.aborted_post_mutation` payload follows the unified schema from sprint spec §9.3.2 Phase C step 3: carries `reason` ∈ `{'closure_grew', 'fresh_lease', 'closure_grew_and_fresh_lease'}`, both `added_*`/`removed_*` arrays, and `fresh_lease_ids`.

**`voom-store::repo::commit_safety_gate`**
- `finalize_destructive_commit` implementation. One IMMEDIATE tx:
  1. Read `commit_intents` row: require `state = 'authorized'` and `epoch == permit.epoch`. Wrong state or epoch → `Conflict` without writing.
  2. `MutationOutcome::NotPerformed` branch → transition to `aborted`, `abort_reason = 'operator_cancel'`, emit `commit.aborted_pre_mutation` with `prior_state = 'authorized'`. Return `Ok(CommitGateOutcome { result: CancelledAfterAuthorize, .. })` (the `closure_final` carries the authorized closure unchanged — no recheck).
  3. `Applied { observed }` branch — defensive trip-wire: recompute `closure_final` + re-evaluate leases. Three sub-branches per sprint spec §9.3.2 Phase C step 3:
     - Closure grew/shifted (delta non-empty vs `closure_authorized`, no fresh lease) → `recovery_required`, emit `commit.aborted_post_mutation` with `reason='closure_grew'`. Return `BlockedByClosureGrew`.
     - Fresh blocking lease (delta empty, fresh lease overlaps) → `recovery_required`, emit with `reason='fresh_lease'`. Return `BlockedByUseLease`.
     - Both fire → emit one event with `reason='closure_grew_and_fresh_lease'` and both populated arrays. Return `BlockedByClosureGrew`.
  4. Silent trip-wire → dispatch by `CommitTarget`:
     - `DeleteFileLocation` → `retire_file_location_in_tx`.
     - `DeleteFileVersion` → `retire_file_version_in_tx`.
     - `ArchiveFileVersion` → `archive_file_version_in_tx`.
     - `ReplaceFileLocation` / `MoveFileLocation` → `replace_file_location_in_tx`.
  5. Update row to `completed`, `finalized_at = now`, bump `epoch`. Emit `commit.completed` with `closure_final` carrying the just-recomputed silent-path closure. COMMIT. Return `Allowed`.

**Sibling tests** for the state/epoch check, `NotPerformed`, each trip-wire sub-branch, and the silent path × each `CommitTarget` variant.

**Integration test** `commit_safety_gate_recovery_required.rs` (new): all three trip-wire sub-branches end in `recovery_required` with the matching event payload. Disk-mode parity.

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

### Commit 10 — Force path (sub-slice 11)

**`voom-events`**
- `commit.forced_override` event kind + payload (`actor`, `reason`, `bypass`) + round-trip coverage.

**`voom-store::repo::commit_safety_gate`**
- `ForcePathToken` JSON serde (the column has been populated as a serialized blob since commit 4; this commit ships the canonical serde impl).
- `ForcePathToken::validate_bypass(...)` — rejects any non-`ClosureIncomplete` bit with `VoomError::Config("force-path bypass not supported: <name>")`. Called by `prepare_destructive_commit` before the gate runs (validation precedes the closure walk; an invalid token never writes a row).
- `prepare_destructive_commit` emits `commit.forced_override` when `override_token.is_some()`, after validation and before the closure walk.

**Integration test** `commit_safety_gate_force_path.rs` (new): valid token honored through to authorize (`closure_incomplete` bypass exercised against `FailingAliasResolver`); invalid bypass bits surface the `Config` error envelope without a `commit_intents` row materializing.

**Exit:** `just ci` green; force path end-to-end exercised; invalid bypass bits rejected before any state change.

## 5. Phase 2 exit gate (matches sequencing doc §3)

After commit 10:

- Every `CommitGateResult` variant triggered in at least one integration test in the correct phase.
- Pending-commit lock asserted to block `UseLeaseRepo::acquire` and the `AliasAttached` branch; asserted to **not** block `reconcile_rename_in_tx`.
- Force path rejects non-`closure_incomplete` bypass bits with `Config`.
- M2 sibling tests for `record_discovered_file_in_tx` and `reconcile_rename_in_tx` extended (not rewritten) and still pass.
- Two-tx pattern is used only for Phase A gate-check aborts (grep-asserted).
- `just ci` green.

## 6. Cross-cutting deltas per commit (consolidated)

| Commit | `voom-core` | `voom-events` | `voom-store` | Tests |
|---|---|---|---|---|
| 1 | `CommitId`; 5 `ErrorCode` + `VoomError` variants; `FailureClass` remap | — | `commit_safety_gate` module + type stubs | sibling smoke + extended `failure_test.rs` |
| 2 | — | — | `AliasResolver` trait + `SqliteAliasResolver` + `FailingAliasResolver` | sibling for both resolvers |
| 3 | — | — | 4 new `IdentityRepo` `_in_tx` mutations | sibling for each |
| 4 | — | `SubjectType::CommitIntent`; `commit.intent_recorded`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete` | `prepare_destructive_commit` + `phase_a_gate_abort_with_event` | sibling + integration `commit_safety_gate.rs` (Phase A) |
| 5 | — | — | `consult_pending_commit_lock_in_tx` + 2 callers retrofitted | extended M2 sibling + integration `commit_safety_gate_pending_lock.rs` |
| 6 | — | `commit.authorized`, `.aborted_by_closure_grew` | `authorize_destructive_commit` | sibling + integration `commit_safety_gate_after_rename.rs` |
| 7 | — | `commit.completed`, `.aborted_post_mutation`, `.aborted_pre_mutation`, `.recovery_required` | `finalize_destructive_commit` | sibling + integration `commit_safety_gate_recovery_required.rs` |
| 8 | — | — | `abort_destructive_commit` | sibling + recovery-contract integration |
| 9 | — | — | `list_pending_commit_intents` | sibling |
| 10 | — | `commit.forced_override` | `ForcePathToken` serde + validation + emission | integration `commit_safety_gate_force_path.rs` |

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

None blocking. Two items to remember at PR time:

- Adversarial review round before opening the PR (per project convention; sprint-spec iteration memory).
- PR description names commits 1–10 individually and points at the parent spec sections each commit satisfies.
