# 0088 — Node-agent shutdown has a wall-clock deadline

## Status

Accepted (2026-08-28)

## Context

Issue #452. The node agent's shutdown tail calls the control plane twice and
waits for both calls without a wall-clock bound.

The tail runs in one order. Each worker coordinator settles its held leases —
`run_coordinator`'s `CoordinatorEvent::Shutdown` arm calls
`settle_leases_for_shutdown` (`crates/voom-node-agent/src/runtime.rs:744`), which
calls the control plane — and only then reaps its child with
`ChildSupervisor::shutdown_all` (`:746`). Once every coordinator has joined,
`finish_shutdown_lifecycle` deactivates the incarnation (`:319`), which is the
write that records `Retired` with `GracefulShutdown`.

Both control-plane waits are bounded only by the client's retry policy.
`production_request_budget` (`crates/voom-node-agent/src/client.rs:450`) puts one
logical call at five attempts of a 30 s `REQUEST_TIMEOUT` plus backoff ceilings —
about 154 s — and the tail makes one settlement call per held lease before it
makes the deactivation call at all.

The only escape is a **second** termination signal:
`ShutdownSignalPhase::signal_forces` consumes the first signal as the one that
began the shutdown, and forces on the next. `signal_phase_for_exit`
(`runtime.rs:1864`) starts a graceful exit already in `ForceEnabled`, so on that
path the next signal does force immediately.

An init system never sends that signal. It sends `SIGTERM` once, waits its stop
timeout, then sends `SIGKILL`. `SIGKILL` skips the deactivation write, which is
the exposure #452 names: an agent that does not exit on `SIGTERM` leaves its
incarnation un-retired.

Issue #592 removed the mechanism that made this reproduce in CI — a pooled
connection holding the `SQLite` write lock, [ADR
0087](0087-cancellation-safe-begin-immediate.md). It did not remove the unbounded
wait. Any unresponsive control plane still reaches it.

## Decision

**The shutdown tail's control-plane waits share one wall-clock deadline.**
`SHUTDOWN_DEADLINE` is 20 s, captured as a `tokio::time::Instant` once when the
tail begins and passed to both waits: `wait_for_coordinators` and
`deactivate_or_second_signal` each gain one `sleep_until` arm. Expiry does what a
second signal already does, so no new teardown path exists.

**Forcing goes through the `ShutdownKind::Forced` watch, not through aborting
coordinator tasks.** A coordinator reaps its child after settlement, so aborting
it at the deadline would leave the worker process running. The watch is observed
by `wait_or_force` inside settlement, which abandons the control-plane wait, and
the reap then runs to completion inside `shutdown_grace_seconds`.

**`ShutdownProgress.forced` becomes `Option<ShutdownForce>`** — `Signal` or
`Deadline` — so the exit error names the cause. Reporting a deadline expiry as
"interrupted by a termination signal" would send an operator looking for a signal
nobody sent.

The startup-failure deactivations take the same bound; they call the same
`deactivate_or_second_signal` and were unbounded for the same reason.

Nothing new bounds the child reap. It is bounded by `shutdown_grace_seconds`
already and was never the part that hung.

## Consequences

Graceful shutdown becomes finite and computable:
`SHUTDOWN_DEADLINE + shutdown_grace_seconds`. The configuration validator holds
`shutdown_grace_seconds` to 1..=60 (`crates/voom-node-agent/src/config.rs:159`),
so the worst case is 80 s and the common case — the runbook's example
`shutdown_grace_seconds = 10` — is 30 s.

80 s fits systemd's upstream 90 s `DefaultTimeoutStopSec` but not every
distribution's. Fedora 44 ships 45 s (`systemctl show -p
DefaultTimeoutStopUSec` → `DefaultTimeoutStopUSec=45s`). The agent cannot read
its unit's `TimeoutStopSec`, so keeping `shutdown_grace_seconds + 20 s` inside it
stays the operator's job — but it is now arithmetic they can do, which is why
`docs/runbooks/operator-node-agent.md` states the sum instead of telling them to
set the stop timeout "above the configured shutdown grace".

A control plane that answers more slowly than 20 s during shutdown now loses the
`Retired` write. That is not a new state. It is what the second-signal force
already produced, and the runbook already records that a forced shutdown can
leave the incarnation or lease terminal state for TTL expiry to reconcile.

Steady-state retry behaviour is untouched. The deadline exists only in the
shutdown tail; acquisition, heartbeats, and lease reporting keep the full retry
budget.

## Considered & rejected

- **Do nothing — a second signal already forces.** verified: `signal_phase_for_exit`
  (`runtime.rs:1864`) returns `ForceEnabled` for a graceful exit, so the next signal
  forces without being consumed first; and systemd sends `SIGTERM` once, then
  `SIGKILL` at the stop timeout (`systemctl show -p DefaultTimeoutStopUSec` →
  `DefaultTimeoutStopUSec=45s`, Fedora 44; 90 s upstream). judgment: an escape only a
  human at a terminal can take is not an escape for a supervised service.
- **Shorten the client's retry budget on the shutdown path.** verified:
  `production_request_budget` (`client.rs:450`) bounds one logical call, and the tail
  makes one settlement call per held lease plus the deactivation, so a per-call bound
  leaves the total proportional to the lease count. judgment: the same client serves
  acquisition and heartbeats, which are not the defect.
- **Bound the deactivation only.** verified: `run_coordinator`'s shutdown arm calls
  `settle_leases_for_shutdown` (`runtime.rs:744`) before the agent reaches
  `finish_shutdown_lifecycle` at all, so the wait that would stay unbounded is the
  first one, not the last.
- **Abort the coordinator tasks at the deadline.** verified: `run_coordinator` reaps
  its child with `ChildSupervisor::shutdown_all` (`runtime.rs:746`) after settlement,
  so an abort there drops the reap. judgment: trading a hung agent for orphaned worker
  processes is not a fix.
- **Make the deadline configurable.** judgment: the range it could usefully take is
  boxed by the stop-timeout ceiling this record targets, and
  `#[tokio::test(start_paused = true)]` — already used in
  `crates/voom-node-agent/src/runtime_test.rs` — makes a constant instant under test,
  so the knob buys neither operator control worth a validation rule nor a test seam.
- **Raise the lifecycle suite's `HANG_GUARD`.** verified: #452 measured the same
  expiry rate at 10 s, 60 s and 150 s guards, with each failing run consuming exactly
  its budget — the bound moves and the hang does not. judgment: it is the bargain that
  made the `expire_due` contention tests false-green (#552).
