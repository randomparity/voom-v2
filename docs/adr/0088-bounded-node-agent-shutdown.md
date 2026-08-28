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
`SHUTDOWN_CALL_DEADLINE`, of 10 s.** The budgets live in one `ShutdownBudgets`
value carried by `AgentRuntime` — `call`, `reap_after_kill`, `backstop_margin` —
with a `DEFAULT` used in production and a `#[cfg(test)]` constructor. They are
threaded as arguments, not read from constants at the point of use: three of the
waits they bound are exercised by existing real-time tests whose whole design is
that the call blocks until a signal arrives, and a real 10 s timer would turn
each into a wall-clock race. Two budgets rather than one shared instant:
a single instant covering both waits leaves deactivation whatever settlement did
not spend, so a settlement finishing at 19 s of 20 s abandons the write on
arrival.

**The settlement budget is a parameter on `wait_or_force`, supplied by one call
site.** `wait_or_force` (`runtime.rs:1744`) is where the settlement wait actually
blocks, and it takes an `Option<Instant>` deadline: `settle_leases_for_shutdown`
passes `Some` at `:1798`, and the two crash-path call sites — `:1705` and `:1718`,
both inside `settle_leases_after_child_crash` — pass `None`. Expiry does locally
what observing `ShutdownKind::Forced` already does: abort the lease tasks and
return forced. It does not publish on the watch, so it does not force
coordinators that are not overrunning.

Both halves of that placement are load-bearing, and each rules out an obvious
alternative. Arming `wait_or_force` unconditionally would arm the crash path,
where `restart_after_child_exit` (`:771-796`) runs during ordinary operation with
no shutdown in progress, so a slow settlement after one worker crash would
terminate a healthy agent. Putting the budget one level up, in
`wait_for_coordinators`, would arm a wall clock over whole coordinator tasks — and
a coordinator does not return until `settle_leases_for_shutdown` *and*
`shutdown_all` have both finished (`:742-747`), so the arm would fire on the
child reap. With any `shutdown_grace_seconds` above 10 and a worker that uses it,
a routine stop with a healthy control plane would then be marked forced and exit
non-zero, putting the unit into `failed` on every stop.

`wait_for_coordinators` therefore gains nothing: its signal arm and its
`!forced` guard are unchanged apart from the field's type.

**A deadline force does not skip the deactivation; a signal force still does.**
`finish_shutdown_lifecycle` returns at `runtime.rs:315` on any force today. Under
this decision `Some(Signal)` keeps exactly that — the operator said stop, and
criterion 4 holds it unchanged — while `Some(Deadline)` falls through to the
deactivation, which has its own budget, and then returns
`shutdown_deadline_error()`. A timer expiring is not an instruction to skip the
write the whole change exists to protect, and attempting it costs a bounded 10 s.
Returning an error there is honest because `Deadline` now means settlement itself
overran: leases were abandoned mid-settlement, whether or not the write landed.

**The cause travels with the settlement result.** Reporting a deadline expiry as
"interrupted by a termination signal" would send an operator looking for a signal
nobody sent, so `LeaseSettlement::Forced` carries a `ShutdownForce` — `Signal`
when `wait_or_force` broke out on the watch, `Deadline` when it broke out on its
own timer — through `CoordinatorExit::Shutdown` into `ShutdownProgress.forced`,
which becomes `Option<ShutdownForce>`. The first force recorded wins, exactly as
today: the signal arm's guard already disables it once anything has forced, and
that behaviour is preserved rather than replaced by a precedence rule.

`deactivate_or_second_signal` returns its error directly, so its own arm returns
a new `shutdown_deadline_error()` beside the existing `forced_shutdown_error()`.
Both are `VoomError::ExternalSystemUnavailable`, so the exit code on a genuine
failure is unchanged. The startup-failure deactivations take the same budget;
they call the same function and were unbounded for the same reason.

**A backstop makes the published total true by construction, because
enumerating every wait is not something this record can promise.** The budgets
above bound the two waits the tail is *supposed* to spend its time in. They do
not bound a wait nobody raced against the shutdown receiver, and the review of
this design found three: `settle_leases_after_child_crash`'s second
`wait_or_force` (`runtime.rs:1718`), which reaches the retrying client; and,
inside `restart_after_child_exit`, `restart_child` (`:804-806` — up to
`RESTART_LIMIT` attempts of `NVIDIA_STARTUP_TIMEOUT`, five minutes each,
`child.rs:22-24`) and the `Ready` readiness update at `:807-815`. Each was found
by reading further, not by a rule, and the next one would be found the same way.

So `run_with_seeded_shutdowns` wraps its whole tail — `wait_for_coordinators`
through `finish_shutdown_lifecycle` — in a `tokio::time::timeout` equal to the
published total, `2 × SHUTDOWN_CALL_DEADLINE + shutdown_grace_seconds +
REAP_AFTER_KILL`, plus `BACKSTOP_MARGIN`, and returns a distinct `shutdown_backstop_error()` on
expiry. Dropping that future drops the coordinator `JoinSet`, which aborts its
tasks; the child a coordinator was reaping is `SIGKILL`ed because `launch` sets
`.kill_on_drop(true)` on it (`child.rs:404`), so no child is left un-signalled.
`RunningChild`'s own `Drop` (`:216-231`) does not cover that window —
`shutdown` has already moved the handle out at `:195` and does not set `reaped`
until `:211`, so `Drop` takes its early return — and it covers only a coordinator
aborted before it reaches `shutdown_all`. The distinction matters because
`kill_on_drop(true)` reads as redundant beside an explicit killing `Drop`, and
removing it would silently reintroduce an orphaned worker on every backstop
expiry.

**The backstop has exactly one enumerated reachable path, and is otherwise
insurance.** Once the enumerated waits are raced and bounded, one deliberate
exception remains: a commit drive past its `applying` receipt is not raced, for the
reasons below, and nothing else bounds it. That is the one firing this design
expects. Everything else the backstop catches is a wait the enumeration missed —
insurance against a completeness claim this record declines to make, bought because
the review of this design found a further unraced wait on each of three passes, and
its own tests drive the wrapper with a never-ready future rather than driving the
system. Its price is 5 s on the number the runbook instructs operators to configure
against. Kept by explicit maintainer decision on 2026-08-28, after a scope audit put
the question.

The margin is what makes the backstop a backstop. The inner bounds are sequential
and sum to `2 × call + grace + reap_after_kill` exactly, and the tail carries
costs outside all of them: `wait_or_force`'s post-expiry
`abort_all` and drain (`runtime.rs:1771-1774`) is itself unbounded,
`shutdown_all` starts each child's `grace` on first poll after a `JoinSet` spawn
rather than at the call (`child.rs:356-361`), and `finish_shutdown_lifecycle`
stops the heartbeat before deactivating. Sized at the bare sum, the backstop
would fire *during* the deactivation it exists to protect — cancelling the
`Retired` write, which is the loss this decision refused when it rejected letting
a deadline force skip deactivation. `BACKSTOP_MARGIN` of 5 s covers those
fragments so every inner bound genuinely expires first. An ordinary overrun is
then still attributed to the wait that caused it; the backstop fires only on the
enumerated unraced wait — the journaled commit drive — or when an inner bound did
not hold, which is a defect. Its error names both, because neither is inferable
from the other and the crate has no other channel, and it does not reuse the
deadline message an operator is told to expect.

Two of those three waits are still raced directly, because a backstop that fires
is a worse outcome than one that does not. `restart_after_child_exit`'s readiness
calls are raced against the shutdown receiver, the way the main coordinator loop
already races its work at `:704-717` — a readiness update for a worker that is
going away is not worth waiting on, and the shutdown handling immediately below
is where the coordinator then goes. And `:1718` takes the settlement budget:
`settle_leases_after_child_crash` short-circuits at `let observed = observed?;`
(`:1714`), so that second wait runs *only* when a shutdown is already in flight.
It is a shutdown-tail wait, not a steady-state one. Only `:1705`, inside
`cancel_and_wait`, is genuinely steady-state and stays unbounded.

**10 s is derived from the guard the change has to prove itself under.**
`crates/voom-node-agent/tests/lifecycle.rs:47` holds `HANG_GUARD` at 30 s, [ADR
0066](0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md) binds both the
agent join and the `Retired` observation to it, and this issue's charter forbids
raising it; a budget near 30 s would leave the guard, not the deadline, firing
first — the bound would move and the symptom would not. Two budgets of 10 s plus
the maximum `shutdown_grace_seconds` of 60 and `REAP_AFTER_KILL` also keep the
whole tail at 86 s, inside systemd's upstream 90 s. And 10 s sits below one
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

**The three values are fields of one `pub struct ShutdownBudgets` in `runtime`,
with a `pub const DEFAULT`** — `call`, `reap_after_kill`, `backstop_margin` — read
by `crates/voom-node-agent/tests/budget_ladder.rs` the way it already reads
`client::REQUEST_TIMEOUT`. `child` gains no public constant: its bound arrives as
an argument. A struct rather than bare constants because three existing tests
gate `deactivate` with a `Notify` that is never notified, so a constant read at
the point of use would turn each into a wall-clock race; the struct carries a
`#[cfg(test)]` constructor that shrinks them. Where this record names
`SHUTDOWN_CALL_DEADLINE`, `REAP_AFTER_KILL` or `BACKSTOP_MARGIN`, read
`budgets.call`, `budgets.reap_after_kill` and `budgets.backstop_margin`; the
names are kept in the arithmetic because they read better there.

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
`2 × SHUTDOWN_CALL_DEADLINE + shutdown_grace_seconds + REAP_AFTER_KILL +
BACKSTOP_MARGIN`. The validator holds `shutdown_grace_seconds` to 1..=60
(`config.rs:159`), so the worst case is 86 s and the common case — the runbook's
example `shutdown_grace_seconds = 10` — is 36 s.

**86 s fits systemd's upstream 90 s `DefaultTimeoutStopSec` and not every
distribution's, and where it does not fit the change does not deliver its
outcome.** Fedora 44 ships 45 s (`systemctl show -p DefaultTimeoutStopUSec` →
`DefaultTimeoutStopUSec=45s`). On that default, any `shutdown_grace_seconds`
above 19 — a configuration the validator accepts — still produces a tail longer
than the stop timeout, so `SIGKILL` still lands and the write is still skipped:
#452's exposure, unfixed, inside the accepted range. The agent cannot read its
unit's `TimeoutStopSec`, so keeping the sum inside it remains the operator's job;
what changes is that the sum exists at all, which is why
`docs/runbooks/operator-node-agent.md` gains the arithmetic in place of "set the
supervisor stop timeout above the configured shutdown grace". Narrowing the
validator's grace range would close the gap without operator action and is not
done here: it would invalidate configurations that are legal today, for a ceiling
that varies by distribution and unit file. That residual is owned by
[#597](https://github.com/randomparity/voom-v2/issues/597).

**A routine stop is unchanged, including a slow one.** Nothing times the child
reap, so a graceful stop whose worker uses its full `shutdown_grace_seconds`
against a healthy control plane settles, reaps, deactivates, records
`Retired`/`GracefulShutdown` and returns `Ok(())` exactly as it does today. That
matters more than it sounds: `voom-node-agent`'s `main` returns
`Result<(), VoomError>` (`main.rs:17-21`), so any error here becomes
`ExitCode::FAILURE`, and a bound that marked a routine stop forced would put the
unit into `failed` on every `systemctl stop`.

**One new way to lose the `Retired` write, and it is the deactivation's own
budget.** A control plane that cannot answer the deactivation inside 10 s
abandons the write where the full retry budget might eventually have landed it,
and the startup-failure deactivations inherit that. A settlement that overruns
its budget does *not* cost the write — the deadline force falls through to the
deactivation — but it does cost the exit code, and it abandons leases
mid-settlement for TTL reconciliation. The *lease* state is not new: live leases
awaiting TTL expiry are what the second-signal force already produced, and the
runbook records that. That is the price of the bound, and the reason 10 s clears a
healthy shutdown by a wide margin rather than being as tight as the ceiling allows.

**The pairing is new, though, and the durable record does not distinguish it.**
Pre-change, `finish_shutdown_lifecycle` returned before the deactivation on *any*
force, so no force could produce a `Retired` incarnation: a second signal left live
leases under an incarnation still `Active`. A `Deadline` force now falls through, so
the write lands with `GracefulShutdown` — which `remote_deactivate` maps to
`Retired` — while the leases that incarnation held were aborted mid-settlement
rather than settled. A `Retired`/`GracefulShutdown` incarnation therefore no longer
implies its leases were settled, and nothing durable says which case it was:
`NodeIncarnationEndReason` has no variant for a partial settlement, and adding one
would be the worker-protocol change this charter excludes. The distinguishing
signals are the non-zero exit and `shutdown_deadline_error()`'s text, and the crate
has no logging, so a supervisor recording exit status alone keeps no attributable
trace. Accepted: landing the write is the outcome #452 asks for, and skipping it to
keep the old implication intact is the trade this decision already rejected. The
runbook says so beside its existing TTL note.

Settlement being concurrent keeps its own budget generous: the coordinators run
as a `JoinSet` and each settles its own leases as another, so settlement's wall
clock is the slowest lease's, not the sum over up to 64 workers.

**A second signal arriving after a deadline force is absorbed rather than
immediate.** `wait_for_coordinators`' signal arm is guarded `signals_open &&
!forced` (`runtime.rs:1838`), and today only a signal can force, so "first force
wins" and "the signal wins" are the same sentence. They stop being the same once
a `Deadline` force exists: with one recorded, the arm is disabled, so a genuine
second signal is no longer consumed there and no longer publishes
`ShutdownKind::Forced`. The remaining coordinators run out their own settlement
budgets and their reaps before the queued signal is consumed in
`deactivate_or_second_signal`. The end state is unchanged — `forced_shutdown_error()`,
write skipped — but the operator's second signal is delayed by up to one budget
plus a reap instead of acting at once — 71 s at the maximum grace. Criterion 4
holds for the outcome and not for the latency, and that narrower reading was
ratified by explicit maintainer decision on 2026-08-28. The alternative — guarding
the signal arm on `forced != Some(ShutdownForce::Signal)` so a second signal still
acts at once — restores the original latency at the cost of the "first force
recorded wins" semantics this change otherwise leaves untouched; it was offered
and declined.

**An unreaped child is silent.** `shutdown_all` returns a `ChildError`, but both
shutdown-path callers discard it (`let _ = supervisor.shutdown_all(…)`,
`runtime.rs:746` and `:796`) and `child.rs` logs nothing — `voom-node-agent`
carries no `tracing` dependency. So `REAP_AFTER_KILL` converts a hang into an
orphaned worker the operator is not told about, where a hang at least shows up as
a stuck unit. Accepted, because the alternative is the unbounded wait; changing
the discard would change coordinator exit semantics for every existing failure on
that path and is not taken here.

**A commit drive past its `applying` receipt is left to the backstop, deliberately.**
The commit coordinator stands down for a shutdown at its listing call and between
intents, and nowhere else. Once `drive_commit_intent` has journaled `applying`, every
remaining step is an await — three canonicalizations, a source-to-staging
materialization, and a hash-copy-link promotion — and for a large artifact that is the
bulk of the drive's wall clock. It is not raced anyway: cancelling there leaves the
intent `Authorized` carrying an `Applying` receipt, and the frozen idempotency key that
would hit the control plane's replay path is per-incarnation stack state, so no later
incarnation can resume it. The fresh authorize is refused as `not pending`, the intent
is skipped on every poll, and recovery classifies it `operator_required` — which
[ADR 0074](0074-fenced-node-local-commit-intents.md) records as wedging the artifact's
commit slot until a human runs `voom artifact recover-commit`. Trading a bounded tail
for a wedged commit slot on every `SIGTERM` is the worse bargain, so this one wait is
uncovered by the published sum.

**That narrows the wedge; it does not close it, and the difference is deliberate.**
The commit coordinator's task is in the `JoinSet` `wait_for_coordinators` joins, so a
drive still running when the tail backstop expires is aborted with the rest and lands
in exactly the state above. Promotion is a copy and two hashes of the artifact's
bytes, so for a large artifact outlasting `grace + 26` is the ordinary case, not the
exotic one — this is the enumerated reachable backstop firing named earlier in this
record. What the change buys is that a drive shorter than the remaining tail budget
now survives a `SIGTERM` where before every drive was cancelled by one. What it costs
is that the cutoff becomes the agent's own and deterministic at `grace + 26`, where
previously it was the init system's `SIGKILL` at a timeout the operator sets — so
raising `TimeoutStopSec` no longer buys a large commit more time. That lever's removal
is recorded in the runbook beside the arithmetic, and the backstop error names the
recovery command because the crate has no log output. Bounding the tail is the
outcome #452 asks for and this is its price; leaving the drive unbounded would
withdraw the bound in the case the bound exists for.

**The lifecycle suite fails differently, not less.** [ADR
0066](0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md) binds the agent
join and the `Retired` observation to one 30 s `HANG_GUARD`. When the control
plane is genuinely wedged the incarnation is still not retired, so that assertion
still fails — but it now fails on the agent's own `shutdown_deadline_error()` at
about 20 s rather than on a guard expiring against a wait with no end.

Steady-state behaviour is untouched. `wait_or_force`'s deadline is an argument,
and only `settle_leases_for_shutdown` supplies it; the two worker-crash call
sites pass `None`. The one steady-state edit is the `select!` around
`restart_after_child_exit`'s readiness call, which changes nothing while no
shutdown is in progress — the receiver simply never fires.

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
- **Arm `wait_or_force` unconditionally, rather than passing the deadline in.**
  verified: it has three call sites (`runtime.rs:1705`, `:1718`, `:1798`) and two of them
  are `settle_leases_after_child_crash`, reached from `restart_after_child_exit`
  (`:771-796`) during ordinary operation with no shutdown in progress. An unconditional
  arm would fold a `Forced` settlement into `progress.forced` after one slow worker-crash
  settlement and terminate a healthy agent with a shutdown-deadline error.
- **Put the settlement budget in `wait_for_coordinators` instead.** It is one arm
  mirroring the signal arm already there, and it needs no change to `LeaseSettlement` or
  `CoordinatorExit`. verified: that function joins whole coordinator tasks, and a
  coordinator returns only after both `settle_leases_for_shutdown` and `shutdown_all`
  (`:742-747`), so the arm fires on wall clock during the child reap. With
  `shutdown_grace_seconds` above 10 (the validator accepts 1..=60, `config.rs:162-163`),
  a worker that uses its grace, and a fully healthy control plane, a routine stop is
  marked forced. verified: `main.rs:17-21` returns `Result<(), VoomError>`, so that stop
  then exits `ExitCode::FAILURE` where it exits 0 today — a unit in `failed` on every
  `systemctl stop`, with the write itself untouched. judgment: cheaper types are not
  worth a false failure on the ordinary path.
- **Let a deadline force skip the deactivation, as a signal force does.** The smallest
  edit at `runtime.rs:315` (`if progress.forced.is_some()`). judgment: a timer expiring
  is not the operator saying stop, and the write is what the change exists to protect;
  attempting it costs a bounded 10 s.
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
