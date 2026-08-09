# ADR 0058: Observe node-agent lifecycle ordering through production paths

## Status

Accepted

## Context

Two node-agent unit tests claim to verify lifecycle ordering but append `restart` or `reap`
to their own event log only after awaiting lease settlement. Those assertions cannot fail
when production reorders settlement, child restart, child reap, or deactivation. The same
runtime module has no direct coverage for restart-budget exhaustion or the mapping from a
joined worker coordinator to the top-level runtime exit.

The worker protocol already provides the real authenticated HTTP server used by workers.
Runtime tests can pair that server with a supervised process fixture and drive the actual
child supervisor, coordinator, lease settlement, restart, reap, and deactivation paths
without adding a production interface whose only consumer is a test.

## Decision

Lifecycle-ordering tests launch a small temporary shell child that records each process
start, atomically exports its supervisor-provided secret beneath a mode-private temporary
directory, prints the production readiness line, and waits on the supervisor's parent-death pipe. An
in-process `HttpServer`, configured with that exact credential, supplies a deliberately
pending operation through the real authenticated worker protocol. Tests use the fake
control-plane boundary to hold lease failure acknowledgement or deactivation and observe
which production stage has and has not completed.

The handoff uses `umask 077`, a newline-terminated temporary file, and atomic rename. The
test combines the captured secret with the worker ID and epoch returned by its fake
activation. The endpoint is published with the same complete-file protocol. Fixture drop
guards kill an owned child and abort an owned server task after an assertion failure.

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
tests because they launch worker processes and bind a loopback server, but they remain
focused unit-module tests.

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
