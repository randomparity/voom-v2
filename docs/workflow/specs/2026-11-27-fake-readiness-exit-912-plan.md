# Fake readiness/exit implementation plan (#912)

Expected implementation size: 60–120 changed lines in two sibling Rust files.
Scope: q912-ff4ab7a9; full-spec; M denominator 250 from live #912 bounded lifecycle assessment. No ADR/migration/shared-file changes.

## Task 1 — Restore readiness-before-exit ordering

Contract C1/C2/C4: the bounded announcement read determines readiness even after natural exit; shutdown remains interruptible.
- focused-test: deterministic already-exited child plus one-poll-pending readiness reader; old code must return Exited and fail the readiness assertion.
- Implement: remove premature child.wait branch and obsolete exit outcome; simplify the private function signature and caller.
- Validate: valid status remains17; existing missing/oversized/shutdown tests remain green; add malformed/non-loopback checks if absent.

## Task 2 — Exercise immediate exit and cancellation

Contract C3: cancellation after tombstone storage preserves exactly one unwaited natural status.
- focused-test: caller_cancellation_after_natural_exit_preserves_unwaited_status and natural_exit_remains_observable_until_delayed_wait.
- Implement: remove READY_EXIT's25ms sleep; retain explicit TombstoneStored synchronization.
- Validate: process_supervisor unit suite serial and parallel, targeted repetition. No timing retries or new sleeps.

## Integration and handoff

Run `just ci` with isolated Cargo target, honoring mandatory hooks. Adversarial whole-branch review uses the design Failure model and oathbind surface. Security review triggers only if actual diff changes parsing/security behavior. Run simplification review, publish PR with #912 closure and complete review disclosures, verify exact-head CI/mergeability, then record MERGE-READY without merging or cleanup.
