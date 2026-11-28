# Fake provider readiness before exit (#912)

## Problem and authority

Issue #912 reports cancellation coverage failing when READY_EXIT exits before its readiness read wins a select. The accepted [subprocess design](2026-08-29-real-subprocess-crash-recovery-design.md#child-supervisor) already orders a bounded frame read before waiting and requires retained exactly-once status. The supervisor violates that contract by concurrently selecting the initial read and exit. The helper's 25 ms sleep hides that race.

## Scope and design

Restore the existing contract in `process_supervisor.rs`: race the bounded readiness read only against supervisor shutdown. Remove the premature exit branch and its obsolete outcome. Readiness acknowledges an announcement, not continuing process liveness. The existing watcher then owns exit observation and the actor retains unwaited status. No caller migration or new ADR decision is needed; ownership stays in the watcher. `remote_runner.rs` remains a consumer of readiness followed by dispatch and wait.

A biased select still permits exit to win while the pipe read is pending. Draining after the exit branch could also honor readiness but retains a second exit path unnecessarily. Reading the bounded frame first follows the accepted design with less code. An exited helper without a frame closes its pipe and fails the existing newline check. A pipe retained elsewhere can consume the existing five-second deadline; no deadline is widened.

## Success

- C1: An exited helper with a complete valid announcement is acknowledged, and its code 17 remains observable (issue acceptance; existing child-supervisor contract).
- C2: A regression forces exit completion before readiness finishes, without retries or scheduling delays (issue acceptance).
- C3: The cancellation test waits for the existing tombstone milestone, then aborts its caller and recovers exactly one unwaited status on shutdown (issue acceptance).
- C4: Missing, oversized, malformed or non-loopback readiness still fails; readiness remains bounded and supervisor shutdown can interrupt it (existing contract preservation).

## Failure model

- Actors and deployments: local fake-provider harness callers, controlled worker subprocesses, local developer and CI tests.
- Invariants and assets: readiness precedes exit classification; sole watcher retains child ownership through reap; unwaited status survives cancellation; bounded frame read and responsive shutdown.
- Accepted failure classes: host/runtime destruction can prevent explicit reap, as already documented by ADR 0095; resource exhaustion can return existing process/read errors. An inherited readiness pipe can consume the existing bounded deadline.
- Covered elsewhere: ADR 0095 and existing supervisor tests govern kill escalation, registry cleanup and cancellation before readiness; worker protocol owns dispatch/authentication. No public protocol or persisted contract changes.

## Validation

Force an already-reaped child and a reader that first returns Pending before delivering its readiness frame; this must fail on the old exit race and pass after correction. Remove the helper sleep so normal lifecycle tests exercise immediate exit. Cover valid readiness/status plus invalid-frame and shutdown behavior through existing tests and focused additions where coverage is missing. Run process-supervisor tests with serial and ordinary parallel execution, repeated targeted cancellation, then `just ci`, mandatory commit hooks and remote CI.
