# Issue 447: Production-observed node-agent lifecycle test coverage

## Scope and authority

Issue #447 requires replacement of two node-agent ordering assertions that are constructed
by the tests themselves and coverage for the remaining runtime restart and coordinator-exit
paths. Current `main` has already added direct forced-reaping and shutdown-signal-phase
coverage through issue #449, so this change preserves those tests rather than duplicating
them. The campaign assigns ADR 0058. Repository guardrails require its matching index row.

This change does not alter node-agent runtime behavior, wire contracts, configuration,
credentials, persisted state, migrations, or issue #446's lifecycle hang bounds. Its
permitted production/test surface is `crates/voom-node-agent/src/runtime.rs` and
`crates/voom-node-agent/src/runtime_test.rs`; the selected approach needs only the test file.

## Success criteria

1. A test using the production coordinator proves that a crashed child is not restarted
   before every held lease reaches acknowledged terminal failure.
2. A test using the production runtime proves that graceful shutdown does not reap a child
   or begin deactivation while terminal lease settlement is blocked, and that the child is
   reaped before deactivation begins.
3. A restart that repeatedly fails child startup reaches `RestartExhausted` through the real
   `restart_child` loop.
4. Tests cover coordinator restart exhaustion, fatal exit, unexpected shutdown/absence, and
   join failure mappings without wildcard matches.
5. The obsolete tests named
   `child_crash_settles_all_held_leases_before_restart_can_begin` and
   `shutdown_orders_settlement_before_reap_and_deactivation` are removed rather than kept as
   duplicate helper coverage.
6. New regressions are shown to bite by temporarily breaking the relevant production order
   or mapping, observing the focused test fail, restoring the code, and rerunning green.
7. Focused node-agent tests and the repository's `just ci` guardrail pass with zero skipped
   tests on the development host.

## Approaches considered

### Selected: process-backed production-path tests

Use `voom_test_support::worker::cargo_bin_or_build` to locate the existing chaos-worker
binary. A temporary `/bin/sh` wrapper records its PID/start count and then `exec`s the real
worker. Because the child supervisor supplies credentials and the parent-death pipe through
environment and stdin, the wrapper does not emulate any worker protocol behavior.

The fake control plane remains the correct mock boundary: it is external, nondeterministic
in production, and already exposes gates for lease failure acknowledgement and deactivation.
Holding those gates lets the test observe actual coordinator progress without sleeps.

### Rejected: production injection seam

A restart trait, async callback, or test lifecycle observer would make ordering easy to
script. It would also add an abstraction with one production use, permit the test double to
diverge from `ChildSupervisor`, and weaken the evidence from process behavior to callback
order.

### Rejected: source-shape guard

An AST or text check could require one call to appear before another. It cannot prove that
the earlier future was awaited to completion or that the child process was reaped, so it
does not encode the runtime guarantee.

## Test fixture and observations

The Unix-only worker fixture owns a temporary wrapper and process-start log. Each launch
appends its shell PID and replaces the shell with chaos-worker, so that PID remains the
supervised worker PID. The fixture provides bounded helpers for start-count observation and
process liveness. Every spawned runtime receives an explicit shutdown signal and every gate
is released before test cleanup.

For crash ordering, the control plane grants one lease whose chaos payload exits the worker.
The failure acknowledgement gate holds settlement. The start log must remain at one entry
while the gate is held; after acknowledgement, production may restart and the log reaches
two. Acquisition switches to idle before release so the replacement stays alive for clean
shutdown.

For graceful ordering, a stall payload keeps one lease held. After the shutdown signal, the
failure acknowledgement gate proves settlement has begun. While held, the worker PID must
remain live and deactivation must not start. After acknowledgement, the deactivation gate
proves deactivation has begun; by then the worker PID must no longer be live. Releasing the
gate lets the runtime retire cleanly.

Restart exhaustion uses an always-failing `/bin/sh` child specification and the real
`restart_child` retry loop. Coordinator mapping tests construct actual `JoinSet` outcomes,
including cancellation for `JoinError`, and inspect the resulting `RuntimeExit` variant and
error classification explicitly.

## Failure handling and bounds

All observation waits use Tokio timeouts of five seconds per awaited process/runtime event,
measured by the host monotonic clock. Exceeding a bound fails that test and cleanup aborts or
signals the owned runtime before returning. The production restart delay remains unchanged;
tests do not pause Tokio time while real child processes are running.

The fixture creates no durable external state. Temporary files are owned by `TempDir`, child
processes retain the existing parent-death stdin contract, and the runtime's normal shutdown
path reaps them. No new dependency is added.

## Verification

Run the four new focused regressions first, then
`cargo test -p voom-node-agent --all-features`. For bite proof, invert the production
settlement/restart condition or one coordinator mapping one at a time, run the corresponding
focused test and record its failure, restore the source, and rerun it green. Before shipping,
run `just ci` and confirm hosted checks are green with the PR `CLEAN` and `MERGEABLE`.

ADR [0058](../../adr/0058-production-observed-node-agent-lifecycle-tests.md) records why the
tests exercise real child processes instead of introducing a production injection seam.
