---
title: VOOM Sprint 1 M3 — Sequencing & Implementation Plan
status: draft
created: 2026-05-17
parent_spec: docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md
parent_sections: §§9–11, §12 (cross-cutting), §13 (testing), §14 (exit criteria)
scope: implementation sequencing for M3 only — no design changes
---

# VOOM Sprint 1 M3 — Sequencing & Implementation Plan

## 1. Purpose

This document is the **sequencing plan** for Sprint 1 M3. The full M3
design — schemas, APIs, algorithms, error semantics, force-path rules,
event payloads, CLI envelopes — already lives in the parent sprint-1
spec at §§9–11. Eight adversarial-review rounds have closed against
that spec (commit `13ad8c2`). This document does not re-design; it
orders the work into five phases with explicit exit gates so the
writing-plans skill can turn it into an executable implementation
plan, and surfaces the M1/M2 touch points and the small number of
judgment calls the parent spec leaves open.

When this document and the parent spec disagree, the parent spec wins.

## 2. Five-phase ordering

Each phase ends at a green-CI checkpoint (`just ci`) and lands on the
existing `feat/sprint-1` branch as a series of commits. No PR is
opened mid-sprint (per project convention).

### Phase 0 — Migration `0004_use_leases_ancillary.sql`

Single migration file containing all ten M3 tables (parent spec §4
mandates one `0004` migration; do not split):

- `asset_use_leases`
- `commit_intents`, `commit_intent_scope_members`
- `external_systems`, `external_system_links`, `external_path_mappings`
- `issues`, `issue_links`
- `quality_scoring_profiles`, `quality_scores`

Plus the indexes named in §9.1 and §10.

**Deliverables:** the migration SQL, a `migration_inventory.rs`
update asserting the ten new tables, and a `voom init` smoke test
that the migration applies cleanly on a fresh DB. No repo code, no
Rust types beyond what the inventory test needs.

**Exit:** `just ci` green; `voom init` smoke passes; `migration_inventory`
sibling test enumerates the ten tables.

### Phase 1 — Asset use leases (parent §9.1, §9.2)

`voom-store::repo::use_leases.rs` and its sibling test, sliced by
lifecycle method in this order:

1. `acquire_in_tx` (without the pending-commit lock — that lands in
   Phase 2 once `commit_intent_scope_members` is populated; parent
   §9.2 explicitly defers).
2. `heartbeat`
3. `release`
4. `force_release`
5. `expire_due`
6. `recover_stale_issuer`
7. `reanchor_on_move`

Each method:

- Is implemented as `_in_tx` per parent §5.1.
- Emits its event (per parent §6.1) in the same transaction via
  `EventRepo::append_in_tx`.
- Carries the sibling test that asserts both the row mutation and
  the event row.

**New Rust types:**

- `voom-core` ID newtypes: `UseLeaseId` (per parent §12.2).
- `voom-events::EventKind` variants: `UseLeaseAcquired`,
  `UseLeaseReleased`, `UseLeaseExpired`, `UseLeaseForceReleased`,
  `UseLeaseRecoveredStaleIssuer`, `UseLeaseReanchoredByMove`.
- `voom-events` payload structs for each new kind plus the `Event`
  enum variants and `SubjectType::AssetUseLease`.
- `voom-store::repo::use_leases::{LeaseScope, NewUseLease,
  UseLease, ReanchorReport, ExpireReport}` types (parent §9.1, §9.2).
- One `ControlPlane` use case file `cases/use_leases.rs` exposing
  one method per `UseLeaseRepo` lifecycle entry.

**Integration test:** `crates/voom-store/tests/use_lease_lifecycle.rs`
covering acquire → heartbeat → release / expire / force_release /
recover_stale_issuer / reanchor against both `:memory:` and disk
pools.

**Exit:** every method has a sibling test, every event round-trips
through `voom-events::kind_test.rs` and `payload_test.rs`, the
integration test passes both pool modes, `just ci` is green.

### Phase 2 — Commit safety gate (parent §9.3)

The biggest phase. Sliced in dependency order:

1. **Skeleton + types.** New module `voom-store::repo::commit_safety_gate`
   with the public types from parent §9.3.1: `CommitTarget`,
   `AffectedScopeClosure`, `CommitIntent`, `CommitPermit`,
   `CommitGateOutcome`, `CommitGateResult`, `CommitIntentState`,
   `MutationOutcome`, `AbortReason`, `ForcePathToken`,
   `BlockedByPendingCommitDetail`, `BlockedByClosureGrewDetail`,
   `ClosureWarning`, `ClosureFailure`, `EvidenceDrift`,
   `EvidenceRevalidationResult`, `PendingCommitIntent`. Plus the
   new `voom-core` ID newtype `CommitId` and the parent §12.1 error
   variants `BlockedByUseLease`, `BlockedByPendingCommit`,
   `BlockedByClosureGrew`, `StaleIdentityEvidence`,
   `ClosureResolutionIncomplete`. Each type added as part of the
   slice that first needs it; types ride with their use-site, not
   landed in a single front-loaded commit.

2. **`AliasResolver`.** Trait + `SqliteAliasResolver` impl + the
   test-only `FailingAliasResolver` in `voom-store::test_support`
   (parent §9.3.4). Tests assert the unreachable path surfaces
   `BlockedByClosureIncomplete` in Phase A and Phase B.

3. **`IdentityRepo` destructive mutations.** Four new `_in_tx`
   methods called only by `finalize_destructive_commit` step 4:
   `retire_file_location_in_tx`, `retire_file_version_in_tx`,
   `archive_file_version_in_tx`, `replace_file_location_in_tx`
   (parent §9.3.2 Phase C step 4). Sibling tests in
   `crates/voom-store/src/repo/identity_test.rs`.

4. **`prepare_destructive_commit` (Phase A).** Closure walk,
   blocking-lease check, accepted-evidence revalidation, intent row
   insert + `commit_intent_scope_members` expansion across all four
   granularities, `commit.intent_recorded` emission. Each Phase A
   gate-check abort (`BlockedByUseLease`, `BlockedByStaleEvidence`,
   `BlockedByClosureIncomplete`) uses the two-tx pattern from
   parent §9.3.5 (no `commit_intents` row materializes).

5. **Pending-commit lock retrofit.** Add the lock-consultation
   query (parent §9.2 SQL) to:
   - `UseLeaseRepo::acquire_in_tx` (Phase 1 code).
   - `IdentityRepo::record_discovered_file_in_tx::AliasAttached`
     branch (M2 code). Existing M2 sibling tests extended; behavior
     under no in-flight commits unchanged.
   - The lock helper itself lives in
     `voom-store::repo::commit_safety_gate` as the single source of
     truth; the two callers invoke it. Test fixtures cover both
     blocking and advisory leases (parent §9.2: lock applies to
     both modes).
   - The lock is **not** consulted by
     `IdentityRepo::reconcile_rename_in_tx` (architectural
     exemption — parent §8.7, §9.2, §9.3.2; arch spec lines 697–708).

6. **`authorize_destructive_commit` (Phase B).** The architectural
   "immediately before" recheck (arch spec lines 1044–1052). One
   IMMEDIATE transaction. Phase B aborts commit in-transaction
   (no two-tx pattern; parent §9.3.5). Updates
   `commit_intent_scope_members` to match the recomputed closure.

7. **`finalize_destructive_commit` (Phase C).** Three branches:
   - `MutationOutcome::NotPerformed` → `CancelledAfterAuthorize`
     (parent §9.3.2 Phase C step 2).
   - Defensive trip-wire firing → `recovery_required` state +
     `commit.aborted_post_mutation` event (parent §9.3.2 Phase C
     step 3, all three sub-branches).
   - Silent trip-wire → durable `IdentityRepo` mutation +
     `commit.completed` (parent §9.3.2 Phase C steps 4-5).

8. **`abort_destructive_commit` (pending-only).** Parent §9.3.2
   `abort_destructive_commit`. Rejects `state = 'authorized'` with
   `Conflict`.

9. **`list_pending_commit_intents`.** Read-only; covers both
   `pending` and `authorized` states (parent §9.3.1 doc-comment).

10. **Lease re-anchoring on rename.** Extend
    `IdentityRepo::reconcile_rename_in_tx` (M2 code) to call
    `UseLeaseRepo::reanchor_on_move` inside the same transaction.
    Existing M2 sibling test extended; new assertions cover the
    re-anchoring branch + emission of `use_lease.reanchored_by_move`
    per affected lease.

11. **Force path.** `ForcePathToken` parsing, `bypass`-set
    validation rejecting non-`closure_incomplete` bits with
    `VoomError::Config`, `commit.forced_override` event emission,
    and the `commit_intents.override_token` durable column
    threading from prepare to authorize (parent §9.3.3).

**Integration tests** under `crates/voom-store/tests/`:

- `commit_safety_gate.rs` — parametrized over each `Blocked*`
  variant × each phase the spec routes it through.
- `commit_safety_gate_after_rename.rs` — parent §9.4 end-to-end
  rename × authorize × evidence scenario.
- `commit_safety_gate_force_path.rs` — token honored through to
  authorize; non-`closure_incomplete` bypass bits rejected.
- `commit_safety_gate_recovery_required.rs` — Phase C trip-wire
  branches (closure_grew, fresh_lease, both) leaving the intent in
  `recovery_required`.
- `commit_safety_gate_pending_lock.rs` — pending-commit lock
  rejects `UseLeaseRepo::acquire` and the `AliasAttached` branch;
  `reconcile_rename_in_tx` proceeds.

**Exit:** every `CommitGateResult` variant is triggered in at least
one integration test in the correct phase; the M2 sibling tests
for `record_discovered_file_in_tx` and `reconcile_rename_in_tx` are
extended (not rewritten) and still pass; the two-tx pattern is used
only for Phase A gate-check aborts; `just ci` green.

### Phase 3 — Ancillary registries + terminal-failure auto-open

Three repos plus the M1 use-case wiring change. Internal sub-areas
are parallelizable but ordered here for clarity:

1. **External systems** (parent §10.1). Repo + use cases + event
   kinds for: registration, profile update, health update, and
   retirement, plus the link-table events (`external_system_link.*`
   added / retired) and path-mapping events
   (`external_path_mapping.*` added / retired). Exact dotted names
   chosen during the slice that adds the payload structs (parent
   §10.1 specifies "each mutation emits its matching
   `external_system.*` event"; final string set lands in
   `EventKind::as_str` at that point). CRUD + `update_health`.

2. **Quality scores** (parent §10.3). Repo + use cases + 3 event
   kinds (`quality_profile.registered`, `quality_score.recorded`,
   `quality_score.superseded`). No scoring math; scores
   caller-provided.

3. **Issues** (parent §10.2). Repo + use cases + 6 event kinds
   (`issue.opened`, `.priority_changed`, `.resolved`, `.suppressed`,
   `.accepted`, `.linked`). Both `_in_tx` and bare forms exist for
   `open` and `link` (the auto-open path needs `_in_tx`).

4. **Terminal-failure auto-open wiring** (parent §10.2). Modify the
   existing M1 control-plane use cases `fail_lease`,
   `expire_due`, `force_release_lease` (live method names in
   `crates/voom-control-plane/src/cases/leases.rs`) to, in the same
   transaction that writes `ticket.failed_terminal` and the matching
   `lease.*` event, call `IssueRepo::open_in_tx` + `link_in_tx` for
   the `terminal_failure` issue. `IssueSeverity` / `IssuePriority`
   derived via the parent §12.5 methods on `FailureClass`. The
   `TicketFailedTerminal` payload's `issue_id` is set to the
   newly-opened issue's id (the field already accepts
   `Option<IssueId>` from M2 commit `c1279b6` — Phase 3 only flips
   it from always-null to always-populated).

5. **M1 frozen tests cleanup.** The M1 sibling tests asserting
   `issue_id == null` no longer exercise reachable code post-Phase 3
   (every terminal transition opens an issue). Delete those
   assertions in the same slice as the wiring change (do not
   leave dead expectations behind).

**Integration test:** `terminal_failure_opens_issue.rs` covering
all three terminal entry points × at least one `FailureClass` per
`FailureRetryClass` (`Retriable`-with-retries-exhausted,
`NonRetriable`, `OperatorRequired`).

**Exit:** all three registry repos have sibling-tested CRUD; the
auto-open integration test passes against all three terminal entry
points; M1 frozen-null tests are deleted (not relaxed); `just ci`
green.

### Phase 4 — CLI inspection surface (parent §11)

`voom-cli/src/commands/` gains one file per new resource group.
Sliced by resource group (one commit per group), each carrying its
insta snapshots:

`job`, `ticket`, `lease`, `worker`, `artifact`, `work`, `variant`,
`bundle`, `asset`, `evidence`, `issue`, `score`, `external-system`,
`use-lease`, `commit-intent`, `event`.

`voom-control-plane` exposes narrow read-only `list_*` / `get_*`
methods per resource (parent §3); the broader write surface stays
internal to the crate.

**Snapshot coverage** per parent §11.3, plus the seven Sprint-1
error envelopes (`BLOCKED_BY_USE_LEASE`, `BLOCKED_BY_PENDING_COMMIT`,
`BLOCKED_BY_CLOSURE_GREW`, `STALE_IDENTITY_EVIDENCE`,
`CLOSURE_RESOLUTION_INCOMPLETE`, `DEPENDENCY_CYCLE`, `CONFLICT`).
The `BLOCKED_BY_CLOSURE_GREW` snapshot is shaped by triggering the
Phase B authorize recheck against a fixture that runs an external
rename between `prepare` and `authorize` (parent §11.3 note: rename
is the only architecturally-exempt path that can shift the closure
into authorize).

**Smoke recipe extensions** per parent §13.3 added to
`justfile :: smoke`.

**Exit:** all 16 resource groups present with §11 subcommand trees;
the §11.3 snapshot list is the explicit checklist and every entry
has a snapshot; `just smoke` extended recipe passes; `just ci`
green.

## 3. Per-phase exit criteria (consolidated)

| Phase | Exit gate |
|---|---|
| 0 | Migration applied cleanly; `migration_inventory` enumerates ten new tables; `voom init` smoke green; `just ci` green. |
| 1 | All seven `UseLeaseRepo` lifecycle methods implemented with sibling tests; integration test green on both pool modes; all seven `use_lease.*` events round-trip; `just ci` green. |
| 2 | Every `CommitGateResult` variant triggered in at least one integration test in the correct phase; pending-commit lock asserted to block `UseLeaseRepo::acquire` and `AliasAttached` and to **not** block `reconcile_rename_in_tx`; force path rejects non-`closure_incomplete` bypass bits; M2 sibling tests extended and green; `just ci` green. |
| 3 | All three registries CRUD-tested; `terminal_failure_opens_issue.rs` covers all three terminal entry points × three `FailureRetryClass` cases; M1 frozen-null tests deleted; `just ci` green. |
| 4 | All 16 CLI resource groups present; §11.3 snapshot list fully covered; seven error envelopes snapshotted; `just smoke` extended recipe green; `just ci` green. |

Sprint-level M3 exit (matches parent §14): `cargo test --workspace
--all-features` green, extended `just smoke` green, every M3
integration test in parent §13.2 present.

## 4. Cross-cutting touch points

The M3 design requires modifications to M1 and M2 code beyond pure
additions. Surfacing them so they're not surprises during impl.

### 4.1 `voom-core` additions

New ID newtypes via `define_id!` (parent §12.2): `UseLeaseId`,
`CommitId`, `ExternalSystemId`, `ExternalSystemLinkId`,
`ExternalPathMappingId`, `IssueLinkId`, `ScoreId`, `ScoreProfileId`.

`IssueId` already lives in `voom-core` from M2 commit `6e7853a`;
confirm and reuse.

New `ErrorCode` / `VoomError` variants (parent §12.1):
`BlockedByUseLease`, `BlockedByPendingCommit`, `BlockedByClosureGrew`,
`StaleIdentityEvidence`, `ClosureResolutionIncomplete`. Confirm
`DependencyCycle` and `Conflict` against the current
`voom-core::error` enum and add only the missing ones (parent §12.1
lists the full target set; M1/M2 may already have several).

The 17 `FailureClass`-derived `ErrorCode` variants from parent §12.1
were partially landed with M1's `FailureClass` work (commits
`6e7853a` / `9b72c84` / `c1279b6`). Confirm the gap against the
parent-spec list and add the rest as part of Phase 3.

`IssueSeverity` / `IssuePriority` already exist (M2 commit `6e7853a`).
Reuse via the parent §12.5 methods on `FailureClass`.

### 4.2 `voom-events` additions per phase

| Phase | New `EventKind` variants |
|---|---|
| 1 | `use_lease.acquired`, `.released`, `.expired`, `.force_released`, `.recovered_stale_issuer`, `.reanchored_by_move` |
| 2 | `commit.intent_recorded`, `.authorized`, `.completed`, `.aborted_pre_mutation`, `.aborted_by_use_lease`, `.aborted_by_stale_evidence`, `.aborted_by_closure_incomplete`, `.aborted_by_closure_grew`, `.aborted_post_mutation`, `.forced_override`, `.recovery_required` |
| 3 | `external_system.*` plus `external_system_link.*` and `external_path_mapping.*` (set finalized during the §10.1 payload slice), `issue.opened`, `.priority_changed`, `.resolved`, `.suppressed`, `.accepted`, `.linked`, `quality_profile.registered`, `quality_score.recorded`, `quality_score.superseded` |

Each phase extends `SubjectType`, `EventKind::as_str`,
`EventKind::from_str`, and the `Event` payload sum-type to keep
round-trip tests in `kind_test.rs` / `payload_test.rs` green.

### 4.3 Touch-back into M2 code (Phase 2)

- **`IdentityRepo::record_discovered_file_in_tx::AliasAttached`** —
  consults the pending-commit lock; returns `BlockedByPendingCommit`
  on match. M2 sibling test extended; pre-M3 behavior preserved
  under no in-flight commits.
- **`IdentityRepo::reconcile_rename_in_tx`** — gains a call to
  `UseLeaseRepo::reanchor_on_move` inside the same transaction.
  Does **not** consult the pending-commit lock. M2 sibling test
  extended.
- **`IdentityRepo` destructive mutations** — four new `_in_tx`
  methods (`retire_file_location_in_tx`, `retire_file_version_in_tx`,
  `archive_file_version_in_tx`, `replace_file_location_in_tx`).
  Pure additions, called only from Phase C step 4.

### 4.4 Touch-back into M1 code (Phase 3)

- **`ControlPlane::fail_lease`,
  `ControlPlane::expire_due`,
  `ControlPlane::force_release_lease`** — in the same transaction
  that writes the terminal event, call `IssueRepo::open_in_tx` +
  `link_in_tx` and populate `TicketFailedTerminal.issue_id`.
- **`LeaseRepo` API** — unchanged. The auto-open wiring is a
  use-case-layer composition.
- **M1 sibling tests asserting `issue_id == null`** — deleted in
  the Phase 3 wiring slice. Post-M3, every terminal transition
  opens an issue; the null-asserting tests no longer cover
  reachable code.

## 5. Pre-decided judgment calls

The parent spec leaves a small number of placement and pattern
choices implicit. Pre-deciding them so the impl plan is unambiguous:

### 5.1 Pending-commit lock helper placement

The lock-consultation query (parent §9.2 SQL) lives as a
`pub(crate)` helper in `voom-store::repo::commit_safety_gate`. Both
`UseLeaseRepo::acquire_in_tx` and
`IdentityRepo::record_discovered_file_in_tx` call into it.

Rationale: the lock semantics are gate-owned; a single source of
truth avoids drift between the two callers. The sideways dependency
from `IdentityRepo` (M2) into `commit_safety_gate` (M3) is
acceptable because the gate is a host-side helper module, not a
domain repo, and the dependency is one-way.

### 5.2 Two-tx pattern boundary

The two-tx pattern (parent §9.3.5) applies **only** to Phase A
gate-check aborts (`BlockedByUseLease`, `BlockedByStaleEvidence`,
`BlockedByClosureIncomplete` raised before the `commit_intents`
row is inserted). Phase B aborts, Phase C trip-wire aborts, Phase C
`NotPerformed`, and the dedicated `abort_destructive_commit` entry
point all commit the intent-state transition and the event row in
a single IMMEDIATE transaction.

To prevent accidental two-tx extension into the in-tx phases, the
two-tx pattern is encoded as an explicit helper in
`voom-store::repo::commit_safety_gate` named to reflect its narrow
scope (e.g., `phase_a_gate_abort_with_event`). Other phase
abort paths use the standard in-tx `EventRepo::append_in_tx`
composition the rest of the codebase uses.

### 5.3 `TicketFailedTerminal.issue_id` test migration

Phase 3 flips the field from always-null (M1/M2 behavior) to
always-populated. Plan:

- M1/M2 sibling tests asserting `issue_id == null` stay frozen
  until the Phase 3 wiring slice begins.
- Phase 3 wiring slice deletes those null assertions in the same
  commit as the wiring change (do not relax to "allow null or
  non-null" — that would mask a regression).
- New Phase 3 tests assert `issue_id` is non-null and the linked
  `terminal_failure` issue exists in `issues`.

## 6. Testing strategy

The parent spec's §13 prescribes most conventions; the M3 plan
adheres to them. Highlights:

- **Sibling unit tests** (`*_test.rs` next to source, per ADR-0004
  and `just check-test-layout`) cover every repo method, every gate
  function, every CLI command handler. Each test asserts both the
  row mutation and the matching event row in the same transaction
  via the `EventRepo` reader.
- **Integration tests** in `crates/voom-store/tests/` cover
  end-to-end scenarios that span multiple repos or the gate's
  multi-phase API (see Phase 2 list above + `use_lease_lifecycle.rs`
  + `terminal_failure_opens_issue.rs`). Every integration test runs
  against both `:memory:` and disk-mode pools via the `disk_mode`
  parity harness from M1.
- **Insta snapshots** in `crates/voom-cli/tests/snapshots/` cover
  the parent §11.3 list plus the seven Sprint-1 error envelopes.
  CLI envelope `schema_version` stays `"0"`.
- **Pending-commit lock tests** explicitly assert the architectural
  exemption (parent §8.7, §9.2, arch spec lines 697–708):
  `reconcile_rename_in_tx` succeeds against an in-flight commit on
  the affected `FileVersion`; the two locked entry points reject
  with `BlockedByPendingCommit`.
- **Recovery contract test.** A fixture inserts a
  `state = 'authorized'` intent + the matching `commit.authorized`
  event row, then asserts `abort_destructive_commit` returns
  `Conflict` (the post-authorize termination must go through
  `finalize(_, NotPerformed)`).
- **`FailureClass` → `IssueSeverity`/`IssuePriority` mapping** is
  exercised in `terminal_failure_opens_issue.rs` across all three
  terminal entry points and at least one class per
  `FailureRetryClass` value.
- **Out of scope for M3:** mutation testing, property-based tests,
  performance benchmarks. The parent spec does not require them
  and they would inflate scope.

## 7. Out of scope

Everything the parent spec §15 already excludes. M3-specific
non-deliverables worth re-stating:

- No filesystem-aware recovery worker for stuck `commit_intents`
  rows (Sprint 5+). M3 ships the table, the journal, and the API.
- No `ArchiveBundle` / `DeleteBundle` `CommitTarget` variants
  (Sprint 5).
- No CLI write commands. M3 CLI is read-only inspection.
- No `voom-api` deliverables (no API server binary in Sprint 1).
- No worker process integration (Sprint 2+).
