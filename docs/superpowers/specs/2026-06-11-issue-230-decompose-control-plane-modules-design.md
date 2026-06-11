# Decompose 1,000+ line voom-control-plane modules (issue #230)

- Status: Draft
- Date: 2026-06-11
- Issue: #230 ([audit L10] Decompose 1,000+ line voom-control-plane modules)
- Related ADR: ADR-0015 (module-decomposition seams; this issue)
- Related: ADR-0004 (sibling unit tests), ADR-0007 (phase-barrier coordinator),
  ADR-0012 (paused-time DB guard), ADR-0013 (payload evolution contract)

## Problem

Four `voom-control-plane` source modules exceed 1,000 lines and are the primary
drag on auditability and per-unit test focus:

| Module | src lines | test lines |
|---|---|---|
| `cases/execution/remote_execution.rs` | 2255 | 1470 |
| `workflow/coordinator.rs` | 1896 | 1410 |
| `workflow/execution/executor.rs` | 1438 | 3114 |
| `artifact/commit.rs` | 1140 | 807 |

(The audit's `planner.rs` reference is `voom-plan` and out of scope.)

A single 2,000-line file mixes several distinct responsibilities behind one
`impl ControlPlane` block and a long tail of private free functions. A reviewer
auditing the lease-acquire path must scroll past replay, heartbeat, complete,
fail, and recover code that has nothing to do with the change. Test files are
similarly monolithic.

## Goal

Decompose each oversized module into a directory module (`foo/mod.rs` + child
submodules) split along the cohesion seams the audit mapped, so each child file
holds one responsibility and (where it has unit tests) carries its own sibling
`*_test.rs`. This is a **pure refactor**: no behavior change, no public-API
change, no schema change, no new dependency.

## Non-goals

- Changing any runtime behavior, SQL, event payload, or error code.
- Changing the public surface of `voom-control-plane` (the `pub` re-exports in
  `artifact/mod.rs`, the `pub(crate)` re-exports in `workflow/mod.rs` and
  `workflow/execution/mod.rs`, and every `pub`/`pub(crate)` item a caller
  outside the split module relies on).
- Moving responsibilities across crate-layering boundaries. Filesystem
  promotion, worker dispatch, and use-case assembly stay in
  `voom-control-plane`; artifact-domain helpers stay in `voom-artifact`.
- Merging or deleting tests, weakening test gating, or reducing coverage.
- "Improving" adjacent code, renaming public items, or reflowing logic. Items
  move verbatim; only their file home and module wiring change.

## Constraints (load-bearing)

1. **Sibling-test layout (ADR-0004, `scripts/check-test-layout.sh`).** Every
   `crates/*/src/**/X_test.rs` MUST have a sibling `X.rs` carrying an active
   `#[cfg(test)] #[path = "X_test.rs"] mod tests;`. No inline
   `#[cfg(test)] mod tests { ... }` in `src/`. Consequence: a directory module
   uses `foo/mod.rs` paired with `foo/mod_test.rs` (the established pattern in
   `scan/`, `remux/`, `audio/`). A child `foo/acquire.rs` with unit tests pairs
   with `foo/acquire_test.rs`.
2. **`super::*` resolution.** The existing monolithic `*_test.rs` files use
   `use super::*;` (remote_execution, commit) or named `super::{...}` imports
   (coordinator). When a file becomes `foo/mod.rs`, its test becomes
   `foo/mod_test.rs` and `super` resolves to `foo` (i.e. `foo/mod.rs`). Per
   ADR-0015, tests follow their items: a test that exercises items now living in
   `foo/child.rs` moves to `foo/child_test.rs` and resolves `super::*` against
   that child. Tests that drive the module through its public entry point (and
   so span several children) stay in `foo/mod_test.rs`; where such a kept test
   reaches a child's private item, `mod.rs` adds a narrow named
   `pub(crate) use child::item;` re-export — never a `use child::*;` glob (the
   glob trips pedantic `wildcard_imports` and is rejected in ADR-0015). Children
   reference each other by qualified path, matching `transcode/mod.rs`.
3. **Paused-time DB guard (ADR-0012, `just check-paused-time-db`).** A test file
   that references `SqlitePool`/`ControlPlane` must not call
   `tokio::time::pause`/`advance`. When splitting a test file, each resulting
   test file must independently satisfy this guard. (The current files already
   satisfy it; the split must not co-locate a paused-time test with a DB test in
   one file.)
4. **Quality gates (`just ci`).** `fmt-check`, `lint` (clippy `-D warnings`,
   pedantic on, unwrap/expect/panic denied), `check-test-layout`,
   `check-paused-time-db`, `check-payload-deny-unknown`, `test`, `doc`, `deny`,
   `audit` all green before every commit. Functions ≤100 lines, cyclomatic ≤8,
   ≤5 positional params, 100-char lines, absolute imports only (no `..`),
   Google-style docstrings on non-trivial public APIs.
5. **`impl ControlPlane` may span files.** Rust permits inherent-impl methods
   for the same type across multiple modules of the same crate. Moving a
   `ControlPlane` method to `foo/acquire.rs` requires only that `ControlPlane`
   and the types in the signature are in scope there; no method needs to become
   public to move. This is the mechanism that makes the split behavior-preserving.
6. **Bisect-friendly history.** Each module's decomposition is one or more small,
   independently-green commits. Never one giant commit. Each commit keeps
   `just ci` green.

## Decomposition plan (per module)

The seams below come from the structural map of each file. Line ranges in the
implementation plan are advisory; the binding rule is "one responsibility per
child file, items moved verbatim, public surface unchanged."

### A. `cases/execution/remote_execution.rs` → `remote_execution/`

Seams (audit-named: acquire / replay / recheck):

- `remote_execution/mod.rs` — DTOs (the `pub` input/outcome structs), the
  `ReplayRoute` trait + its impls, route constants, and narrow named `pub(crate) use child::item;`
  re-exports for the private items kept tests still reach (no globs). Holds the shared
  idempotency/replay glue (`finish_replay_in_tx`, `decode_*`, `replay_error`,
  suppression-key helpers) and shared validation
  (`is_remote_replayable_error`, conversion helpers) that every entry point uses.
- `remote_execution/heartbeat.rs` — node heartbeat + lease heartbeat
  (`remote_node_heartbeat`, `remote_lease_heartbeat` + their `*_in_tx`).
- `remote_execution/acquire.rs` — `remote_acquire` + preflight/selected/leased
  `*_in_tx`, candidate scoring (`score_remote_candidates`,
  `aggregate_score_decision`, `candidate_from_ticket`, selected-candidate
  helpers), capacity recheck (`recheck_selected_remote_capacity_in_tx`,
  `capacity_no_candidate_in_tx`, active-lease-count helpers,
  `max_parallel_for_worker_operation_in_tx`), decision building
  (`decision_from_score`, `capacity_decision`, `scheduler_reason`,
  `scheduler_summary`, operation-set helpers), and artifact-plan building for
  dispatch (`artifact_plan_input`, `artifact_handles`, `remote_plan`,
  `artifact_access_mode_from_scheduler`).
- `remote_execution/complete.rs` — `remote_complete` + `*_in_tx`,
  `complete_remote_ok_in_tx`/`complete_remote_error_in_tx`, complete-evidence
  validation (`validated_artifact_complete_evidence`, `string_array_evidence`,
  `artifact_failure_status`).
- `remote_execution/fail.rs` — `remote_fail` + `*_in_tx`.
- `remote_execution/recover.rs` — `remote_recover`, node-validation helpers
  (`verify_remote_node_token_in_tx`, `validate_remote_node_live`,
  `require_remote_worker`, `require_positive_ttl`).

Test placement: keep the existing monolithic test as `remote_execution/mod_test.rs`
initially (paired with `mod.rs`), then split per-child where it reduces a child
test below the focus threshold and each split test still resolves its symbols and
satisfies the paused-time guard. Splitting tests is encouraged but bounded by
keeping every commit green; a child test file is only created when its source
child carries the items it exercises.

### B. `workflow/coordinator.rs` → `coordinator/`

Seams (audit-named: coordinator vs executor vs promotion):

- `coordinator/mod.rs` — public `CoordinatorOutcome`/`CoordinatorError`, the
  barrier entry points (`run_phase_barrier`, `resume_phase_barrier` + job
  lifecycle helpers `with_phase_barrier_job`, `*_in_job`, `phase_barrier_run_inputs`),
  the `PhaseLoop` state machine and its types, `drive_phase_loop`,
  `dispatch_phase`, and the named exports the coordinator test imports (`active_version_with_snapshot`,
  `project_media_snapshot_input`).
- `coordinator/planning.rs` — phase planning/policy projection
  (`classify_phase`, `phase_outcome`, `reject_unhandled_on_error`, `phase_draft`,
  `regenerate_phase_report`, `initial_phase_files`), report/summary aggregation
  (`job_grain_summary`, `zero_phase_summary`, `per_operation_json`).
- `coordinator/promotion.rs` — terminal-artifact placement
  (`promote_terminal_artifacts`, `promote_artifact`, `promotion_location_ids`,
  `asset_source_path`, `resolve_promotion_dirs`, `longest_common_dir`,
  `move_terminal_artifact`, `ensure_output_dir`, `ensure_unique_active_branch_ids`).
- `coordinator/finalize.rs` — per-file/per-phase row writing
  (`finalize_phase`, `finalize_failed_phase`, `finalize_succeeded_run`,
  `finalize_file`, `finalize_zero_phase_run`, `write_file_row`,
  `ticket_ids_for_node`, `ticket_result_location_ids`, `working_dir_artifacts`,
  `ProducedRefs` + impl), and the small payload/sqlite helpers
  (`first_stream_of_kind`, `payload_str`, `payload_u32`, `sqlite_u64`,
  `sqlite_i64`, `phase_ordinal`) re-exported through `mod.rs`.
- `coordinator/resume.rs` — resume reconciliation (`reconcile_resume`,
  `active_branch_ids`, `file_branch_path`, `active_version_with_snapshot`,
  `project_media_snapshot_input`).

Test placement: existing test imports two named functions and constructs some
private fixtures; keep it as `coordinator/mod_test.rs` with `mod.rs` re-exporting
the named items. Split per-child where clean.

### C. `workflow/execution/executor.rs` → `executor/`

Seams:

- `executor/mod.rs` — `WorkflowExecutor`, the options structs and their
  `Default`/builder impls (config), the public/`pub(crate)` re-exports the
  `workflow::execution` module forwards (`WorkflowExecutor`,
  `WorkflowExecutorOptions`, `WorkflowChaosOptions`), `RunLoopState` +
  `run_plan_in_job`/`submit_and_run`. Children referenced by qualified path.
- `executor/config.rs` — `WorkflowTimingOptions`/`WorkflowQueueOptions`/
  `WorkflowArtifactRoots`/`OperationArtifactRoots`/`WorkflowDispatchOptions`/
  `WorkflowStreamOptions` + their impls and `WORKFLOW_JOB_KIND`.
- `executor/tickets.rs` — root/node ticket creation (`create_root_tickets`,
  `create_node_ticket`), payload rendering (`render_root_payload`,
  `render_root_remux_payload`, `root_payload_result`, `root_payload_error`),
  ticket helpers (`ticket_kind`, `parse_payload`, `depends_on_node`,
  `all_dependencies_succeeded`), and `resolve_policy_file_source`.
- `executor/dispatch.rs` — spawn/join (`try_spawn_dispatch`,
  `process_joined_dispatch`, `SpawnOutcome`), worker selection
  (`candidate_workers` + reservation/capacity helpers
  `increment_reservation`, `decrement_reservation`, `local_reservation_blocks`,
  `json_string_array_contains`, `max_parallel_for_operation`).
- `executor/expansion.rs` — post-success expansion (`expand_successful_ticket`,
  `expand_policy_node_completion`, `succeeded_node_ids`) and state queries
  (`node_ticket_exists`, `ready_workflow_tickets`, `workflow_finished`).
- `executor/errors.rs` — failure classification/retry (`first_failed_ticket_error`,
  `ticket_failure_class`, `selector_failure_class`, `retry_delay`,
  `WorkflowRunError`) and sqlite/time helpers (`format_time`, `sqlite_i64/u64/u32`).

Note: there is already a sibling `workflow/execution/dispatch.rs`. To avoid a
name clash with `executor/dispatch.rs`, the executor's dispatch child is named
`executor/spawn.rs` (a directory module is its own namespace, so the clash is
only a readability concern; we use a distinct name for clarity).

Test placement: the 3,114-line `executor_test.rs` is the largest. It imports
named items and drives via the public surface. Keep it as `executor/mod_test.rs`
initially; split into `executor/<child>_test.rs` opportunistically where the
test partitions cleanly along the source seams and each split keeps green.

### D. `artifact/commit.rs` → `commit/`

Seams:

- `commit/mod.rs` — public DTOs/errors (`CommitArtifactInput`,
  `CommitArtifactReport`, `CommitRecoveryReport`, `CommitArtifactPreMutationReport`,
  `CommitArtifactCommandError`), the `commit_artifact`/`recover_commit`
  `ControlPlane` entry points, the `CommitArtifactHooks` trait + context structs
  + `commit_artifact_with_hooks`. Children referenced by qualified path. (The
  `artifact/mod.rs` `pub use commit::{...}` re-export must keep resolving the
  same names from `commit/mod.rs`.)
- `commit/prepare.rs` — `prepare_commit`, `prepare_commit_in_tx`,
  `read_commit_source_facts`, `read_verified_staging_facts`,
  `prepare_commit_paths`, `PreparedCommit`/`CommitSourceFacts`/
  `VerifiedStagingFacts`/`CommitPreparedPaths`/`PrepareCommitError`.
- `commit/pre_mutation.rs` — `read_handle_facts_in_tx`,
  `live_staging_location_in_tx`, `require_expected_facts`, `pre_mutation`,
  `append_failed_pre_mutation`, `pre_mutation_error`, `PreMutationContext`/
  `HandleFacts`/`LiveStagingLocation`.
- `commit/promote.rs` — `promote_prepared`, `install_temp_no_replace`,
  `fsync_parent_dir`, `remove_file_if_exists`, `PromotionOutcome`.
- `commit/finalize.rs` — `finalize_commit`, `update_commit_report_in_tx`,
  `report_from_record`.
- `commit/recovery.rs` — `recover_commit_inner`, `recovery_read_error`,
  `transition_recovery`, `observe_recovery`, `same_file_facts`,
  `recovery_reason`, `failure_class_for_error`, `path_exists`.

Test placement: `commit_test.rs` uses `use super::*;` and reaches private items;
keep as `commit/mod_test.rs` with `mod.rs` re-importing children. Split where clean.

## Success criteria (falsifiable)

1. None of the four target source files exceeds ~600 lines after the split, and
   no newly created child source file exceeds ~600 lines. (Measured by
   `wc -l`; the threshold is a guideline, not a gate — the binding rule is one
   responsibility per file.)
2. `git diff BASE -- <module>` shows items moved verbatim: no change to any
   function body, signature, SQL string, event name, or error code. Verified by
   diffing moved bodies (e.g. `git show` of the move commit is rename+move, not
   rewrite).
3. The public surface is byte-identical: `cargo public-api`-style check is not
   available, so instead the set of `pub`/`pub(crate)` items re-exported from
   `artifact/mod.rs`, `workflow/mod.rs`, `workflow/execution/mod.rs`, and the
   four module roots is unchanged (verified by grepping the re-export lists
   before/after and by the fact that no caller outside the split module needed
   editing).
4. `just ci` is green at every commit (fmt, clippy `-D warnings`,
   check-test-layout, check-paused-time-db, check-payload-deny-unknown, test,
   doc, deny, audit).
5. Every existing test still runs (none silently dropped): the count of `#[test]`
   / `#[tokio::test]` functions across the module's test files is unchanged
   before/after, and `cargo test -p voom-control-plane` reports the same passing
   count for the affected modules.
6. `check-test-layout` passes: every `*_test.rs` has its sibling `*.rs` with a
   `#[path]` decl; no inline test modules introduced.

## Edge cases / failure modes

- **`super::*` symbol loss.** If a kept `mod_test.rs` references a private item
  that moved into a child and `mod.rs` does not re-export it, the test fails to
  compile. Mitigation: move the test beside the child that owns the item
  (preferred), or add a narrow named `pub(crate) use child::item;` re-export to
  `mod.rs`. Never a `use child::*;` glob.
- **Private-item visibility across children.** A private free function in
  `acquire.rs` used by `complete.rs` must be `pub(super)` (or `pub(crate)`), or
  live in `mod.rs`. The plan puts genuinely shared helpers in `mod.rs`;
  child-to-child use is made `pub(super)` with the minimum visibility that
  compiles, never wider than `pub(crate)`.
- **Name clash with existing sibling `dispatch.rs`.** Handled by naming the
  executor child `spawn.rs`.
- **`check-paused-time-db` co-location.** When splitting a test, never place a
  `tokio::time::pause` test in the same file as a `ControlPlane`/`SqlitePool`
  test. Verified by `just check-paused-time-db` per commit.
- **Doc-comment movement.** Module-level `//!` docs and item `///` docs move
  with their items; `mod.rs` gets a `//!` summarizing the module's
  responsibility map so `just doc` stays meaningful.
- **Clippy line/complexity limits triggered by re-wrapping.** Items move
  verbatim, so per-function complexity is unchanged. `mod.rs` uses named
  re-exports and children use qualified paths, so the pedantic `wildcard_imports`
  lint never fires from this work.
