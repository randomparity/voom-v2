# Restore abandoned ffprobe fixtures

Goal: recover process-interrupted fixture changes without deleting foreign siblings.
Architecture: worker.rs retains ownership and uses a deterministic recovery directory
under its existing lock. Unix hard-link identity distinguishes owned replacement files; originals use rename.
Stack: Rust std filesystem APIs and tempfile subprocess tests.
Expected implementation size: 510–550 changed lines (M) — existing 480 plus private-stage and legacy-coexistence regressions.

Sizing: existing lifecycle241 and tests239 lines; builder setting about8–12,
permission regression about20–45, and legacy preservation about6–10. Reuse the
existing fixture owner and child-test pattern. The frozen denominator remains250.

## Global Constraints
Keep public helper signatures and the trusted profile-parent lock path unchanged.
Use the current Rust toolchain and std filesystem APIs; add no dependencies.
Linux and macOS CI are the named supported deployments. Preserve ordinary
cleanup on other hosts; refuse occupied-sibling recovery without proven identity.

## File map
- crates/voom-test-support/src/worker.rs: extend current fixture owner with locked
  recovery, remove PID-specific backup construction, retain public helpers.
- crates/voom-test-support/src/worker_test.rs: temporary-fixture recovery tests and
  explicitly invoked subprocess helper; never mutate the active target sibling.
- Existing three control-plane callers keep their signatures and behavior.

## Task: recover fixture state
Interfaces: public hide_stale_fake_ffprobe_sibling(&str) and
install_fake_ffprobe_sibling(&Path, &str) still return io::Result<FfprobeSiblingGuard>.
Private path-parameterized setup/recovery permits isolated tests. TargetDebugLock
retains its existing contract. No later task consumes new public APIs.

Verification inventory (each Mode: focused-test):
- Normal cleanup and retry: temporary original/replacement bytes; red evidence is
  missing deterministic recovery or original still hidden; green command below.
- Aborted ownership: kill child holding a guard, reacquire and restore original;
  on Unix red evidence is abandoned replacement surviving or missing original;
  elsewhere assert safe refusal/preservation for unavailable identity.
- Foreign-file distinction: replace sibling inode and assert recovery errors while
  preserving foreign bytes and backup; red evidence is deletion/overwrite.
- Delayed writer: keep a prior invocation writer blocked before open, retry with
  a unique staging path, then release it and assert the published replacement
  remains intact; red evidence is truncation of the new fixture.
- Partial setup and cleanup: construct empty/staged/original/restored states and
  retry recovery twice, including both lookalike histories, witness-retirement
  cut and relative symlink original; red evidence is conflict or stranded original.
- Legacy and malformed directory: explicit diagnostic, no mutation of legacy or
  foreign paths; red evidence is successful acquisition or overwritten files.

Steps:
- Staging permissions: child process with umask 0000 creates a stage and asserts
  group/other bits are zero immediately; red is0777, green is0700.
- Setup/rollback double failure: retain initial error kind, disclose rollback
  and journal path, then clear injected fault and recover on retry; red evidence
  is masked rollback failure or missing original.

1. Add temp-root tests reproducing interrupted state before implementation; run
   `cargo test -p voom-test-support worker::tests::fixture` and retain red output.
2. Implement deterministic recovery under lock, hard-link publication and Unix
   identity checks on Unix; elsewhere use a pending marker and rename publication
   without hard-link requirements. Set Unix staging mode 0700 at creation using
   existing tempfile Builder.permissions. Use unique child-writer staging, original rename, and retire
   replacement before restoring original; retain normal live-guard cleanup elsewhere. Remove obsolete
   hidden-path/counter logic. Error cleanup preserves recoverable journal state.
3. Run the focused command above: expected all selected tests pass. Run one real
   killed-child recovery test and existing lock tests; no ignored helper is run
   as an ordinary test.
4. Run `just fmt`, `just ci`; expected exit 0. Commit without bypassing hooks.
5. Whole-branch adversarial review, security path review, simplification, delivery
   and exact-head CI follow the quest workflow before merge-ready handoff.

Rollback: revert the two implementation files; resolve retained fixture recovery
state manually while test processes are stopped. No production state is changed.
Deferrals: none. BASE_BRANCH main; branch fix/501-interrupted-fixture-recovery.
