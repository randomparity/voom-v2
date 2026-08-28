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

Each arm is guarded so it fires once. `sleep_until` on an elapsed `Instant` is
immediately ready, and `wait_for_coordinators` loops `while
!coordinators.is_empty()` — the deadline deliberately does not leave that loop, so
that the reap still completes. An unguarded arm would therefore be ready on every
subsequent poll and spin the select hot for the whole reap window, on exactly the
oversubscribed host this defect appears under. The signal arm's existing
`if signals_open && !forced` (`runtime.rs:1838`) is the pattern.

**20 s is derived from the guard the change has to prove itself under, not chosen
for roundness.** `crates/voom-node-agent/tests/lifecycle.rs:47` holds
`HANG_GUARD` at 30 s, [ADR 0066](0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md)
binds both the agent join and the `Retired` observation to it, and this issue's
charter forbids raising it. A deadline at or near 30 s would leave the guard, not
the deadline, as the thing that fires first — the bound would move and the
symptom would not. Issue #452 measured loaded passing runs of that suite at 6–9 s,
so 20 s clears a healthy shutdown by a wide margin while leaving the guard room
for that progress plus a 1 s reap. It also sits below one `REQUEST_TIMEOUT`
(30 s), deliberately: a shutdown blocked on a wedged control plane should abandon
rather than spend a further attempt.

**Forcing goes through the `ShutdownKind::Forced` watch, not through aborting
coordinator tasks.** A coordinator reaps its child after settlement, so aborting
it at the deadline would leave the worker process running. The watch is observed
by `wait_or_force` inside settlement, which abandons the control-plane wait, and
the reap then runs to completion inside `shutdown_grace_seconds`.

**Each wait reports its own cause, because they do not share a channel.**
Reporting a deadline expiry as "interrupted by a termination signal" would send
an operator looking for a signal nobody sent, and the two waits need separate
mechanisms to avoid it. `ShutdownProgress` (`runtime.rs:1936`) is produced only by
`wait_for_coordinators` and read only at `:315`, so it can carry the settlement
wait's cause and nothing else: `forced` becomes `Option<ShutdownForce>` —
`Signal` or `Deadline`. The deactivation wait never constructs a
`ShutdownProgress`; `deactivate_or_second_signal` returns its error directly, so
its deadline arm returns a new `shutdown_deadline_error()` beside the existing
`forced_shutdown_error()`. Both are `VoomError::ExternalSystemUnavailable`, so
the exit code is unchanged.

The startup-failure deactivations take the same bound; they call the same
`deactivate_or_second_signal` and were unbounded for the same reason. They run
before any shutdown tail exists, so each captures its own
`Instant::now() + SHUTDOWN_DEADLINE` at the point of failure rather than sharing
the tail's, and each reports through the same `shutdown_deadline_error()` — there
is no `ShutdownProgress` on those paths at all.

Nothing new bounds the child reap. It is bounded by `shutdown_grace_seconds`
already and was never the part that hung.

## Consequences

Graceful shutdown becomes finite and computable:
`SHUTDOWN_DEADLINE + shutdown_grace_seconds`. The configuration validator holds
`shutdown_grace_seconds` to 1..=60 (`crates/voom-node-agent/src/config.rs:159`),
so the worst case is 80 s and the common case — the runbook's example
`shutdown_grace_seconds = 10` — is 30 s.

**80 s fits systemd's upstream 90 s `DefaultTimeoutStopSec` and not every
distribution's, and where it does not fit the change does not deliver its
outcome.** Fedora 44 ships 45 s (`systemctl show -p DefaultTimeoutStopUSec` →
`DefaultTimeoutStopUSec=45s`). On that default, any `shutdown_grace_seconds`
above 25 — a configuration the validator accepts — still produces a tail longer
than the stop timeout, so systemd still sends `SIGKILL` and the deactivation
write is still skipped. That is #452's original exposure, unfixed, inside the
accepted range. The agent cannot read its unit's `TimeoutStopSec`, so keeping
`shutdown_grace_seconds + 20 s` inside it remains the operator's job; what
changes is that the sum is now finite and computable, which is why
`docs/runbooks/operator-node-agent.md` gains the arithmetic in place of "set the
supervisor stop timeout above the configured shutdown grace". Narrowing the
validator's grace range would close the gap without operator action and is not
done here: it would invalidate configurations that are legal today, for a ceiling
that varies by distribution and unit file.

**A deadline expiry during settlement skips deactivation entirely**, and the
trigger is not the one an earlier draft of this record named. `finish_shutdown_lifecycle`
returns at `runtime.rs:315` as soon as `progress.forced` is set, so the
deactivation arm is reachable only when settlement finished inside the budget.
Any condition that keeps settlement from completing in 20 s therefore costs the
`Retired` write — not only a control plane answering slowly. Settlement is
concurrent, so the exposure is smaller than a per-lease reading suggests: the
coordinators run as a `JoinSet` and each settles its own leases as another, so
the wall clock is the slowest lease's, not the sum over up to 64 workers. The
resulting state is not new either — it is what the second-signal force already
produced, and the runbook already records that a forced shutdown can leave the
incarnation or lease terminal state for TTL expiry to reconcile.

**The lifecycle suite fails differently, not less.** [ADR
0066](0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md) binds the agent
join and the `Retired` observation to one 30 s `HANG_GUARD`. When the control
plane is genuinely wedged the incarnation is still not retired, so that assertion
still fails — but it now fails on the agent's own `shutdown_deadline_error()`
at about 20 s, rather than on a guard expiring against a wait with no end. The
guard stops being the only thing that detects the hang, which is what makes
raising it (#446, and this charter's exclusion) unnecessary rather than merely
forbidden.

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
  expiry rate at 10 s, 60 s and 150 s guards, with each failing run consuming its whole
  budget (67.6/67.6/67.7 s at 60 s; 156.2/157.8 s at 150 s) — the bound moves and the
  hang does not. judgment: it is the bargain that made the `expire_due` contention tests
  false-green (#552).
- **Change nothing in the agent; specify the deployment's stop timeout instead** — ship
  a unit file with `TimeoutStopSec=` above `production_request_budget()`, or say so in
  the runbook. verified: this repository ships no unit file today (`fd -H -e service -e
  unit .` returns nothing), so it would be new guidance rather than an edit, and it needs
  no code at all. judgment: it works by moving the default the charter's outcome is
  stated against — "honoured well inside an init system's *default* stop timeout" — so it
  answers a different question. It is the closest alternative to the decision, because
  the decision does not remove the operator-configuration dependency either: the
  Consequences concede that keeping `shutdown_grace_seconds + 20 s` inside the stop
  timeout stays the operator's job. What the decision buys over it is that the number the
  operator must accommodate is finite and small, where today it is
  `production_request_budget()` per call with no total at all.
