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

Each arm is guarded so it fires once, the way the signal arm already is
(`if signals_open && !forced`, `runtime.rs:1838`): the deadline deliberately does
not leave `wait_for_coordinators`' loop, and an elapsed `sleep_until` is ready on
every poll.

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
the reap then runs to completion inside its own bounds below.

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

The single site that reads the field must branch on it, which is the whole reason
for widening it: `runtime.rs:315` is `if progress.forced { return
Err(forced_shutdown_error()); }` today, and the smallest mechanical edit —
`if progress.forced.is_some()` — would report a termination signal on a
settlement-path deadline expiry, the exact message this decision exists to
prevent, on the only deadline path that is reachable during settlement. It maps
instead: `Some(Signal)` to `forced_shutdown_error()`, `Some(Deadline)` to
`shutdown_deadline_error()`.

The startup-failure deactivations take the same bound; they call the same
`deactivate_or_second_signal` and were unbounded for the same reason. They run
before any shutdown tail exists, so each captures its own
`Instant::now() + SHUTDOWN_DEADLINE` at the point of failure rather than sharing
the tail's, and each reports through the same `shutdown_deadline_error()` — there
is no `ShutdownProgress` on those paths at all.

**The reap needs one bound of its own, because `shutdown_grace_seconds` does not
supply the one this record's arithmetic assumed.** `RunningChild::shutdown`
(`crates/voom-node-agent/src/child.rs:193-213`) wraps only the polite wait in
`tokio::time::timeout(grace, child.wait())` at `:199`; when that expires it calls
`start_kill()` at `:204` and then `child.wait().await` at `:207` with no timeout.
A child the kernel cannot kill — a worker parked in uninterruptible sleep on a
hung mount — leaves that wait pending forever, and `wait_for_coordinators` is
still joining. So the post-kill wait gets `REAP_AFTER_KILL`, 1 s: the child is
already doomed at that point and the wait only collects its exit status, so
abandoning it orphans nothing that `SIGKILL` had not already claimed — the
process is reparented and reaped by init. On expiry `shutdown_all` reports the
unreaped child through the `ChildError` it already returns.

The reap was not the stall. Issue #452's body named `ChildSupervisor::shutdown_all`
as an unisolated suspect, and its 2026-08-27 reopening comment ruled it out:
"Both are ruled out. The agent reaches `deactivate` and behaves correctly. It is
the **control plane** that deadlocks." The bound above exists to make the
arithmetic below true, not to close the reported hang.

## Consequences

Graceful shutdown becomes finite and computable:
`SHUTDOWN_DEADLINE + shutdown_grace_seconds + REAP_AFTER_KILL`. The deadline runs
concurrently with the coordinators, so the worst case is a settlement that spends
the whole budget followed by a full grace and a full post-kill wait. The
configuration validator holds `shutdown_grace_seconds` to 1..=60
(`crates/voom-node-agent/src/config.rs:159`), so that worst case is 81 s and the
common case — the runbook's example `shutdown_grace_seconds = 10` — is 31 s.

**81 s fits systemd's upstream 90 s `DefaultTimeoutStopSec` and not every
distribution's, and where it does not fit the change does not deliver its
outcome.** Fedora 44 ships 45 s (`systemctl show -p DefaultTimeoutStopUSec` →
`DefaultTimeoutStopUSec=45s`). On that default, any `shutdown_grace_seconds`
above 24 — a configuration the validator accepts — still produces a tail longer
than the stop timeout, so systemd still sends `SIGKILL` and the deactivation
write is still skipped. That is #452's original exposure, unfixed, inside the
accepted range. The agent cannot read its unit's `TimeoutStopSec`, so keeping the
sum inside it remains the operator's job; what changes is that the sum exists at
all, which is why `docs/runbooks/operator-node-agent.md` gains the arithmetic in
place of "set the supervisor stop timeout above the configured shutdown grace".
Narrowing the validator's grace range would close the gap without operator action
and is not done here: it would invalidate configurations that are legal today,
for a ceiling that varies by distribution and unit file.

**A deadline expiry during settlement skips deactivation entirely, and that is a
new way to lose the `Retired` write.** `finish_shutdown_lifecycle` returns at
`runtime.rs:315` as soon as `progress.forced` is set, so the deactivation arm is
reachable only when settlement finished inside the budget. Any condition that
keeps settlement from completing in 20 s therefore costs the write — not only a
control plane answering slowly. A shutdown that is slow but working loses
something it used to get: a settlement that took 25 s retired the incarnation
before this change and abandons it after, and the startup-failure deactivations
inherit the same bound, so a slow control plane can now leave an incarnation
activated and never deactivated where it previously succeeded. The resulting
state is not new — it is what the second-signal force already produced, and the
runbook records that a forced shutdown leaves terminal state for TTL expiry to
reconcile — but the way of reaching it is. That is the price of the bound, and
the reason 20 s is sized to clear a healthy shutdown rather than to be as tight
as the ceiling allows.

Settlement being concurrent is what keeps that price small. The coordinators run
as a `JoinSet` and each settles its own leases as another, so settlement's wall
clock is the slowest lease's, not the sum over up to 64 workers.

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
- **Give the shutdown path its own, shorter per-call retry budget.** The closest
  alternative to this decision, and cheaper: no shared `Instant` threaded through two
  functions, no guarded `sleep_until` arms, no widened `ShutdownProgress`. verified: a
  per-call budget bounds calls, and the tail does not wait on calls — it waits on
  *tasks*. `wait_for_coordinators` joins coordinator tasks (`runtime.rs:1810`), each of
  which drains a lease `JoinSet` through `wait_for_leases` (`:1694`) and then reaps its
  child (`:746`). None of the reap and none of a lease task's non-call work falls under
  any client budget, so the tail would still have no total — which is the thing #452
  asks for. judgment: it would also have to be threaded to the shutdown call sites as a
  second client configuration, so it is not the cheaper option it first appears to be.
  (An earlier ground for this bullet — that a per-call bound leaves the total
  proportional to the lease count — was wrong: settlement is concurrent, as the
  Consequences state.)
- **Bound the whole tail, reap included, at `SHUTDOWN_DEADLINE`.** It would deliver the
  outcome unconditionally: 20 s fits every stop timeout in play, including the 45 s
  measured above, for every configuration the validator accepts — and it would remove
  the operator arithmetic this change adds to the runbook instead. verified: the
  validator accepts `shutdown_grace_seconds` up to 60 (`config.rs:159`), so a 20 s total
  would cut a configured grace short in every configuration above 20. judgment: the
  grace is the operator's statement of how long a worker may take to finish cleanly, and
  a global bound that silently overrides it while still accepting the value is worse
  than an arithmetic they can check. The bound the reap does get is
  `REAP_AFTER_KILL`, which starts after `SIGKILL` and so overrides nothing the operator
  asked for.
- **Bound the deactivation only.** verified: `run_coordinator`'s shutdown arm calls
  `settle_leases_for_shutdown` (`runtime.rs:744`) before the agent reaches
  `finish_shutdown_lifecycle` at all, so the wait that would stay unbounded is the
  first one, not the last.
- **Abort the coordinator tasks at the deadline.** verified: `run_coordinator` reaps
  its child with `ChildSupervisor::shutdown_all` (`runtime.rs:746`) after settlement,
  so an abort there drops the reap. judgment: trading a hung agent for orphaned worker
  processes is not a fix.
- **Make the deadline configurable.** verified: it buys no test seam —
  `#[tokio::test(start_paused = true)]` is already used in
  `crates/voom-node-agent/src/runtime_test.rs`, so a constant costs no wall clock under
  test. The operator-control half is a real loss and is stated rather than denied: a
  deployment whose control plane legitimately needs more than 20 s to settle would keep
  retiring cleanly with a knob and loses that write without one. judgment: it is not
  taken because the ceiling a knob has to fit inside is the unit's `TimeoutStopSec`,
  which the knob cannot raise — an operator who needs a longer shutdown must edit the
  unit file either way, and a control plane needing over 20 s during shutdown is a
  condition to fix rather than to wait on.
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
