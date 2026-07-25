# Issue #330 implementation plan

Base: `main` at `a4e844aceea42cabfd73f5ff9b33cfff56373262`

## 1. Type the published compiled gate

Files:

- `crates/voom-policy/src/compile/compiled.rs`
- `crates/voom-policy/src/compile/lower/phases.rs`
- validator and sibling tests
- `crates/voom-plan/src/eligibility.rs`, planner, and sibling tests
- published compiled control-flow fixture

Behavior:

- replace `CompiledPhase.run_if: Option<CompiledCondition>` with typed
  `CompiledRunIf`;
- preserve the existing predicate JSON representation;
- require exactly one published trigger and reference;
- reject non-predecessors against the computed topological `phase_order` during
  lowering;
- keep history-free preview planning fail-closed while allowing the coordinator
  to clear only a gate it has already resolved.

TDD red:

- typed lowering and serde compatibility tests fail against the opaque
  predicate;
- non-predecessor and extra-token forms compile when they should fail.

Verify:

- `cargo test -p voom-policy run_if`
- `cargo test -p voom-control-plane --all-features --test published_grammar_corpus`

Commit: `feat(policy): type published phase run gates`

## 2. Persist inherited per-file phase history

Files:

- `migrations/0023_workflow_file_run_history.sql`
- `voom-store` migrator, workflow summary repository, and sibling tests
- migration inventory/schema tests

Behavior:

- add the strict job/branch/ordinal outcome table;
- atomically insert and read inherited history;
- accept only committed/skipped outcomes and reject duplicate or orphan rows.

TDD red:

- schema and repository round-trip tests fail before migration/repository
  support exists;
- invalid outcome and partial-batch tests prove rollback.

Verify:

- `cargo test -p voom-store file_run_history`
- `cargo clippy -p voom-store --all-targets --all-features -- -D warnings`

Commit: `feat(store): persist inherited phase gate history`

## 3. Evaluate gates per file and carry history through resume

Files:

- `workflow/coordinator/mod.rs`
- `workflow/coordinator/planning.rs`
- `workflow/coordinator/resume.rs`
- `workflow/coordinator/finalize.rs`
- coordinator sibling and phase-barrier integration tests

Behavior:

- attach phase history to each `PhaseFile`;
- update it from finalized rows;
- filter each gated planning draft per file and clear only the resolved gate;
- combine/copy inherited history during resume job creation;
- fail closed on missing or inconsistent state.

TDD red:

- mixed-file `modified` gate admits both files;
- `completed` does not distinguish a successful skip from missing state;
- repeated resume loses the predecessor outcome.

Verify:

- `cargo test -p voom-control-plane --lib run_if`
- `cargo test -p voom-control-plane --lib resume_phase_history`
- `cargo test -p voom-control-plane --all-features --test phase_barrier_flow`

Commit: `feat(policy): evaluate phase run gates per file`

## 4. Final verification and shipping

- update S08/S09 corpus oracles only where implemented behavior changes;
- run focused policy/store/coordinator tests;
- run adversarial review once, fix only material findings;
- fetch/rebase onto current `origin/main`;
- run `just ci`;
- push, open a PR with `Closes #330`, wait for required checks, and merge
  serially before starting #335.
