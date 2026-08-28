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

"Unbounded" here means bounded only by the client's retry policy, roughly 154 s
per logical call (`production_request_budget`, `crates/voom-node-agent/src/client.rs:450`),
and the tail makes one settlement call per held lease before it makes the
deactivation call.

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
| R1 | A single shutdown signal makes the shutdown tail complete or abandon within a bounded, documented wall-clock deadline shorter than systemd's upstream 90 s `DefaultTimeoutStopSec`. | charter criterion 1 |
| R2 | When deactivation completes inside the deadline, the incarnation still reaches `Retired` with `GracefulShutdown`, and the settlement → child-reaping → deactivation ordering is preserved. | charter criterion 2 |
| R3 | When the deadline expires, the agent exits promptly and reports the missed deactivation as an error naming the deadline, not a signal that did not arrive. | charter criterion 3; `AGENTS.md` Rule 12 |
| R4 | The second-signal force path keeps working unchanged. | charter criterion 4 |
| R5 | A deterministic regression fails against the pre-change behaviour. | charter criterion 5; `AGENTS.md` Rule 9 |
| R6 | `just ci` is green. | charter criterion 6 |

Out of scope, per the charter's exclusions: any control-plane or `voom-store`
change (the write-lock deadlock is #592, merged); any HTTP API, schema,
migration, authentication, or worker-protocol change; raising the lifecycle
suite's `HANG_GUARD` (#446).

## Design

Each control-plane wait in the tail gets its own wall-clock budget. ADR 0088
records the decision and the alternatives; this section states what gets built.

### The two constants

```rust
/// Wall-clock budget for one control-plane wait in the shutdown tail.
const SHUTDOWN_CALL_DEADLINE: Duration = Duration::from_secs(10);

/// Bound on collecting a killed child's exit status.
const REAP_AFTER_KILL: Duration = Duration::from_secs(1);
```

Constants, not configuration. Total shutdown becomes
`2 × SHUTDOWN_CALL_DEADLINE + shutdown_grace_seconds + REAP_AFTER_KILL`; the
validator holds `shutdown_grace_seconds` to 1..=60
(`crates/voom-node-agent/src/config.rs:159`), so the worst case is 81 s and the
runbook's example (`shutdown_grace_seconds = 10`) gives 31 s. No existing
configuration becomes invalid, because no configuration field changes.

10 s is derived from `HANG_GUARD`, the 30 s bound the lifecycle suite applies to
the agent join and the `Retired` observation together
(`crates/voom-node-agent/tests/lifecycle.rs:47`, [ADR
0066](../../adr/0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md)),
which this charter forbids raising: a budget near 30 s would leave the guard
firing first. Two of them plus the maximum grace and `REAP_AFTER_KILL` also keep
the tail inside systemd's upstream 90 s. It is below one `REQUEST_TIMEOUT`
(30 s) on purpose — a shutdown blocked on a wedged control plane should abandon
rather than spend an attempt.

### Where each budget is applied

Each budget goes where its wait is, and the placement is the load-bearing part of
the design.

**Settlement — `wait_or_force` (`runtime.rs:1744`), not `wait_for_coordinators`.**
`wait_or_force` is the function `settle_leases_for_shutdown` blocks in, so a
budget there times the control-plane wait and nothing else. A budget in
`wait_for_coordinators` would not: it joins whole coordinator tasks, and a
coordinator returns only after `settle_leases_for_shutdown` *and*
`ChildSupervisor::shutdown_all` (`:742-747`). With `shutdown_grace_seconds = 30`,
a worker that uses its grace, and a healthy control plane, such a budget fires on
the reap, sets `forced`, and loses the `Retired` write — #452's exposure
re-created. `wait_for_coordinators` therefore gains no arm at all.

`wait_or_force` gains one, guarded so it fires once:

```rust
() = tokio::time::sleep_until(deadline), if forced.is_none() => { … }
```

Each coordinator computes `Instant::now() + SHUTDOWN_CALL_DEADLINE` when it
observes the shutdown event. They observe it within microseconds of each other,
so the budgets are effectively common without threading an instant through
`spawn_coordinators`.

**Deactivation — `deactivate_or_second_signal`.** One arm:

```rust
() = tokio::time::sleep_until(deadline) => return Err(shutdown_deadline_error()),
```

with its own `Instant::now() + SHUTDOWN_CALL_DEADLINE` captured when the call
begins. The two startup-failure call sites capture theirs the same way; they run
before any shutdown tail exists.

`tokio::time::Instant` throughout, so a paused-clock test advances them.

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

The settlement cause travels further. `wait_or_force` can already distinguish the
two: it breaks out on the `ShutdownKind::Forced` watch for a signal force, and on
its own timer for a deadline. `LeaseSettlement::Forced` carries that as a
`ShutdownForce`:

```rust
enum ShutdownForce { Signal, Deadline }
```

through `CoordinatorExit::Shutdown` into `ShutdownProgress.forced`, which changes
from `bool` to `Option<ShutdownForce>`. `finish_shutdown_lifecycle` maps it at
`runtime.rs:315`: `Some(Signal)` to today's `forced_shutdown_error()`,
`Some(Deadline)` to `shutdown_deadline_error()`. Leaving that site as
`if progress.forced.is_some()` would report a signal on the deadline path, which
is the whole reason the field is widened.

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
`SHUTDOWN_CALL_DEADLINE` is *below* `production_request_budget()` and that the
whole tail stays under systemd's upstream 90 s default — rather than as a rung
that satisfies the ordering.

### Documentation

`docs/runbooks/operator-node-agent.md` told the operator to "Set the supervisor
stop timeout above the configured shutdown grace". That understates the
requirement now that the total is `shutdown_grace_seconds + 21`, and a runbook
that fails when followed is a defect. It gets the arithmetic, the note that a
distribution's default is often below the upstream 90 s (Fedora: 45 s) with the
command to check, and the sentence that a shutdown blocked on an unresponsive
control plane now ends at the deadline rather than waiting for a second signal.

This file and `tests/budget_ladder.rs` were added to the charter's permitted
surface by explicit maintainer decisions on 2026-08-28, recorded in the amended
`WORK:SCOPE` annotations on issue #452. The original surface included neither.

## Failure modes

| Condition | Behaviour |
|---|---|
| Control plane healthy | Unchanged. Settlement, reap, deactivation, `Retired`/`GracefulShutdown`, `Ok(())`. |
| Reap uses the full `shutdown_grace_seconds`, control plane healthy | Unchanged, and this is the case the placement exists for: no budget covers the reap, so deactivation still runs and still retires the incarnation. |
| Settlement blocked, no second signal | Settlement forced at its budget; children reaped inside grace; deactivation **not attempted** — `finish_shutdown_lifecycle` returns at `runtime.rs:315` as soon as `progress.forced` is set, exactly as on a second-signal force. `shutdown_deadline_error()`. |
| Settlement completes, deactivation blocked | The deactivation arm fires at its own budget and returns `shutdown_deadline_error()` directly. |
| Second signal arrives before either budget | Unchanged: `forced_shutdown_error()`. |
| Control plane slower than 10 s but healthy | The `Retired` write is lost and TTL expiry reconciles it — the same outcome the second-signal force already produced. Accepted; recorded in ADR 0088's consequences. |
| Child does not die on `SIGKILL` (uninterruptible sleep) | The post-kill wait is abandoned at `REAP_AFTER_KILL`; `shutdown` returns a `ChildError` naming the child, which its caller discards. The agent exits; the process is reparented to init. |

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
| `shutdown_deadline_forces_blocked_lease_settlement` | `runtime_test.rs` | R1, R5 | `wait_or_force` against leases that never settle returns `Some(ShutdownKind::Forced)` at the budget and `settle_leases_for_shutdown` reports `LeaseSettlement::Forced(ShutdownForce::Deadline)`. |
| `a_settlement_deadline_expiry_reports_the_deadline_not_a_signal` | `runtime_test.rs` | R3 | `finish_shutdown_lifecycle` given `forced: Some(ShutdownForce::Deadline)` returns `shutdown_deadline_error()`, not `forced_shutdown_error()`. The mapping at `runtime.rs:315` is the reason the field is widened, so it gets its own test. |
| `a_second_signal_still_outranks_the_shutdown_deadline` | `runtime_test.rs` | R4 | With a budget far in the future, a second signal still produces `forced_shutdown_error()` — the deadline did not replace the escape. |
| `a_full_shutdown_grace_does_not_trip_the_settlement_deadline` | `runtime_test.rs` | R2 | A coordinator whose settlement is instant but whose reap outlasts `SHUTDOWN_CALL_DEADLINE` still reaches deactivation and retires the incarnation. This is the regression for the defect that moved the budget out of `wait_for_coordinators`. |
| `an_unkillable_child_is_abandoned_at_the_reap_bound` | `child_test.rs` | R1 | A child that survives its grace and cannot be reaped inside `REAP_AFTER_KILL` makes `shutdown` return a `ChildError` naming it rather than pending. An unkillable process cannot be constructed portably, so the test drives the bound with a near-zero `REAP_AFTER_KILL`, proving the timeout is wired rather than proving the kernel case. |
| the shutdown rung | `tests/budget_ladder.rs` | R1 | `SHUTDOWN_CALL_DEADLINE` is below `production_request_budget()` — the named inversion — and `2 × SHUTDOWN_CALL_DEADLINE + 60 + REAP_AFTER_KILL` is under 90 s. |

The existing second-signal tests
(`second_signal_interrupts_deactivation_only_after_reap`,
`second_signal_interrupts_a_non_graceful_deactivation`,
`restart_exhausted_deactivation_requires_a_genuine_second_signal`) get a
far-future budget so the signal remains the cause under test. The five existing
sites that read or construct `ShutdownProgress.forced` (`runtime_test.rs:107`,
`:141`, `:969`, `:1051`, `:1136`) change with the field's type.

R6 is `just ci`, which includes `check-adr-index` — ADR 0088 needs its
`docs/adr/README.md` row in this change.

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
`REAP_AFTER_KILL` means a worker process can outlive the agent that supervised
it, where previously the agent waited forever; the process is reparented to init
and holds whatever resources it held, which is the same exposure a `SIGKILL`ed
agent already produced.

## AI surfaces

None. No LLM call, prompt, retrieval path, classifier, agent loop, tool-use
chain, or model configuration is added or modified.
