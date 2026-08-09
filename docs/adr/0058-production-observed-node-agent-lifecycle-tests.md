# ADR 0058: Observe node-agent lifecycle ordering through production paths

## Status

Accepted

## Context

Two node-agent unit tests claim to verify lifecycle ordering but append `restart` or `reap`
to their own event log only after awaiting lease settlement. Those assertions cannot fail
when production reorders settlement, child restart, child reap, or deactivation. The same
runtime module has no direct coverage for restart-budget exhaustion or the mapping from a
joined worker coordinator to the top-level runtime exit.

The node agent already has a real chaos-worker binary and a test-support helper that locates
or builds workspace worker binaries. The runtime tests can therefore drive the actual child
supervisor, coordinator, lease settlement, restart, reap, and deactivation paths without
adding a production interface whose only consumer is a test.

## Decision

Lifecycle-ordering tests launch the existing chaos worker through a small temporary shell
wrapper. The wrapper records each process start and then replaces itself with the real
worker, preserving the exact credentials and parent-death pipe established by the child
supervisor. Tests use the fake control-plane boundary to hold lease failure acknowledgement
or deactivation and observe which production stage has and has not completed.

One crash-mode test proves that a replacement process is not started while terminal lease
settlement is blocked. One stall-mode test proves that shutdown does not reap the child or
begin deactivation while settlement is blocked, and that reap completes before deactivation
begins. Direct tests exercise restart-budget exhaustion and every `coordinator_exit`
classification. The obsolete self-constructed ordering tests are removed.

No production lifecycle interface, callback, observer, timing constant, or behavior changes.
No migration is required.

## Consequences

The ordering regressions fail on observable process, control-plane, and runtime behavior
instead of on an event sequence authored by the test. The tests are slower than pure helper
tests because they launch worker processes, but they remain focused unit-module tests and
reuse the same workspace binary support as the existing lifecycle integration suite.

The process-backed tests run only on Unix, where `/bin/sh` and process signalling provide
the worker wrapper and liveness probe. Existing helper-level tests continue to cover the
platform-neutral state transitions. Windows does not gain an equivalent process-ordering
proof in this change.

## Considered & rejected

- Add a restart trait, callback, or lifecycle observer to production code. This would make
  events easy to script, but it introduces a second implementation boundary solely for
  tests and risks proving the seam rather than the coordinator.
- Assert source order or inspect the syntax tree. That can detect a textual move but does
  not prove awaited settlement, child reap, or deactivation behavior under concurrency.
- Keep the existing tests and rename them as helper tests. Their settlement assertions are
  useful, but newer focused tests already cover those helpers; keeping the misleading
  ordering names preserves the original defect.
- Extend the end-to-end lifecycle suite. It is already broad and comparatively expensive;
  the fake control-plane gates needed for precise ordering belong beside the runtime's unit
  fixtures.
