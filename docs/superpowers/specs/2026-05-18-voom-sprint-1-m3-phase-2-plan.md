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
| `DeleteFileVersion` `CommitTarget` variant | Deferred to Sprint 5+ | Retiring a `FileVersion` without a defined cascade leaves live `FileLocation` rows pointing at the retired version (Codex round-5 finding). The safe cascade (atomically retire every location under the version using the snapshotted epochs) needs its own design pass that this plan does not deliver. Sprint 1 has no production caller for `DeleteFileVersion` (no CLI write commands; no API destructive endpoint; rename reconciliation calls retire methods directly). Matches the existing precedent for `ArchiveFileVersion` / `ArchiveBundle` / `DeleteBundle` (all deferred because schema or cascade semantics are not ready). Revised after Codex round-5 finding #2. |
| Alias resolver transaction boundary | Trait reserved for external sources; DB enumeration via in-tx identity helper | `SqliteAliasResolver` shipped in commit 2 (`ae4be4b`) read via `fetch_all(&self.pool)`, which would either observe rows outside the gate's IMMEDIATE tx snapshot (multi-connection pool) or deadlock waiting for the connection already held by the open tx (memory pool, size=1). Round-5 fix deletes `SqliteAliasResolver` and adds `IdentityRepo::list_live_file_locations_by_version_in_tx` that the gate's closure walker calls directly inside the gate tx. The `AliasResolver` trait stays alive but is reserved for genuinely external sources (FS mounts, object stores) landing in Sprint 4/5. Revised after Codex round-5 finding #1. |
| `replace_file_location_in_tx` cross-version guard | Enforced inside identity (defense-in-depth) | The previous design pinned a "trusts caller" stance via test `replace_file_location_trusts_caller_supplied_version_id_by_design` (`edba8e4`), with the cross-version invariant living only at the gate boundary on `FileLocationProposal`. Codex round-6 finding #1: the method is a public `IdentityRepo` trait method, so the "Phase C is the sole caller" assumption is not enforceable by the type system. Pre-fetch retired row in-tx, reject mismatch with `VoomError::Conflict` before the retire UPDATE runs. The gate-boundary type-level invariant (`FileLocationProposal` has no `file_version_id`) still holds — this is the inner ring. Revised after Codex round-6 finding #1. |
| `replace_file_location_in_tx` retire+insert atomicity | SAVEPOINT (`tx.begin()` → nested) wraps the pair | Codex round-6 finding #2: previously the method ran UPDATE retire then INSERT new in the caller's outer tx. SAVEPOINT wraps the pair; ROLLBACK TO on any insert error restores the outer tx to pre-UPDATE state, so a caller that commits the outer tx after the inner failure sees the old row still live. Revised after Codex round-6 finding #2. |
| `commit_intents` post-Phase-B CHECK | Require `closure_authorized IS NOT NULL` for `authorized` / `completed` / `recovery_required` | Codex round-6 finding #3. Closes the schema-level gap at migration 0005 rather than relying on Phase B's UPDATE to populate both columns together. The `aborted` branch is deliberately left as-is because aborted-from-pending has neither column set; aborted-from-trip-wire has mixed shape depending on which wire fires. Revised after Codex round-6 finding #3. |
| Applied finalize failure recovery | SAVEPOINT around dispatch + completion + event; `recovery_required` with `recovery_reason = 'mutation_failed'` on inner Err | Codex round-7 finding #1 (critical). The `Applied` branch of `finalize_destructive_commit` previously ran identity dispatch + intent completion + event append behind a flat `?`, so any post-trip-wire DB failure rolled back the outer tx and left the row stuck in `'authorized'` — even though the caller had already performed the durable filesystem mutation. Fix mirrors the round-6 SAVEPOINT pattern: `tx.begin()` opens a nested savepoint; on inner Err the savepoint rolls back to pre-dispatch state and the outer tx transitions the intent to `recovery_required` with `recovery_reason = 'mutation_failed'`, emits `commit.aborted_post_mutation` (`reason = 'mutation_failed'`) plus `commit.recovery_required`, and commits. The caller observes a clean `Ok(FinalizeOutcome::Blocked(_))` carrying the new `CommitGateResult::BlockedByMutationFailed { error }` variant. Revised after Codex round-7 finding #1. |
| Caller-observed closure merging | Phase C unions `Applied { observed }` into recomputed `closure_final` before delta / lease checks | Codex round-7 finding #2 (high). The `Applied { observed }` payload documented that Phase C compares the caller's observed aliases against the recomputed closure, but `finalize_destructive_commit` only checked `NotPerformed` and never destructured the `Applied` payload. The merged closure is the authoritative input to `id_member_delta` and `list_blocking_leases_in_tx`; members the caller saw but the resolver/DB didn't enumerate surface as `added_*` entries on the closure-grew trip-wire and on the `commit.aborted_post_mutation` payload. `resolution_warnings` is intentionally excluded from the merge (warnings do not contribute to drift). Revised after Codex round-7 finding #2. |
| Overlapping-prepare guard | `prepare_destructive_commit` consults `consult_pending_commit_lock_in_tx` for every member of `closure_initial` before inserting the new intent | Codex round-7 finding #3 (high). Two operators preparing destructive commits on overlapping scope (same location / version / bundle / asset) used to both end up with `pending` intents because Phase A ran closure / lease / evidence checks but never consulted `commit_intents` for existing in-flight rows. Fix iterates the closure from finest to coarsest granularity (`file_locations → file_versions → bundles → file_assets`) and calls `consult_pending_commit_lock_in_tx` per member; first match aborts via the two-tx pattern with `BlockedByPendingCommit { commit_id, offending_scope }` + a dedicated `commit.aborted_by_pending_commit` event. The two-tx pattern is still confined to `phase_a_gate_abort_with_event` (the only sanctioned use). A DB-level partial unique index that would catch the race at the schema layer was considered but deferred — SQLite partial-index predicates cannot reference another table's state, so the in-tx consult is the durable enforcement point. Revised after Codex round-7 finding #3. |
| Phase C recheck failure recovery | Recovery boundary covers EVERY post-Applied failure path (snapshot decode + trip-wire recompute + silent path + trip-wire branch) | Codex round-8 finding #1 (critical). Round-7 wrapped the SAVEPOINT around the silent dispatch + completion + event append, but `run_phase_c_trip_wires_in_tx` (which invokes the closure walker, lease re-eval, and per-member epoch check) still ran via `?` before the savepoint. A Phase C closure-walker abort (converted to `VoomError::Internal` per the round-5 escape rule) propagated out of finalize, leaving the row in `'authorized'` with no `recovery_required` marker — even though the caller had already mutated the filesystem. Fix expands the SAVEPOINT to cover EVERYTHING after the `MutationOutcome::Applied` accept point. `finalize_applied_with_recovery_boundary` opens the savepoint; `finalize_applied_inner` runs decode + trip-wires + either silent path or trip-wire branch; on any inner Err the savepoint rolls back to pre-Applied-accept state and the outer tx routes through `finalize_mutation_failed_in_tx` (with empty `closure_final` — the mutation-failure path is orthogonal to the four §9.3.2 trip-wires). The round-7 inner savepoint inside `finalize_silent_path_in_tx` is removed because the outer boundary subsumes it. Revised after Codex round-8 finding #1. |
| Gate transactions use BEGIN IMMEDIATE | Single `begin_gate_tx` helper emits `BEGIN IMMEDIATE`; all four phase entry points + the two-tx helper route through it | Codex round-8 finding #2 (high). The Phase 2 spec says "one IMMEDIATE transaction" but the implementation used `pool.begin()`, which is SQLite's default deferred BEGIN. Two concurrent prepares on overlapping scope could both read "no overlap" before either inserted scope_members rows, racing through the in-tx overlapping-prepare consult (round-7 finding #3) because deferred BEGIN doesn't take RESERVED until the first write. Fix introduces `pub(crate) async fn begin_gate_tx(pool) -> Result<Transaction, VoomError>` that calls `pool.begin_with("BEGIN IMMEDIATE")` (sqlx 0.8.6 helper). RESERVED-on-BEGIN forces the second writer to either wait on `busy_timeout` (5s by default) or receive `SQLITE_BUSY`; the duplicate-pending-rows outcome becomes impossible at the lock layer rather than relying solely on the in-tx consult. Routed through every gate entry point (`prepare_destructive_commit`, `authorize_destructive_commit`, `finalize_destructive_commit`, `abort_destructive_commit`) and both legs of `phase_a_gate_abort_with_event`. A concurrent-prepare integration test pins the load-bearing property (at most one Pending across two overlapping prepares). Revised after Codex round-8 finding #2. |

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
  - `CommitTarget` — variants for Sprint 1: `DeleteFileLocation`, `ReplaceFileLocation`, `MoveFileLocation`. (`ArchiveFileVersion` is omitted; deferred to Sprint 5+ per §3 — schema has no `archived_at` column. `DeleteFileVersion` is likewise omitted; deferred to Sprint 5+ per §3 — the safe cascade over live `FileLocation` rows under a retired version is not yet defined. Revised after Codex round-5 finding #2.)
  - `AffectedScopeClosure`, `DestructiveCommit`, `CommitIntent`, `CommitPermit`, `CommitGateOutcome`, `CommitGateResult`, `CommitIntentState`, `MutationOutcome`, `AbortReason`, `TargetMemberKind`.
  - `CommitPermit` is opaque: fields are **module-private** (no `pub` qualifier); external consumers reach state via `commit_id()` / `authorized_at()` / `closure_authorized()` / `evaluated_lease_ids()` / `revalidated_evidence()` / `epoch()` accessors. Only code inside `commit_safety_gate` (Phase B's `authorize_destructive_commit` in commit 6, plus the sibling `tests` child module) can fabricate or mutate a permit — the rest of `voom-store` cannot. Phase B builds permits in-module via the struct literal; **no crate-visible constructor is exposed**, because exposing one would re-open the bypass path the module-private fields are there to close. The permit carries NO per-member epoch snapshot — that snapshot is persisted to `commit_intents.target_row_epochs` (migration 0005) and re-read by Phase C from the DB by `commit_id`. `TargetMemberKind` derives `Serialize, Deserialize` with `#[serde(rename_all = "snake_case")]` so the JSON column form matches the gate's existing vocabulary. Revised after Codex round-4 finding #1 and two stop-time review rounds — the original scaffold's all-pub `target_row_epochs: Vec<(TargetMemberKind, u64, u64)>` field allowed callers to bypass the stale-epoch trip-wire and lost the snapshot on process crash; an intermediate `pub(crate)` + speculative `new()` shape still let any code in `voom-store` fabricate permits and shipped an unused API behind a `cfg_attr(not(test), expect(dead_code))` workaround; the final shape (this row) replaces that with module-private fields and no constructor at all.
  - `CommitGateResult` variants for Sprint 1: `Allowed`, `BlockedByUseLease`, `BlockedByPendingCommit`, `BlockedByClosureGrew`, `BlockedByClosureIncomplete`, `BlockedByStaleEvidence`, `BlockedByStaleTargetEpoch { drift }`, `CancelledAfterAuthorize`.
  - `ForcePathToken { actor: String, reason: String, bypass: BTreeSet<BypassKind> }`, with `BypassKind` enum carrying just `ClosureIncomplete` for Sprint 1. **Stub only** — defined for compile-stability, but no caller in commits 1–9. Commit 10 retrofits `prepare_destructive_commit` and `authorize_destructive_commit` to accept `Option<ForcePathToken>` and ships the serde + validation + bypass branches in one slice.
  - `ClosureWarning`, `ClosureFailure`, `EvidenceDrift`, `EvidenceRevalidationResult`, `PendingCommitIntent`. (Previously enumerated `BlockedByPendingCommitDetail` / `BlockedByClosureGrewDetail` parallel structs are deleted per §3 — the inline variant bodies on `CommitGateResult` are now the single source of truth for both shapes.)

**Sibling test** `commit_safety_gate_test.rs`: constructor + Debug round-trip smoke for the public types.

**Exit:** `just ci` green; M1 sibling tests still pass against the rewired `FailureClass` mapping; the new types are reachable from `voom-store` consumers but no algorithm uses them yet.

### Commit 2 — `AliasResolver` trait + test fixture (sub-slice 2)

Retroactive description after the round-5 fix: commit 2 (`ae4be4b`) originally also shipped a `SqliteAliasResolver` production impl alongside the trait + `FailingAliasResolver` fixture. The Codex round-5 review (see §3 "Alias resolver transaction boundary") flagged that `SqliteAliasResolver::aliases_for_version` read via `fetch_all(&self.pool)`, which is incompatible with the gate's IMMEDIATE-tx closure-walker requirement (either observes rows outside the tx snapshot or deadlocks on a size-1 pool). The round-5 fix commit deletes `SqliteAliasResolver` entirely and adds `IdentityRepo::list_live_file_locations_by_version_in_tx` for DB-internal alias enumeration inside the gate tx (consumed by commit 4 / Phase A). The doc-only retroactive description below reflects the post-fix shape; the `ae4be4b` commit history itself is not rewritten.

- Add `AliasResolver` trait + `AliasResolutionError` enum to `commit_safety_gate`. The trait is reserved for **external (non-DB) alias sources** (FS mounts, object stores) — DB-internal enumeration uses the in-tx identity helper instead.
- Add `FailingAliasResolver` to `voom-store::test_support` under the existing test-support feature gate: takes a configured set of `FileVersionId`s and returns `AliasResolutionError::Unreachable` for them. Sprint 1 ships no production resolver; this fixture is the only `AliasResolver` impl in-tree.

**Sibling tests** cover `FailingAliasResolver` returns `Unreachable` for configured IDs and the empty set otherwise.

**Exit:** `just ci` green; trait + test fixture exercised in unit tests; integration tests still empty (need Phase A/B to consume them).

### Commit 3 — `IdentityRepo` destructive `_in_tx` mutations (sub-slice 3)

One new method on `IdentityRepo` trait + `SqliteIdentityRepo` impl, plus sibling-test gap plug for two existing M2 methods. Pure additions — no callers yet. No M2 method bodies are touched (see §7).

**Already in M2 (used as-is):**
- `retire_file_location_in_tx(tx, id, retired_at, expected_epoch) -> Result<FileLocation, VoomError>` (`identity.rs:618` / impl `:1261`) — guarded UPDATE on `file_locations` (`WHERE id = ? AND epoch = ? AND retired_at IS NULL`); sets `retired_at`, bumps `epoch`; returns the row. `rows_affected != 1` → `VoomError::Conflict` covers both already-terminal AND stale-epoch.
- `retire_file_version_in_tx(tx, id, retired_at, expected_epoch) -> Result<FileVersion, VoomError>` (`identity.rs:586`) — same shape against `file_versions`. No Phase C dispatch consumes `retire_file_version_in_tx` in Sprint 1 (`DeleteFileVersion` deferred per Phase 2 plan §3 round-5 fix; see Phase 2 plan §9 follow-up). The sibling-test gap plug remains independently valuable because the method exists in M2 and the epoch-guard contract had zero coverage; covering it pins the contract for the eventual Sprint 5+ caller.

The Phase C call sites that DO exist (`retire_file_location_in_tx` + `replace_file_location_in_tx`) consume the row return value as `let _ = id_repo.retire_..._in_tx(...).await?;`. The earlier plan draft specified `Result<(), VoomError>` here; that draft was wrong about the M2 convention and is corrected by this row.

**New in this commit:**
- `replace_file_location_in_tx(tx, retired_id, retired_expected_epoch, new_location: NewFileLocation, retired_at) -> Result<FileLocationId, VoomError>` — atomically retires `retired_id` under its `expected_epoch` guard and inserts a new `FileLocation` on the same `FileVersion`. Two defense-in-depth guards (Codex round-6): (a) **pre-fetch and reject** — reads the retired row in-tx and returns `VoomError::Conflict` if `new_location.file_version_id` differs from the retired row's version (no partial write); (b) **SAVEPOINT** wraps the UPDATE retire + INSERT new pair so a caller that commits the outer tx after an insert failure observes the old row still live. Takes `NewFileLocation` (identity's own type) — not `FileLocationProposal` (a `commit_safety_gate` type) — so identity does not import from the gate layer above it. The round-2 gate-boundary type-level invariant still holds: `FileLocationProposal` has no `file_version_id`, so Phase C (commit 7) can only source it by reading the retired row.

**Sibling tests** in `identity_test.rs` plug the existing M2 gap and cover the new method:
- `retire_file_location_in_tx` (zero coverage in M2 today): happy path; `Conflict` on already-terminal row; **`Conflict` on stale-epoch live row** (the guard Phase C will rely on).
- `retire_file_version_in_tx` (zero coverage in M2 today): same three rows.
- `replace_file_location_in_tx` (five sibling tests after round-6): happy path; `Conflict` on already-terminal retired row with atomicity check (no new row inserted); `Conflict` on stale-epoch live retired row with atomicity check; **`Conflict` on cross-version mismatch** with atomicity check (replaces the prior "trusts caller" test pin); **`Database` error + SAVEPOINT rollback** verified via a temporary `BEFORE INSERT` trigger that forces the INSERT to fail after the version-check passes. Plus three schema-CHECK negative-coverage tests in `commit_safety_gate_test.rs` asserting SQLite rejects `commit_intents` rows with `closure_authorized = NULL` for `authorized` / `completed` / `recovery_required`.

**Exit:** `just ci` green; ten new sibling-test rows; `replace_file_location_in_tx` covered for both terminal-row and stale-epoch `Conflict` paths plus the atomicity guarantee; no integration test (no caller yet).

### Commit 4 — `prepare_destructive_commit` (Phase A) (sub-slice 4)

**`voom-events`**
- Add `SubjectType::CommitIntent`.
- Add `EventKind` variants: `commit.intent_recorded`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete`.
- Add `Event` payload variants + `voom-events::kind_test.rs` / `payload_test.rs` round-trip coverage.

**`voom-store::repo::commit_safety_gate`**
- `phase_a_gate_abort_with_event(...)` helper encoding the two-tx pattern (sequencing doc §5.2): named to reflect narrow scope so the pattern cannot accidentally leak into Phases B/C.
- `DestructiveCommit` input shape in commit 4 does **not** carry an `override_token` field. The force path lands in commit 10 (see §3 decision row) which retrofits the signature.
- `prepare_destructive_commit` implementation:
  1. Closure walk: target → `FileVersion`(s) → live `FileLocation`s + owning `AssetBundle`(s). DB-internal alias enumeration calls `IdentityRepo::list_live_file_locations_by_version_in_tx(tx, file_version_id)` directly on the gate's IMMEDIATE tx handle (round-5 fix; sees within-tx writes the pool variant cannot). External (non-DB) alias sources — FS mounts, object stores — are consulted via the injected `&dyn AliasResolver` (Sprint 1 ships only the `FailingAliasResolver` test fixture; production resolvers land in Sprint 4/5). On `AliasResolutionError::Unreachable`, abort unconditionally with `BlockedByClosureIncomplete` + `commit.aborted_by_closure_incomplete` (two-tx pattern). No bypass branch exists in this commit; commit 10 adds the `closure_incomplete` bypass.
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
     - `ReplaceFileLocation` / `MoveFileLocation` → `replace_file_location_in_tx(tx, retired_id, retired_expected_epoch, new_proposal, now)`.
     (`ArchiveFileVersion` and `DeleteFileVersion` dispatch removed — variants do not exist in Sprint 1; see §3. `DeleteFileVersion` deferral revised after Codex round-5 finding #2.)
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
- `ArchiveFileVersion` and `DeleteFileVersion` are not implemented; the `CommitTarget` enum carries only the three location-level variants (`DeleteFileLocation`, `ReplaceFileLocation`, `MoveFileLocation`). Both are deferred to Sprint 5+ per parent-spec precedent for `ArchiveBundle` / `DeleteBundle` and the Codex round-5 finding on `DeleteFileVersion` cascade semantics (see §9 follow-ups).
- M2 sibling tests for `record_discovered_file_in_tx` and `reconcile_rename_in_tx` extended (not rewritten) and still pass.
- Two-tx pattern is used only for Phase A gate-check aborts (grep-asserted).
- `just ci` green.

## 6. Cross-cutting deltas per commit (consolidated)

| Commit | `voom-core` | `voom-events` | `voom-store` | Tests |
|---|---|---|---|---|
| 1 | `CommitId`; 5 `ErrorCode` + `VoomError` variants; `FailureClass` remap | — | `commit_safety_gate` module + type stubs incl. opaque `CommitPermit` (accessors only) and `CommitGateResult::BlockedByStaleTargetEpoch`. **Migration 0005** (commit-intent persistent permit + `recovery_reason` column) folded into this commit's round-4 fix; see §3. | sibling smoke + extended `failure_test.rs` |
| 2 | — | — | `AliasResolver` trait (scoped to external sources only — round-5 retroactive narrowing) + `FailingAliasResolver` test fixture. `SqliteAliasResolver` was shipped here in `ae4be4b` then deleted by the round-5 fix; DB-internal alias enumeration moves to `IdentityRepo::list_live_file_locations_by_version_in_tx` (round-5 fix commit). | sibling for `FailingAliasResolver` |
| 3 | — | — | 1 new `IdentityRepo` mutation (`replace_file_location_in_tx`); 2 existing M2 retire methods (location, version) used as-is and get sibling-test coverage they lacked. `retire_file_version_in_tx` has no Phase C dispatch in Sprint 1 (`DeleteFileVersion` deferred per §3 round-5); the gap plug pins the epoch-guard contract for the eventual Sprint 5+ caller. | sibling for `replace` + gap-plug rows for both existing retire methods (each incl. dedicated `expected_epoch` mismatch on a live row) |
| 4 | — | `SubjectType::CommitIntent`; `commit.intent_recorded`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete` | `prepare_destructive_commit` + `phase_a_gate_abort_with_event` | sibling + integration `commit_safety_gate.rs` (Phase A) |
| 5 | — | — | `consult_pending_commit_lock_in_tx` + 2 callers retrofitted | extended M2 sibling + integration `commit_safety_gate_pending_lock.rs` |
| 6 | — | `commit.authorized`, `.aborted_by_closure_grew` | `authorize_destructive_commit` (Phase B persists `target_row_epochs` JSON into `commit_intents` atomically with `state = 'authorized'`) | sibling + integration `commit_safety_gate_after_rename.rs` |
| 7 | — | `commit.completed`, `.aborted_post_mutation` (now incl. `reason='stale_target_epoch'` + `target_epoch_drift` payload field), `.aborted_pre_mutation`, `.recovery_required` | `finalize_destructive_commit` (re-reads `target_row_epochs` from `commit_intents` by `commit_id`; recovery-required writes go to `recovery_reason` column). Silent-dispatch list reduces to three branches (`DeleteFileLocation`, `ReplaceFileLocation`, `MoveFileLocation`); `DeleteFileVersion` dispatch dropped under §3 round-5 deferral. | sibling + integration `commit_safety_gate_recovery_required.rs` |
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
- **Parent-spec drift to flag upstream (round-5).** `docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md` §9.3.1 (around line 1650) still defines `DeleteFileVersion` on `CommitTarget`, and §9.3.2 dispatch tables (around line 2253) reference it. This plan now defers the variant to Sprint 5+ per Codex round-5 finding #2 (retiring a `FileVersion` without a defined cascade leaves live `FileLocation` rows pointing at the retired version). Tag for the next sprint-1 spec adversarial round alongside the existing `ArchiveFileVersion` and `CommitPermit` drift notes.
- **Round-6 overlay** (Codex review of branch through commit `edba8e4`, verdict `needs-attention`). Three findings folded into a single `fix(store)` commit on top of `edba8e4`: (1) `replace_file_location_in_tx` enforces the cross-version invariant inside identity (defense-in-depth, not just at the gate boundary — the public trait method had no enforcement); (2) the retire+insert pair is wrapped in a SAVEPOINT so a caller that catches `Err` and commits the outer tx does not persist data loss; (3) migration 0005 CHECK requires `closure_authorized IS NOT NULL` for the three post-Phase-B states. The cross-version isolation test pinned in `edba8e4` is inverted in-place. Design spec: `docs/superpowers/specs/2026-05-18-voom-sprint-1-m3-phase-2-round-6-overlay-design.md`.
- **Round-7 overlay** (Codex review of branch through commit `275f341` — final commit of the M3 P2 sequence; verdict `needs-attention`, 1 critical + 2 high). Three findings folded into a single `fix(store)` commit on top of `275f341`: (1) [critical] `finalize_destructive_commit`'s `Applied` branch propagated post-mutation DB failures without recovery state; SAVEPOINT now wraps dispatch + completion + event append, and an inner Err transitions the row to `recovery_required` with `recovery_reason = 'mutation_failed'` plus a new `CommitGateResult::BlockedByMutationFailed { error }` variant; (2) [high] caller-observed closure (`MutationOutcome::Applied { observed }`) is now destructured and merged with the recomputed `closure_final` before Phase C trip-wire decisions, so caller-only aliases surface as `added_*` entries on the closure-grew payload; (3) [high] `prepare_destructive_commit` consults `consult_pending_commit_lock_in_tx` (M3 P2 commit 5) for every member of `closure_initial` before inserting the new pending intent, blocking overlapping prepare-vs-prepare via `BlockedByPendingCommit` + a new `commit.aborted_by_pending_commit` event. The DB-level partial-unique-index secondary recommendation was considered but deferred — SQLite partial-index predicates cannot reference another table's state, so the in-tx consult is the durable enforcement point.
- **Round-8 overlay** (Codex review of branch through commit `56c9053` — round-7 overlay; verdict `needs-attention`, 1 critical + 1 high). Two findings folded into a single `fix(store)` commit on top of `56c9053`: (1) [critical] the round-7 SAVEPOINT covered the silent dispatch + completion + event append but not the Phase C trip-wire recompute that runs immediately before it. A closure-walker abort at Phase C (translated to `VoomError::Internal` per the round-5 escape rule) propagated `?` out of finalize and left the row stuck in `'authorized'` — even though the caller had performed the durable FS mutation. Fix expands the recovery boundary: `finalize_applied_with_recovery_boundary` opens a savepoint covering decode + trip-wires + either silent path or trip-wire branch; on any inner Err the savepoint rolls back and the outer tx routes through `finalize_mutation_failed_in_tx` with `recovery_reason = 'mutation_failed'`. The round-7 inner savepoint inside the silent-path helper is removed because the outer boundary subsumes it. (2) [high] gate transactions used SQLite's default deferred BEGIN, leaving the round-7 overlapping-prepare check racy at the lock layer (two prepares could both read "no overlap" before either wrote). Fix introduces `begin_gate_tx` helper that calls `pool.begin_with("BEGIN IMMEDIATE")` (sqlx 0.8.6); all four phase entry points and both legs of `phase_a_gate_abort_with_event` route through it. RESERVED-on-BEGIN serializes overlapping writers at the lock layer, with `busy_timeout` (5s) bounding contention.

- **Round-9 review** (Codex review of branch through commit `891c995` — round-8 overlay; verdict `needs-attention`, 1 high + 1 medium). Findings **not addressed in this branch** — adversarial-review budget for this sprint cycle (3 reviews) exhausted; both findings tracked for the next sprint-1 spec adversarial round and the eventual M3 cleanup PR. (1) [high] Phase A two-tx abort can durably commit the aborted `commit_intents` row in tx1 then fail before tx2's `commit.aborted_by_*` event lands (BEGIN IMMEDIATE contention, append_in_tx error, tx2 commit failure, or process crash). This is the documented tradeoff of the two-tx pattern (sequencing doc §5.2 — abort row durable before event row, inverting the failure mode vs. event-without-row). Cross-cutting follow-up: introduce an outbox + idempotency-key recovery worker (Sprint 4/5) so an aborted row missing its event is detected and repaired rather than retried into duplicate aborts. (2) [medium] `first_evidence_pin_drift` returns `false` (i.e., treats as non-drift) when a pinned `FileVersion` or `FileLocation` row is missing from the DB. Should fail closed by mapping `None` to `PinnedFileVersionRetired` / `PinnedLocationRetired`. Small targeted fix; add to the next overlay commit. Both findings are tagged in the PR description for upstream review.
