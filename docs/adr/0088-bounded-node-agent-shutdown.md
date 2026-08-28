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
`production_request_budget` (`client.rs:450`) puts one logical call at five
attempts of a 30 s `REQUEST_TIMEOUT` plus backoff ceilings — 153.75 s, asserted
in `crates/voom-node-agent/tests/budget_ladder.rs:97-99`.

The only escape is a **second** termination signal.
`ShutdownSignalPhase::signal_forces` consumes the first signal as the one that
began the shutdown and forces on the next; `signal_phase_for_exit`
(`runtime.rs:1864`) starts a graceful exit already in `ForceEnabled`, so on that
path the next signal forces immediately.

An init system never sends that signal. It sends `SIGTERM` once, waits its stop
timeout, then sends `SIGKILL` — skipping the deactivation write. That is the
exposure #452 names: an agent that does not exit on `SIGTERM` leaves its
incarnation un-retired.

Issue #592 removed the mechanism that made this reproduce in CI — a pooled
connection holding the `SQLite` write lock, [ADR
0087](0087-cancellation-safe-begin-immediate.md). It did not remove the unbounded
wait. Any unresponsive control plane still reaches it.

## Decision

**Each control-plane wait in the shutdown tail gets its own wall-clock budget,
`SHUTDOWN_CALL_DEADLINE`, of 10 s.** Expiry does what a second signal already
does, so no new teardown path exists. Two budgets rather than one shared instant:
a single instant covering both waits leaves deactivation whatever settlement did
not spend, so a settlement finishing at 19 s of 20 s abandons the write on
arrival.

**The settlement budget is one `sleep_until` arm in `wait_for_coordinators`,
mirroring the signal arm beside it.** That arm already reads
`forced = true; let _ = shutdown.send(ShutdownKind::Forced);` and keeps looping
(`runtime.rs:1838-1846`); the deadline arm does the same, guarded so it fires
once. Sending on the watch rather than abandoning the join is what keeps the
child reap whole: `wait_or_force` observes `Forced` and abandons the
control-plane wait, while `ChildSupervisor::shutdown_all` does not observe it and
runs to completion at the operator's full `shutdown_grace_seconds`.

**Not in `wait_or_force` itself**, which is where the settlement wait literally
blocks. It has three call sites (`runtime.rs:1705`, `:1718`, `:1798`) and only
the last is the shutdown tail; the other two are `settle_leases_after_child_crash`,
reached from `restart_after_child_exit` (`:771-796`) on an ordinary worker crash
with no shutdown in progress. An arm there would arm all three, so a crash whose
lease settlement ran past 10 s would terminate a healthy, running agent.

**A deadline force does not skip the deactivation; a signal force still does.**
`finish_shutdown_lifecycle` returns at `runtime.rs:315` on any force today. Under
this decision `Some(Signal)` keeps exactly that — the operator said stop, and
criterion 4 holds it unchanged — while `Some(Deadline)` falls through to the
deactivation, which has its own budget, and then returns
`shutdown_deadline_error()` whether or not the write landed. A timer expiring is
not an instruction to skip the write the whole change exists to protect, and
attempting it costs a bounded 10 s. This is also what makes a long-but-healthy
reap safe: with `shutdown_grace_seconds = 30` and a worker that uses its grace,
the deadline elapses during the reap, but the incarnation still retires.

**The cause is decided where the arm fires.** Reporting a deadline expiry as
"interrupted by a termination signal" would send an operator looking for a signal
nobody sent, so `ShutdownProgress.forced` becomes `Option<ShutdownForce>` —
`Signal` or `Deadline` — written by whichever of the two arms fired. Nothing
travels: `LeaseSettlement` and `CoordinatorExit` are unchanged. With up to 64
coordinators the two arms can both fire, so precedence is stated rather than left
to `join_next()` order: **a signal force outranks a deadline force**, because it
is the operator's explicit instruction and it is the arm whose behaviour must not
change. The join arm, which sets the flag when a coordinator reports
`LeaseSettlement::Forced` (`:1822-1824`), only records a cause where none is set;
both local arms set theirs before sending `Forced`, so that path is reachable
only through `wait_or_force`'s watch-closed case, and it takes `Signal` to
preserve today's message there.

`deactivate_or_second_signal` returns its error directly, so its own arm returns
a new `shutdown_deadline_error()` beside the existing `forced_shutdown_error()`.
Both are `VoomError::ExternalSystemUnavailable`, so the exit code is unchanged.
The startup-failure deactivations take the same budget; they call the same
function and were unbounded for the same reason.

**10 s is derived from the guard the change has to prove itself under.**
`crates/voom-node-agent/tests/lifecycle.rs:47` holds `HANG_GUARD` at 30 s, [ADR
0066](0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md) binds both the
agent join and the `Retired` observation to it, and this issue's charter forbids
raising it; a budget near 30 s would leave the guard, not the deadline, firing
first — the bound would move and the symptom would not. Two budgets of 10 s plus
the maximum `shutdown_grace_seconds` of 60 and `REAP_AFTER_KILL` also keep the
whole tail at 81 s, inside systemd's upstream 90 s. And 10 s sits below one
`REQUEST_TIMEOUT` (30 s) deliberately: a shutdown blocked on a wedged control
plane should abandon rather than spend an attempt.

**The reap needs one bound of its own, because `shutdown_grace_seconds` does not
supply the one the arithmetic above assumes.** `RunningChild::shutdown`
(`child.rs:193-213`) wraps only the polite wait in
`tokio::time::timeout(grace, child.wait())` at `:199`; on expiry it calls
`start_kill()` at `:204` and then `child.wait().await` at `:207` with no timeout.
A child the kernel cannot kill — a worker parked in uninterruptible sleep on a
hung mount — leaves that wait pending forever. So the post-kill wait gets
`REAP_AFTER_KILL`, 1 s: the child is already doomed and the wait only collects
its exit status, so abandoning it orphans nothing `SIGKILL` had not already
claimed, and the process is reparented to init.

The reap was not the stall. #452's body named `ChildSupervisor::shutdown_all` as
an unisolated suspect; its 2026-08-27 reopening comment ruled it out — "Both are
ruled out. The agent reaches `deactivate` and behaves correctly. It is the
**control plane** that deadlocks." The bound exists to make the arithmetic true,
not to close the reported hang.

**The shutdown budgets invert `budget_ladder.rs`'s ordering rule, deliberately,
and are recorded there as a rung.** That file's rule is that an observer's budget
must exceed the budget of what it observes, because an observer expiring first
"reports a timeout of its own instead of the failure underneath it (see #592)". A
10 s budget observing a 153.75 s logical call does exactly that. It is correct
here and nowhere else: during shutdown the agent's obligation is to exit, and the
failure underneath is one it can no longer act on. The rung records the inversion
and its scope so the relationship is asserted rather than rediscovered.

## Consequences

Graceful shutdown becomes finite and computable:
`2 × SHUTDOWN_CALL_DEADLINE + shutdown_grace_seconds + REAP_AFTER_KILL`. The
validator holds `shutdown_grace_seconds` to 1..=60 (`config.rs:159`), so the
worst case is 81 s and the common case — the runbook's example
`shutdown_grace_seconds = 10` — is 31 s.

**81 s fits systemd's upstream 90 s `DefaultTimeoutStopSec` and not every
distribution's, and where it does not fit the change does not deliver its
outcome.** Fedora 44 ships 45 s (`systemctl show -p DefaultTimeoutStopUSec` →
`DefaultTimeoutStopUSec=45s`). On that default, any `shutdown_grace_seconds`
above 24 — a configuration the validator accepts — still produces a tail longer
than the stop timeout, so `SIGKILL` still lands and the write is still skipped:
#452's exposure, unfixed, inside the accepted range. The agent cannot read its
unit's `TimeoutStopSec`, so keeping the sum inside it remains the operator's job;
what changes is that the sum exists at all, which is why
`docs/runbooks/operator-node-agent.md` gains the arithmetic in place of "set the
supervisor stop timeout above the configured shutdown grace". Narrowing the
validator's grace range would close the gap without operator action and is not
done here: it would invalidate configurations that are legal today, for a ceiling
that varies by distribution and unit file.

**One new way to lose the `Retired` write, and it is the deactivation's own
budget.** A control plane that cannot answer the deactivation inside 10 s
abandons the write where the full retry budget might eventually have landed it,
and the startup-failure deactivations inherit that. Settlement overrunning no
longer costs the write, because a deadline force falls through to the
deactivation; nor does a long reap. The resulting state is not new — it is what
the second-signal force already produced, and the runbook records that a forced
shutdown leaves terminal state for TTL expiry to reconcile — but the way of
reaching it is. That is the price of the bound, and the reason 10 s clears a
healthy shutdown by a wide margin rather than being as tight as the ceiling
allows.

Settlement being concurrent keeps its own budget generous: the coordinators run
as a `JoinSet` and each settles its own leases as another, so settlement's wall
clock is the slowest lease's, not the sum over up to 64 workers.

**An unreaped child is silent.** `shutdown_all` returns a `ChildError`, but both
shutdown-path callers discard it (`let _ = supervisor.shutdown_all(…)`,
`runtime.rs:746` and `:796`) and `child.rs` logs nothing — `voom-node-agent`
carries no `tracing` dependency. So `REAP_AFTER_KILL` converts a hang into an
orphaned worker the operator is not told about, where a hang at least shows up as
a stuck unit. Accepted, because the alternative is the unbounded wait; changing
the discard would change coordinator exit semantics for every existing failure on
that path and is not taken here.

**The lifecycle suite fails differently, not less.** [ADR
0066](0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md) binds the agent
join and the `Retired` observation to one 30 s `HANG_GUARD`. When the control
plane is genuinely wedged the incarnation is still not retired, so that assertion
still fails — but it now fails on the agent's own `shutdown_deadline_error()` at
about 20 s rather than on a guard expiring against a wait with no end.

Steady-state behaviour is untouched. The budgets are in `wait_for_coordinators`
and `deactivate_or_second_signal`, neither of which runs outside a shutdown; the
worker-crash settlement path through `wait_or_force` is deliberately not armed.

## Considered & rejected

- **Do nothing — a second signal already forces.** verified: `signal_phase_for_exit`
  (`runtime.rs:1864`) returns `ForceEnabled` for a graceful exit, so the next signal
  forces without being consumed first; and systemd sends `SIGTERM` once, then `SIGKILL`
  at the stop timeout (`systemctl show -p DefaultTimeoutStopUSec` →
  `DefaultTimeoutStopUSec=45s`, Fedora 44; 90 s upstream). judgment: an escape only a
  human at a terminal can take is not an escape for a supervised service.
- **Do nothing — TTL expiry already reconciles an un-retired incarnation.** The
  stronger null option, and the one this record's own Consequences invite. verified: the
  reconciliation path exists and is documented — `docs/runbooks/operator-node-agent.md`
  records that a forced shutdown "can leave the incarnation or lease terminal state for
  TTL expiry/recovery to reconcile". judgment: rejected because the charter's completion
  criteria name the `Retired` write rather than eventual consistency, and because what
  the change buys is promptness and attribution — an agent that exits on `SIGTERM` with
  an error naming the deadline, instead of one `SIGKILL`ed mid-shutdown leaving an
  operator to infer from a TTL. It does not buy correctness TTL reconciliation could not
  eventually reach.
- **Put the settlement budget in `wait_or_force`, where the wait literally blocks.**
  verified: `wait_or_force` has three call sites (`runtime.rs:1705`, `:1718`, `:1798`)
  and two of them are `settle_leases_after_child_crash`, reached from
  `restart_after_child_exit` (`:771-796`) during ordinary operation with no shutdown in
  progress. An arm there arms all three, so a slow settlement after one worker crash
  would fold a `Forced` settlement into `progress.forced` and terminate a healthy agent
  with a shutdown-deadline error. judgment: the budget belongs where the shutdown is,
  and `wait_for_coordinators` is the only wait that runs nowhere else.
- **Let a deadline force skip the deactivation, as a signal force does.** The smallest
  edit at `runtime.rs:315` (`if progress.forced.is_some()`), and it is wrong. verified:
  the deadline elapses on wall clock regardless of what the coordinators are doing, and
  a coordinator does not return until both `settle_leases_for_shutdown` and
  `shutdown_all` have finished (`:742-747`) — so with `shutdown_grace_seconds = 30` (the
  validator accepts 1..=60, `config.rs:162-163`), a worker that uses its grace, and a
  fully healthy control plane, the flag is set by the reap's duration alone and the
  write is skipped. That is #452's exposure re-created by the fix.
- **One shared absolute deadline across both waits.** verified: it leaves deactivation
  only what settlement did not spend, so a settlement finishing at 19 s of a 20 s budget
  abandons the write against a client whose single attempt is 30 s (`client.rs:33`).
- **Give the shutdown path a shorter per-call retry budget instead.** Cheaper: no
  `sleep_until` arms, no widened `ShutdownProgress`. verified: a per-call budget bounds
  calls, and the tail waits on *tasks* — `wait_for_coordinators` joins coordinator tasks
  (`runtime.rs:1810`), each draining a lease `JoinSet` through `wait_for_leases`
  (`:1694`) and then reaping its child (`:746`). The reap and a lease task's non-call
  work fall under no client budget, so the tail would still have no total.
- **Bound the whole tail, reap included.** It would deliver the outcome
  unconditionally: 20 s fits every stop timeout in play for every configuration the
  validator accepts. verified: the validator accepts `shutdown_grace_seconds` up to 60
  (`config.rs:159`), so such a bound cuts a configured grace short in every
  configuration above it. judgment: the grace is the operator's statement of how long a
  worker may take to finish cleanly, and a global bound that silently overrides it while
  still accepting the value is worse than arithmetic they can check. `REAP_AFTER_KILL`
  starts after `SIGKILL` and so overrides nothing the operator asked for.
- **Bound the deactivation only.** verified: `run_coordinator`'s shutdown arm calls
  `settle_leases_for_shutdown` (`runtime.rs:744`) before the agent reaches
  `finish_shutdown_lifecycle` at all, so the wait that would stay unbounded is the first
  one.
- **Abort the coordinator tasks at the deadline.** verified: `run_coordinator` reaps its
  child with `ChildSupervisor::shutdown_all` (`runtime.rs:746`) after settlement, so an
  abort there drops the reap. judgment: trading a hung agent for orphaned worker
  processes is not a fix.
- **Make the budget configurable.** verified: it buys no test seam —
  `#[tokio::test(start_paused = true)]` is already used in `runtime_test.rs`, so a
  constant costs no wall clock under test. The operator-control half is a real loss and
  is stated rather than denied: a deployment whose control plane legitimately needs more
  than 10 s to deactivate would keep retiring cleanly with a knob and loses that write
  without one. judgment: the ceiling a knob must fit inside is the unit's
  `TimeoutStopSec`, which the knob cannot raise — an operator needing a longer shutdown
  edits the unit file either way.
- **Change nothing in the agent; specify the deployment's stop timeout instead** — a
  unit file with `TimeoutStopSec=` above `production_request_budget()`, or runbook
  guidance to that effect. verified: this repository ships no unit file today (`fd -H -e
  service -e unit .` returns nothing), so it would be new guidance rather than an edit,
  and needs no code. judgment: it works by moving the default the charter's outcome is
  stated against. It is close to the decision, because the decision does not remove the
  operator-configuration dependency either; what it buys over it is that the number the
  operator must accommodate is finite and small.
- **Raise the lifecycle suite's `HANG_GUARD`.** verified: #452 measured the same expiry
  rate at 10 s, 60 s and 150 s guards, each failing run consuming its whole budget
  (67.6/67.6/67.7 s at 60 s; 156.2/157.8 s at 150 s) — the bound moves and the hang does
  not. judgment: it is the bargain that made the `expire_due` contention tests
  false-green (#552).
