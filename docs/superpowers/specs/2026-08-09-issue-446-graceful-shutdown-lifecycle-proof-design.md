# Issue 446: Graceful-shutdown lifecycle proof

Decision: [ADR 0066](../../adr/0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md)

## Scope and authority

Issue #446 requires the black-box node-agent lifecycle test to address the wait that
actually expired: the second agent's graceful-shutdown join. The campaign criteria require
the proof to retain the first agent's fencing assertions, observe the second incarnation as
`Retired` with `GracefulShutdown`, observe orderly worker retirement, keep a path-specific
hang guard without increasing it, fail when graceful shutdown never terminates, and pass
repeated CPU-contended runs plus the repository guardrails.

PR #454 already gave the timeout a name and corrected its diagnostic from the first agent
to the second. PR #460 then bounded the production control-plane client's retry policy,
removing the unlimited retry that could keep deactivation pending forever. This change does
not alter that production behavior. It strengthens the black-box witness so task exit and
the durable terminal state are one bounded lifecycle assertion, and it verifies the landed
fix under the load shape that exposed the defect.

The implementation surface is `crates/voom-node-agent/tests/lifecycle.rs`, this spec, ADR
0066, its CI-coupled index row, and the implementation plan. It does not change a public
API, persisted schema, migration, authentication, dependency, or worker-protocol contract.
Migration 0037 remains unused.

## Approaches considered

### Selected: one guarded lifecycle witness

After requesting the second agent's graceful shutdown, await both its task termination and
the control plane's `Retired` incarnation state inside one `HANG_GUARD`. The terminal-state
poll remains a real control-plane read and the join remains the production runtime future;
neither is replaced by a test-only observer. When the guard expires, report that the
second-agent graceful-shutdown lifecycle did not complete and include which witness was
still missing plus a bounded snapshot of incarnation state and observed request paths.

The existing first-agent fencing join and all of its supersession assertions remain
unchanged. After the combined second-agent witness completes, retain the explicit
`GracefulShutdown`, superseded prior incarnation, and retired-worker assertions.

### Rejected: increase or retry the timeout

The observed failures consumed 10-, 60-, and 150-second bounds exactly while successful
runs finished in 6–9 seconds. A larger or retried timeout delays a hang report and cannot
prove progress.

### Rejected: add a production lifecycle hook

A callback or injectable shutdown observer would make the test deterministic but would
prove the hook rather than the real task, HTTP control-plane boundary, durable incarnation
transition, and supervised-worker retirement. It would also add a single-use production
abstraction.

### Rejected: poll durable state without joining the task

`Retired` is necessary but not sufficient: code after deactivation could still prevent the
runtime future from completing. Dropping the join would miss the exact failure reported by
#446.

## Lifecycle witness

The helper owns a mutable reference to the second agent's `JoinHandle`, the control plane,
the node ID, and the request recorder. Within one timeout it drives two futures:

1. the second agent join, which must yield a successful runtime result; and
2. the existing incarnation poll, which must observe the newest incarnation as `Retired`.

The helper returns the terminal incarnation history for the existing reason assertions.
The worker query remains after the helper and proves both workers were retired. The test
therefore continues to cover the ordered production path: graceful request, coordinator
settlement and child reap, deactivation, durable retirement, runtime exit.

Timeout diagnostics must not create a second unbounded wait. The helper records progress
as each witness completes. On expiry it reports the missing witness and the request paths
already held by the fixture. Any optional database snapshot is itself bounded and falls
back to an explicit unavailable marker.

## Regression and contention proof

The regression proof has two arms:

- Send the second shutdown signal, then temporarily make the production runtime remain
  pending after successful deactivation has recorded durable retirement but before
  `run_until` returns. Use a short local guard. The focused lifecycle test must fail with
  the new diagnostic identifying durable retirement as complete and the second-agent join
  as missing. Restore the source and guard, then rerun green.
- Record the host OS, architecture, logical CPU count, and Rust version. Build the lifecycle
  test binary once. Start twice as many CPU busy loops as logical CPUs, then run four waves
  of 16 concurrent copies of that exact binary with two test threads per copy. Stop and
  reap every load process after the fourth wave. All 64 test-process exit codes must be
  zero; preserve the exact command, host facts, elapsed time, and per-wave result counts in
  the work report.

Ordinary serial reruns do not satisfy the second arm. The contended run is verification
evidence rather than a default CI loop because oversubscribing every CI job would increase
suite cost and still could not guarantee a specific scheduler contention level.

## Failure handling

`HANG_GUARD` remains 30 seconds. The first-agent expiry continues to say that fencing did
not terminate the superseded agent. The second-agent expiry identifies graceful-shutdown
lifecycle completion and distinguishes a missing join from a missing durable retirement
witness. Join errors and runtime errors retain their existing direct test failures rather
than being converted into timeouts.

The helper performs no cleanup on success or failure. Existing fixture and task ownership
remain responsible for shutdown; a timed-out test process owns no durable state outside its
temporary SQLite fixture.

## Verification

Run the focused lifecycle test, the deliberate post-retirement non-termination bite, the
specified 4-by-16 concurrent-copy exercise under 2x CPU oversubscription,
`cargo test -p voom-node-agent --all-features`, and bare `just ci`. Report all ignored or
skipped tests; the known baseline is six environment-specific ignored tests (one ffmpeg
hardware test and five toxiproxy network-resilience tests), so any additional skip is a
regression to investigate.
