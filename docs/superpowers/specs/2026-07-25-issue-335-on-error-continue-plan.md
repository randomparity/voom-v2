# Issue 335 implementation plan

## 1. Preserve the job for isolatable ticket failures

- Files: `workflow/execution/executor/{mod.rs,spawn.rs}` and sibling tests.
- Add an internal failure-handling mode and a coordinator-facing entry point
  that continues independent branches after durable ticket failure.
- Give every phase a unique workflow invocation id and scope ready, retry,
  completion, and failed-ticket queries to it. Preserve job-wide durable
  counts and add coordinator accumulation for invocation-local telemetry.
- Keep validation, database, join, and inconsistent-state failures fatal.
- First write tests that fail because an undispatched sibling is abandoned and
  the shared job is marked failed, plus a two-invocation test that exposes a
  prior phase's failed ticket poisoning the next phase.
- Verify:
  `cargo test -p voom-control-plane workflow::execution::executor --features test`.
- Commit: `feat(workflow): isolate terminal ticket failures`.

## 2. Finalize a continued phase per file

- Files: `workflow/coordinator/{mod.rs,finalize.rs,planning.rs}` and sibling
  tests.
- Replace the blanket continue rejection with effective-strategy resolution;
  keep compiled `skip` rejected before job open.
- Query planned-node terminal ticket states, classify failed files as blocked,
  finalize the phase/report, remove failed files, and retain the first error.
- First write mixed-file and all-fail tests that fail under the current
  immediate-abort path.
- Verify:
  `cargo test -p voom-control-plane workflow::coordinator --features test`.
- Commit: `feat(policy): continue after per-file phase failures`.

## 3. Finish honestly and restrict promotion

- Files: `workflow/coordinator/{mod.rs,promotion.rs}` and sibling tests.
- Promote only artifacts belonging to survivor assets. When a continued error
  exists, persist cumulative summary data, fail the job, and return every
  completed phase/file row in the partial outcome.
- First write tests that expose false success and promotion of a failed file's
  earlier artifact.
- Verify:
  `cargo test -p voom-control-plane workflow::coordinator --features test`.
- Commit: `fix(policy): report continued runs as partial failures`.

## 4. Pin resume and published compatibility

- Files: coordinator resume tests and published grammar corpus documentation.
- Prove blocked files never re-enter, survivors retain history across repeated
  resume, config defaults and phase overrides select the correct behavior, and
  compiled `skip` remains readable but rejected.
- Verify:
  `cargo test -p voom-policy`,
  `cargo test -p voom-control-plane --test published_grammar_corpus --features test`,
  and focused coordinator resume tests.
- Commit: `test(policy): pin continued failure and resume semantics`.

## 5. Review and ship

- Review the complete diff once against the charter. Fix only defensible
  in-scope findings; track independent gaps under #325.
- Rebase onto current `main`.
- Run focused tests and `just ci` after rebase.
- Push, open a PR closing #335, wait for required checks, and merge only green.
