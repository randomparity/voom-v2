# Abandon timed-out media evidence workers

## Problem

Issue #835: a socket timeout leaves a lease potentially running while later files reuse its
worker. CPU, RSS and I/O then look like clean measurements. Parent #787 requires truthful
failure and resource accounting. Scope is the approved q835-73094899 charter; exclusions are empty.

## Success

C1. Within one Worker lifetime, after timeout is recorded under its existing lock, later
call admissions return `worker_abandoned` without constructing an HTTP connection or a lease.
C2. `usage()` returns the existing four metric keys with `unavailable` values after timeout.
C3. Calls admitted before abandonment keep their actual success or failure; their completion
cannot clear abandonment. Concurrent requests remain concurrent, and `stop()` retains its
30-second graceful wait and kill fallback. A new repetition starts fresh Worker instances.
C4. The stalled socket fixture closes accepted sockets and joins its listener thread.

## Design

Extend the existing Worker owner with one lock-protected boolean, initialized false.
`call()` checks it in the same critical section that allocates a lease. Its TimeoutError
handler marks it true under that lock before returning `transport_timeout`. No other error
abandons the worker. HTTP work remains outside the lock, so a preadmitted call can still be
sent or finish after another call times out; it retains its actual result. This is admission
fencing, not cancellation of existing concurrent calls.

`usage()` initializes its current metric dictionary, checks abandonment under the same lock,
and returns unavailable values immediately when set. `run_repetition` already joins its call
pool before reading usage, so its record cannot race an active timeout. Keeping this decision
inside Worker also protects direct callers. No owner transition or caller migration is needed.
Lazy/fingerprint/reuse import baseline helpers but do not instantiate Worker; rollout uses
Worker for isolated correctness cases and already consumes terminal frames. No consumer
requires a record-shape change. See [ADR 0126](../../adr/0126-abandoned-media-evidence-workers.md).

## Global Constraints

Use Python standard library and existing Worker lock; add no dependency or toolchain floor.
Preserve existing measurement keys, failure privacy, process cleanup and concurrent dispatch.
Change only the baseline harness, its selftests, this spec, and ADR 0126 plus its index row.
Plans remain transient and are not committed. Production scheduling and trust stay unchanged.

## Failure model

- Actors and deployments: local operators and CI running the baseline replay or its importing
  experiment scripts against local authenticated worker processes.
- Invariants and assets: no post-abandonment call admission dispatches; timeout-contaminated
  per-worker metrics are unavailable; existing in-flight results remain truthful; fixture
  sockets and owned threads are cleaned up.
- Accepted failure classes: preadmitted calls may dispatch or complete after abandonment,
  since canceling them would change concurrent semantics; partial reads and host-wide counters
  retain their existing meanings and are not attributed as clean per-worker measurements.
- Covered elsewhere: #339 owns flagship acceptance; #823 owns unrelated child-I/O description;
  existing Worker.stop owns graceful close and kill fallback; no production protocol changes.

## Validation

Use focused red/green timeout, repetition, queued and in-flight tests in the baseline selftest.
A real local listener accepts without replying; assert only one request, timeout then abandonment,
metric unavailability and named hash/probe row failures. Mock transport only for deterministic
thread interleavings, with events and bounded joins. Check new repetition state by constructing
healthy actual workers. Run baseline selftests with tracemalloc to confirm fixture cleanup;
run importing-consumer selftests, selected hooks, then final `just ci` before push.
