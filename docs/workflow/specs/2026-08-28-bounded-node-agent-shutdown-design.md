# Bounded node-agent shutdown — design

Issue: [#452](https://github.com/randomparity/voom-v2/issues/452)
Decision record: [ADR 0088](../../adr/0088-bounded-node-agent-shutdown.md)
Scope charter: issue #452, token `q452-81b0b749`

## Problem

A node agent stopped with `SIGTERM` can wait indefinitely before it exits. The
shutdown tail makes control-plane calls at two points and bounds neither:

```
signal → shutdown_tx.send(kind)
       → wait_for_coordinators()          each coordinator:
                                            settle_leases_for_shutdown()  ← control plane, unbounded
                                            ChildSupervisor::shutdown_all()  ← bounded by shutdown_grace_seconds
       → finish_shutdown_lifecycle()      → node_heartbeat.stop()
                                          → deactivate_or_second_signal()  ← control plane, unbounded
```

"Unbounded" here means bounded only by the client's retry policy, 153.75 s
per logical call (`production_request_budget`, `crates/voom-node-agent/src/client.rs:450`,
asserted at `crates/voom-node-agent/tests/budget_ladder.rs:97-99`).

The only escape is a *second* termination signal. An init system does not send
one: it sends `SIGTERM`, waits its stop timeout, then sends `SIGKILL` — which
skips the deactivation write and leaves the incarnation un-retired. That is the
production exposure issue #452 names.

Issue #592 removed the mechanism that made this reproduce in CI (ADR 0087). It
did not remove the unbounded wait.

## Requirements

Each requirement traces to the frozen scope charter on issue #452.

| # | Requirement | Source |
|---|---|---|
| R1 | A single shutdown signal makes the shutdown tail complete or abandon within a bounded, documented wall-clock deadline shorter than systemd's upstream 90 s `DefaultTimeoutStopSec`. **Residual:** the worst case is 86 s, so on a platform whose default is lower — Fedora ships 45 s — every `shutdown_grace_seconds` above 19 still exceeds it and is still `SIGKILL`ed with the write skipped. Accepted and recorded in ADR 0088; narrowing the validator was considered and rejected there, and the residual is owned by [#597](https://github.com/randomparity/voom-v2/issues/597). | charter criterion 1 |
| R2 | When deactivation completes inside the deadline, the incarnation still reaches `Retired` with `GracefulShutdown`, and the settlement → child-reaping → deactivation ordering is preserved. | charter criterion 2 |
| R3 | When the deadline expires, the agent exits promptly and reports the missed deactivation as an error naming the deadline, not a signal that did not arrive. | charter criterion 3; `AGENTS.md` Rule 12 |
| R4 | The second-signal force path keeps working unchanged **in outcome**. Its latency after a `Deadline` force is already recorded is not: the signal is consumed one budget plus a reap later. Ratified as outcome-only by maintainer decision, 2026-08-28. | charter criterion 4; maintainer ratification 2026-08-28 |
| R5 | A deterministic regression fails against the pre-change behaviour. Only a test that *compiles* against the pre-change tree can supply this: a test naming a new type or parameter fails to build, which is different evidence. | charter criterion 5; `AGENTS.md` Rule 9 |
| R6 | `just ci` is green. | charter criterion 6 |

Out of scope, per the charter's exclusions: any control-plane or `voom-store`
change (the write-lock deadlock is #592, merged); any HTTP API, schema,
migration, authentication, or worker-protocol change; raising the lifecycle
suite's `HANG_GUARD` (#446).

## Design

Each control-plane wait in the tail gets its own wall-clock budget. ADR 0088
records the decision and the alternatives; this section states what gets built.

### The budgets

```rust
/// Wall-clock budgets for the shutdown tail. Threaded as values, not read from
/// constants at the point of use, so tests can shrink them.
#[derive(Debug, Clone, Copy)]
pub struct ShutdownBudgets {
    /// One control-plane wait in the tail.
    pub call: Duration,
    /// Collecting a killed child's exit status.
    pub reap_after_kill: Duration,
    /// Slack the tail backstop adds over the sum of the inner bounds.
    pub backstop_margin: Duration,
}

impl ShutdownBudgets {
    pub const DEFAULT: Self = Self {
        call: Duration::from_secs(10),
        reap_after_kill: Duration::from_secs(1),
        backstop_margin: Duration::from_secs(5),
    };

    /// Upper bound on the whole tail, and the number the runbook publishes.
    #[must_use]
    pub fn tail(&self, grace: Duration) -> Duration {
        self.call * 2 + grace + self.reap_after_kill + self.backstop_margin
    }
}
```

`AgentRuntime` carries one, `DEFAULT` in production, set by a `#[cfg(test)]`
constructor beside the existing `with_client` seam otherwise. **A struct rather
than bare constants is not incidental.** Three existing tests —
`second_signal_interrupts_deactivation_only_after_reap` (`runtime_test.rs:1009`),
`restart_exhausted_deactivation_requires_a_genuine_second_signal` (`:1040`) and
`child_startup_failure_deactivation_requires_a_genuine_second_signal` (`:1085`) —
are plain `#[tokio::test]` on real time, and each gates `deactivate` with a
`Notify` that is never notified: their whole design is that deactivation blocks
until a signal arrives. A 10 s constant read at the point of use would turn every
one of them into a wall-clock race. They get a far-future `call` instead. The
same seam lets the backstop and crash-release tests, which cannot run on a paused
clock (below), use budgets measured in milliseconds.

Total shutdown becomes `budgets.tail(grace)` = `2 × call + grace +
reap_after_kill + backstop_margin`. The validator holds `shutdown_grace_seconds`
to 1..=60 (`crates/voom-node-agent/src/config.rs:159`), so the worst case is 86 s
and the runbook's example (`shutdown_grace_seconds = 10`) gives 36 s. No existing
configuration becomes invalid, because no configuration field changes.

10 s is derived from `HANG_GUARD`, the 30 s bound the lifecycle suite applies to
the agent join and the `Retired` observation together
(`crates/voom-node-agent/tests/lifecycle.rs:47`, [ADR
0066](../../adr/0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md)),
which this charter forbids raising: a budget near 30 s would leave the guard
firing first. It is below one `REQUEST_TIMEOUT` (30 s) on purpose — a shutdown
blocked on a wedged control plane should abandon rather than spend an attempt.
5 s of margin is sized to cover the tail costs that fall outside every inner
bound, enumerated with the backstop below.

### Where each budget is applied

Each budget goes where its wait is, and the placement is the load-bearing part of
the design.

**Settlement — an `Option<Instant>` parameter on `wait_or_force`, supplied by one
call site.**

```rust
async fn wait_or_force(
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    report_any: bool,
    deadline: Option<Instant>,                        // new
) -> Option<(ShutdownKind, Option<ShutdownForce>)>    // was Option<ShutdownKind>
```

The second tuple element is itself optional because `report_any = true` returns
`Some(kind)` for any observed kind *without having forced anything* — leases are
not aborted, and `child_crash_lease_settlement` (`runtime.rs:1725-1733`) maps that
observation to `LeaseSettlement::Completed`. There is no `ShutdownForce` to report
in that case.

Two of the three call sites supply a deadline:

- `:1798`, inside `settle_leases_for_shutdown` — `Some(Instant::now() + budgets.call)`.
- `:1718`, the second wait in `settle_leases_after_child_crash` — also `Some(…)`.
  It looks like a crash-path wait and is not one: the function short-circuits at
  `let observed = observed?;` (`:1714`), so `:1718` is reached only when a
  shutdown is already in flight. It is a shutdown-tail wait, and it reaches the
  retrying client, so leaving it unbounded would be a 153.75 s hole.
- `:1705`, inside `cancel_and_wait` (`:1698-1706`) — `None`. This is the only
  genuinely steady-state site. The parameter threads through `cancel_and_wait`'s
  signature to reach it.

Expiry aborts the lease tasks and returns forced, exactly as observing
`ShutdownKind::Forced` does; it does **not** publish on the watch, so it does not
force coordinators that are not overrunning.

Two placements are ruled out, and each rules itself out for a different reason.

*Arming `wait_or_force` unconditionally* arms `:1705` as well, and that one runs
from `restart_after_child_exit` (`:771-796`) during ordinary operation, so a slow
settlement after one worker crash would fold a `Forced` settlement into
`progress.forced` and terminate a healthy, running agent.

*Putting the budget in `wait_for_coordinators`* — one arm mirroring the signal arm
already there, needing no change to `LeaseSettlement` — arms a wall clock over
whole coordinator tasks. A coordinator does not return until
`settle_leases_for_shutdown` **and** `shutdown_all` have both finished
(`:742-747`), so the arm fires during the child reap. With `shutdown_grace_seconds`
above 10, a worker that uses its grace, and a fully healthy control plane, a
routine stop is marked forced; `main.rs:17-21` returns `Result<(), VoomError>`, so
that stop exits `ExitCode::FAILURE` where it exits 0 today, putting the unit into
`failed` on every `systemctl stop`.

`wait_for_coordinators` therefore gains no arm. Its signal arm and its
`if signals_open && !forced` guard (`:1838`) are unchanged apart from the field's
type becoming `Option<ShutdownForce>`, so `!forced` becomes `forced.is_none()`
with identical semantics: the first force recorded wins, as today.

**Deactivation — `deactivate_or_second_signal`.** One arm:

```rust
() = tokio::time::sleep_until(deadline) => return Err(shutdown_deadline_error()),
```

taking the deadline as a parameter — `deactivate_or_second_signal(..., deadline: Instant)`
— so its callers compute `Instant::now() + budgets.call` and the three existing
real-time second-signal tests can pass a far-future value. The two startup-failure call sites capture theirs the same way; they run
before any shutdown tail exists.

`tokio::time::Instant` throughout, so a paused-clock test advances them.

### The waits that do not observe the shutdown, and the backstop

The per-wait budgets bound the two waits the tail is *supposed* to spend its time
in. They do not bound a wait nobody raced against the shutdown receiver, and
`restart_after_child_exit` (`runtime.rs:772-819`) holds three:

| Wait | Location | Unbounded cost |
|---|---|---|
| `set_worker_readiness(NotReady)` | `:781-790` | `production_request_budget()` = 153.75 s |
| `restart_child` | `:804-806` | `RESTART_LIMIT` = 3 attempts × `startup_deadline.timeout(...)`; `STARTUP_TIMEOUT` 10 s, but `NVIDIA_STARTUP_TIMEOUT` 5 min (`child.rs:22-24`, `:247-250`) — up to ~15 min |
| `set_worker_readiness(Ready)` | `:807-815` | `production_request_budget()` = 153.75 s |

The coordinator does not consult the shutdown receiver again until it returns to
the select at `:703-717`, so a `SIGTERM` landing anywhere in that window is not
observed until the whole sequence drains — and `wait_for_coordinators` waits for
every coordinator. On a node with an NVIDIA worker that crashed just before the
signal, the tail is minutes, whatever the budgets above say.

**Two responses, and both are needed.**

*Race the readiness calls — against a clone of the receiver.* Both
`set_worker_readiness` calls are raced against the shutdown state, the way the
main coordinator loop already races its work at `:704-717`. The mechanism is
specified, not left to the implementer, because the naive one is wrong:

```rust
let mut watcher = shutdown.clone();
tokio::select! {
    result = set_worker_readiness(...) => result?,
    _ = watcher.wait_for(|kind| *kind != ShutdownKind::Running) => { /* abandon */ }
}
```

`changed()` on the original receiver would mark the value seen, and the value is
sent exactly once (`run_with_seeded_shutdowns` at `:290`; `wait_for_coordinators`
re-sends only on a second signal, `:1845`). Consuming it here would strip the
`cancel_and_wait` wait immediately below — `wait_or_force` at `:1705`, whose only
escape is `shutdown.changed()` (`:1755`) and which this design deliberately
leaves unbounded — of its escape, leaving it to block on lease tasks settling
against the same unresponsive control plane. That is the hazard the comment at
`:792-793` already names. A clone tracks its own seen version and `wait_for`
checks the current value first, so the original still observes the change and
`settle_leases_after_child_crash` behaves as it does today.

This changes nothing while no shutdown is in progress, because the predicate
never holds. `restart_child` and the `RESTART_DELAY` before it are covered by the
same race, since a shutdown observed first returns rather than restarting.

*Back the whole tail with a timeout.* Racing the waits I found is a claim that I
found them all, and three review passes each found another. So
`run_with_seeded_shutdowns` wraps its tail — `wait_for_coordinators` through
`finish_shutdown_lifecycle` — in

```rust
tokio::time::timeout(budgets.tail(grace), tail).await
```

returning a distinct `shutdown_backstop_error()` on expiry — distinct because the
settlement arm, the deactivation arm and the backstop otherwise all return
`shutdown_deadline_error()`, and an operator could not then tell a documented
bounded abandon from an unenumerated wait blowing through every inner bound. The
first is expected and in the runbook; the second is a bug report. With no
`tracing` dependency the returned error is the only channel, so the distinction
has to live in it.

Dropping that future drops the coordinator `JoinSet`, which aborts its tasks. The
child a coordinator was reaping is still `SIGKILL`ed, but by `.kill_on_drop(true)`
on the `tokio::process::Child` (`child.rs:404`) — **not** by `RunningChild`'s
`Drop`. `shutdown` moves the handle out at `:195` and does not set `reaped` until
`:211`, so throughout the reap `Drop` takes its early return at `:222-224` and
never reaches its `start_kill()`. `Drop` covers only a coordinator aborted
*before* `shutdown_all`, still owning its handle. Both mechanisms are named here
because `kill_on_drop(true)` reads as redundant beside a killing `Drop`, and
removing it would silently orphan a worker on every backstop expiry.

**`backstop_margin` is what makes this a backstop rather than the mechanism.**
The inner bounds are sequential and sum to `2 × call + grace + reap_after_kill`
exactly, and the tail carries costs outside all of them:

- `wait_or_force`'s post-expiry `leases.abort_all(); wait_for_leases(leases).await`
  (`runtime.rs:1771-1774`) is itself unbounded, and runs after the 10 s it just spent;
- `ChildSupervisor::shutdown_all` spawns each `shutdown` into a `JoinSet`
  (`child.rs:356-361`), so `grace` starts on first poll after the spawn, not at the call;
- `finish_shutdown_lifecycle` stops the heartbeat and unwraps `settled?` before
  deactivating.

Sized at the bare sum, the backstop and the last inner bound expire at the same
instant and the backstop wins — cancelling the deactivation it exists to protect,
which is the same loss the design refused when it rejected letting a deadline
force skip deactivation. The margin buys every inner bound the right to expire
first.

### What a deadline force does at `runtime.rs:315`

`finish_shutdown_lifecycle` returns before deactivating on any force today:

```rust
if progress.forced {
    return Err(forced_shutdown_error());
}
```

The site branches instead:

- `Some(ShutdownForce::Signal)` — return `forced_shutdown_error()`, exactly as
  today. The operator said stop; charter criterion R4 holds this unchanged.
- `Some(ShutdownForce::Deadline)` — fall through to the deactivation, which has
  its own budget, then return `shutdown_deadline_error()`. A timer expiring is
  not an instruction to skip the write, and attempting it costs a bounded 10 s.
  The error is honest because `Deadline` here means settlement itself overran and
  leases were abandoned mid-settlement.
- `None` — unchanged.

### The reap's own bound

`RunningChild::shutdown` (`crates/voom-node-agent/src/child.rs:193-213`) applies
`grace` only to the polite wait at `:199`. On expiry it calls `start_kill()` at
`:204` and then `child.wait().await` at `:207` with **no timeout**, so a child the
kernel cannot kill — a worker parked in uninterruptible sleep on a hung mount —
leaves the whole tail pending. The post-kill wait is wrapped in
`tokio::time::timeout(REAP_AFTER_KILL, child.wait())`. Abandoning it orphans
nothing `SIGKILL` had not already claimed: the child is doomed and the wait only
collects its exit status, so the process is reparented to init. On expiry
`shutdown` returns the `ChildError` it already returns for the other failures on
this path, naming the unreaped child.

That error goes nowhere. Both shutdown-path callers discard it
(`let _ = supervisor.shutdown_all(vec![child]).await`, `runtime.rs:746` and
`:796`) and `child.rs` logs nothing — `voom-node-agent` has no `tracing`
dependency. An unreaped child is therefore silent, which ADR 0088 records as an
accepted observability regression against the unbounded wait it replaces.
Changing the discard would change coordinator exit semantics for every existing
failure on that path and is not done here.

### What the operator sees

Reporting a deadline expiry as "interrupted by a termination signal" would send
an operator looking for a signal nobody sent, so each wait reports its own cause.

`deactivate_or_second_signal` returns its error directly, so its arm returns a new
`shutdown_deadline_error()` naming the operation and the bound.

The settlement cause travels with the settlement result. `wait_or_force` already
distinguishes the two ways it can break out — the `ShutdownKind::Forced` watch,
or (now) its own timer — so it returns that as a `ShutdownForce`:

```rust
enum ShutdownForce { Signal, Deadline }
```

`LeaseSettlement::Forced` carries it, through `CoordinatorExit::Shutdown` into
`ShutdownProgress.forced`, which changes from `bool` to `Option<ShutdownForce>`.
No precedence rule is introduced: the signal arm's existing guard already
disables it once anything has forced, so the first force recorded wins exactly as
it does today.

Both errors are `VoomError::ExternalSystemUnavailable`, so the exit code is
unchanged. No `tracing` call is added; the returned error reaches the operator
through the binary's exit path.

### The budget ladder

`crates/voom-node-agent/tests/budget_ladder.rs` asserts that an observer's budget
exceeds the budget of what it observes, "because an observer that expires no
later than what it observes reports a timeout of its own instead of the failure
underneath it (see #592)". A 10 s budget observing a 153.75 s
`production_request_budget()` inverts that rule.

The inversion is deliberate and scoped: during shutdown the agent's obligation is
to exit, and the failure underneath is one it can no longer act on. The file's
module docs already name #452 as the exposure that keeps `REQUEST_TIMEOUT` out of
the ladder, and its closing rule is that "adding a rung means adding it here too.
A layer absent from this file is a layer nobody is checking." So the shutdown
budgets are added there as a named inversion with its own assertion — that
`budgets.call` is *below* `production_request_budget()` and that the
whole tail stays under systemd's upstream 90 s default — rather than as a rung
that satisfies the ordering.

### Documentation

`docs/runbooks/operator-node-agent.md` told the operator to "Set the supervisor
stop timeout above the configured shutdown grace". That understates the
requirement now that the total is `shutdown_grace_seconds + 26`, and a runbook
that fails when followed is a defect. It gets the arithmetic, the note that a
distribution's default is often below the upstream 90 s (Fedora: 45 s) with the
command to check, and the sentence that a shutdown blocked on an unresponsive
control plane now ends at the deadline rather than waiting for a second signal.

The runbook edit is already committed on this branch, ahead of the code it
describes; both land in the same pull request, and the constants it names —
`budgets.call`, `budgets.reap_after_kill` — must exist by the time it merges.
That is a check for the branch review, not a licence to merge the doc alone.

This file and `tests/budget_ladder.rs` were added to the charter's permitted
surface by explicit maintainer decisions on 2026-08-28, recorded in the amended
`WORK:SCOPE` annotations on issue #452. The original surface included neither.

## Failure modes

| Condition | Behaviour |
|---|---|
| Control plane healthy | Unchanged. Settlement, reap, deactivation, `Retired`/`GracefulShutdown`, `Ok(())`. |
| Reap uses the full `shutdown_grace_seconds`, control plane healthy | Unchanged, and this is the case the placement exists for: no budget covers the reap, nothing is forced, the incarnation retires, and the process exits 0. |
| Settlement blocked, no second signal | Settlement forced at its budget with cause `Deadline`; leases abandoned; children reaped inside grace; deactivation **is** attempted under its own budget, so the incarnation still retires if the control plane answers. Returns `shutdown_deadline_error()` either way. |
| Settlement completes, deactivation blocked | The deactivation arm fires at its own budget and returns `shutdown_deadline_error()` directly. |
| Second signal arrives before either budget | Unchanged: `forced_shutdown_error()`. |
| Deactivation slower than 10 s but healthy | The `Retired` write is lost and TTL expiry reconciles it — the same outcome the second-signal force already produced. Accepted; recorded in ADR 0088's consequences. |
| Worker crashes, then a shutdown arrives while its readiness update is blocked | `restart_after_child_exit`'s readiness calls are released by the shutdown receiver instead of draining the retry budget. |
| Worker crashes, shutdown arrives mid-restart of an NVIDIA worker | Same race releases it; the 5-minute startup timeout is never entered, because a shutdown observed first returns instead of restarting. |
| Second signal arrives after a deadline force is already recorded | The signal arm is disabled by its existing `!forced` guard, so the signal is consumed later in `deactivate_or_second_signal`. Outcome unchanged (`forced_shutdown_error()`, write skipped); latency is up to one budget plus a reap — 71 s at the maximum grace. R4's "unchanged" is ratified as outcome-only by explicit maintainer decision on 2026-08-28; the alternative (guarding on `forced != Some(Signal)`) was offered and declined. |
| `shutdown_grace_seconds` above 19 on a 45 s stop timeout | The tail can exceed the platform default, `SIGKILL` lands, the write is skipped. #452's exposure, unfixed, inside the accepted configuration range. The runbook publishes the arithmetic so an operator can avoid it; narrowing the validator so no operator action is needed is #597. |
| An unraced wait not enumerated here | The tail backstop fires at the published total, aborts the coordinators (children `SIGKILL`ed by `.kill_on_drop(true)`, `child.rs:404` — **not** by `RunningChild::Drop`), and returns the distinct `shutdown_backstop_error()`. |
| Child does not die on `SIGKILL` (uninterruptible sleep) | The post-kill wait is abandoned at `budgets.reap_after_kill`; `shutdown` returns a `ChildError` naming the child, which its caller discards. The agent exits; the process is reparented to init. |

## Testing

The runtime tests live in `crates/voom-node-agent/src/runtime_test.rs`, beside
the existing second-signal tests, using the existing `FakeControlPlane` and its
`deactivate_gate`. `#[tokio::test(start_paused = true)]` is already used in that
file, so the 10 s constant costs no wall-clock time. `check-paused-time-db` is
satisfied: the file references neither `SqlitePool` nor the exact identifier
`ControlPlane`.

| Test | File | Requirement | Proves |
|---|---|---|---|
| `shutdown_deadline_abandons_a_blocked_deactivation` | `runtime_test.rs` | R3, R5 | A gated `deactivate` that never returns yields `shutdown_deadline_error()`, wrapped in a `tokio::time::timeout` well past the budget so the pre-change build fails on `Elapsed` rather than hanging. |
| `shutdown_deadline_forces_blocked_lease_settlement` | `runtime_test.rs` | R1, R5 | `wait_or_force` with a deadline, against leases that never settle, aborts them and reports `Deadline`; `settle_leases_for_shutdown` returns `LeaseSettlement::Forced(ShutdownForce::Deadline)`. |
| `a_crashed_worker_release_does_not_wait_out_the_retry_budget` | `runtime_test.rs` | R1, **R5** | With `set_worker_readiness` gated the way `deactivate_gate` gates deactivation, a crashed child and then a shutdown releases `restart_after_child_exit` instead of draining `production_request_budget()`. This is the regression for the unobserved coordinator wait. |
| `the_crash_settlement_path_is_not_armed` | `runtime_test.rs` | R1 | `settle_leases_after_child_crash` with a settlement far longer than `budgets.call` completes normally and does not force. This is the regression for the steady-state contamination that kept the budget out of `wait_or_force`. |
| `a_settlement_deadline_expiry_reports_the_deadline_not_a_signal` | `runtime_test.rs` | R3 | `finish_shutdown_lifecycle` given `forced: Some(ShutdownForce::Deadline)` returns `shutdown_deadline_error()`, not `forced_shutdown_error()`. The mapping at `runtime.rs:315` is the reason the field is widened, so it gets its own test. |
| `a_second_signal_still_outranks_the_shutdown_deadline` | `runtime_test.rs` | R4 | With a budget far in the future, a second signal still produces `forced_shutdown_error()` — the deadline did not replace the escape. |
| `a_second_signal_after_a_deadline_force_still_forces` | `runtime_test.rs` | R4 | The deadline-then-signal ordering the previous test cannot reach: a `Deadline` force is recorded, then a second signal arrives, and the run still ends in `forced_shutdown_error()` with the write skipped. Asserts the outcome R4 protects, not the latency, which the failure-mode table records as changed. |
| `the_tail_backstop_bounds_an_unraced_wait` | `runtime_test.rs` | R1 | A coordinator parked in a wait no receiver races still lets `run_with_seeded_shutdowns` return `shutdown_deadline_error()` at `budgets.tail(grace)`. Drives the backstop directly rather than relying on the enumeration above being complete. |
| `a_full_shutdown_grace_is_not_a_forced_shutdown` | `runtime_test.rs` | R2 | A coordinator whose settlement is instant but whose reap outlasts `budgets.call` is **not** forced: the incarnation retires and `run_with_seeded_shutdowns` returns `Ok(())`. Nothing times the reap, and a routine stop must not exit non-zero. |
| `a_deadline_forced_settlement_still_deactivates` | `runtime_test.rs` | R2, R3 | `finish_shutdown_lifecycle` given `forced: Some(ShutdownForce::Deadline)` deactivates, retires the incarnation, and *then* returns `shutdown_deadline_error()`. |
| `an_unkillable_child_is_abandoned_at_the_reap_bound` | `child_test.rs` | R1 | A child that survives its grace and cannot be reaped inside the bound makes `shutdown` return a `ChildError` naming it rather than pending. An unkillable process cannot be constructed portably, so the test drives a near-zero bound instead, proving the timeout is wired rather than proving the kernel case. That needs a seam: `ChildSupervisor::with_timeouts` (`child.rs:276`, already `#[cfg(all(test, target_os = \"linux\"))]`) gains a reap parameter, and `RunningChild::shutdown` takes the bound as an argument beside `grace` rather than reading the constant directly. |
| `the_backstop_error_names_a_defect` | `runtime_test.rs` | R3 | The backstop returns `shutdown_backstop_error()`, distinct from `shutdown_deadline_error()`, so an operator can tell an unenumerated wait from a documented abandon. |
| the shutdown rung | `tests/budget_ladder.rs` | R1 | `budgets.call` is below `production_request_budget()` — the named inversion — and `2 × SHUTDOWN_CALL_DEADLINE + 60 + REAP_AFTER_KILL` is under 90 s. `ShutdownBudgets` is `pub` with a `pub const DEFAULT`, so this integration test reads it the way it already reads `client::REQUEST_TIMEOUT`. |

**R5's witnesses are the two tests that compile against the pre-change tree.**
`shutdown_deadline_abandons_a_blocked_deactivation` is *not* one of them: the plan
gives `deactivate_or_second_signal` a `deadline` parameter, and that test passes it
as an argument, so pre-change it does not build. The witnesses are:

- `a_sigterm_exits_the_agent_when_the_control_plane_never_answers`, driven through
  `AgentRuntime::with_client` and `run_until` — both pre-existing — taking the
  **default** budgets and wrapped in an outer `tokio::time::timeout` well above the
  86 s tail. Pre-change the gated deactivation never returns and the test fails on
  `Elapsed`; post-change it returns the deadline error at about 10 s. This is the
  one that covers the charter's headline behaviour.
- `a_crashed_worker_release_does_not_wait_out_the_retry_budget`, existing API plus
  one new gate on the fake; pre-change it drains the retry budget and exits
  `CoordinatorExit::Fatal`.

Every other test names a type or parameter this change introduces, so pre-change it
does not build — real coverage of the new behaviour, but not evidence for R5.

The existing second-signal tests
(`second_signal_interrupts_deactivation_only_after_reap`,
`second_signal_interrupts_a_non_graceful_deactivation`,
`restart_exhausted_deactivation_requires_a_genuine_second_signal`) get a
far-future budget so the signal remains the cause under test. Two type changes ripple beyond the runtime. `ShutdownProgress.forced` becoming
`Option<ShutdownForce>` touches `runtime_test.rs:107`, `:141`, `:969`, `:1051`
and `:1136`. `LeaseSettlement::Forced` gaining a payload touches
`child_crash_lease_settlement`'s construction of it (`runtime.rs:1725-1733`) and
`runtime_test.rs:921`, `:947`, `:955`, `:1002`. The crash path is unarmed, so
every construction there is `ShutdownForce::Signal`. The compiler finds all of
them; they are listed so the change's size is not a surprise mid-build.

R6 is `just ci`. Two things about it are specific to this change.

`check-adr-index` is included, so ADR 0088 needs its `docs/adr/README.md` row —
already present on this branch.

The new budgets are real time in any suite that does not pause the clock, and
two of the specified tests cannot pause it. `the_tail_backstop_bounds_an_unraced_wait`
and `a_crashed_worker_release_does_not_wait_out_the_retry_budget` both need a real
coordinator, which means a `RunningChild` from `ChildSupervisor::start_all` and
therefore a real process; every existing test in `runtime_test.rs` that does this
(`child_crash_restarts_only_after_every_held_lease_settles` at `:634-648`,
`graceful_shutdown_settles_before_child_reap_and_deactivation` at `:744-760`) is
`#[cfg(unix)] #[tokio::test]` on real time via `ProcessWorkerFixture`. That is
precisely why the budgets are a struct: those two set `call`,
`reap_after_kill` and `backstop_margin` in the tens of milliseconds, so they cost
well under a second rather than the 26 s the defaults would.

Everything else pauses. `runtime_test.rs` already uses
`#[tokio::test(start_paused = true)]` widely, and the three existing real-time
second-signal tests take a far-future `call`, so none of them gains a timer.
`tests/lifecycle.rs` runs on real time under a 30 s `HANG_GUARD` and arranges no
blocked control-plane wait, so the budgets never elapse there.

Expected added `just test` wall clock is therefore **about 10 seconds**, almost
all of it the one R5 witness that must take the default budgets to compile
against the pre-change tree; everything else is under a second. That is a
prediction to check on the first real run, not an assumption: if it moves
further, the cause is another suite reaching a budget it was not meant to reach,
or a new test that took the defaults by mistake.

## Threat model

Not required, and the reason is stated rather than left silent. The change adds
no entry point and widens no existing one; touches no authentication,
authorization, session, or tenancy logic; handles no secret and edits no CI
configuration; parses no input it did not produce; builds no command, query,
path, URL, or template from a non-literal; widens no permission grant; and
changes no dependency, lockfile, or pinned action. The one default it changes is
a shutdown timeout, which is a liveness bound rather than a security control, and
it shortens a wait rather than extending one.

Two properties worth naming. The budgets make the existing forced-shutdown path
reachable without an operator signal, so anything that can keep the control plane
from answering for 10 s can now cause a `Retired` write to be skipped. That same
party could already cause it by keeping the control plane from answering for
153.75 s, and the resulting state — an incarnation reconciled by TTL expiry — is
unchanged; a shorter time to reach an outcome that was already reachable. And
`budgets.reap_after_kill` means a worker process can outlive the agent that supervised
it, where previously the agent waited forever; the process is reparented to init
and holds whatever resources it held, which is the same exposure a `SIGKILL`ed
agent already produced.

## AI surfaces

None. No LLM call, prompt, retrieval path, classifier, agent loop, tool-use
chain, or model configuration is added or modified.
