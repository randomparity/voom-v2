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

One wall-clock deadline covers both control-plane waits in the tail. ADR 0088
records the decision and the alternatives; this section states what gets built.

### The deadline value

```rust
/// Wall-clock budget for every control-plane wait in the shutdown tail.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(20);
```

A constant, not configuration. Total shutdown becomes
`SHUTDOWN_DEADLINE + shutdown_grace_seconds`; the configuration validator holds
`shutdown_grace_seconds` to 1..=60 (`crates/voom-node-agent/src/config.rs:159`),
so the worst case is 80 s and the runbook's example configuration
(`shutdown_grace_seconds = 10`) gives 30 s. No existing configuration becomes
invalid, because no configuration field changes.

20 s is derived from `HANG_GUARD`, the 30 s bound the lifecycle suite applies to
the agent join and the `Retired` observation together
(`crates/voom-node-agent/tests/lifecycle.rs:47`, [ADR
0066](../../adr/0066-observe-graceful-shutdown-as-one-bounded-lifecycle.md)),
which this charter forbids raising. A deadline at or near 30 s would leave the
guard firing first — the bound would move and the symptom would not. Issue #452
measured loaded passing runs of that suite at 6–9 s, so 20 s clears a healthy
shutdown by a wide margin and leaves the guard room for that progress plus a 1 s
reap. It is also below one `REQUEST_TIMEOUT` (30 s) on purpose: a shutdown
blocked on a wedged control plane should abandon rather than spend another
attempt.

### Where the deadline is captured

`tokio::time::Instant`, so a paused-clock test advances it. Captured once at
each point a shutdown tail begins:

- `run_with_seeded_shutdowns`, immediately after `exit` is determined and before
  `wait_for_coordinators` — this single instant covers coordinator settlement,
  child reaping, and the deactivation that follows.
- Each startup-failure path that already calls `deactivate_or_second_signal`
  (children failed to start; readiness marking failed) captures its own at the
  point of failure. These are not the `SIGTERM` tail, but they call the same
  function for the same reason and were unbounded for the same reason.

### What expiry does

Expiry fires the force that a second signal already fires.

`wait_for_coordinators` gains one select arm, guarded so it fires once rather
than spinning on an elapsed deadline:

```rust
() = tokio::time::sleep_until(deadline), if forced.is_none() => {
    forced = Some(ShutdownForce::Deadline);
    let _ = shutdown.send(ShutdownKind::Forced);
}
```

The existing `signals.recv()` arm sets `forced = Some(ShutdownForce::Signal)`.
Sending `ShutdownKind::Forced` on the watch — rather than aborting the
coordinator tasks — is load-bearing: `wait_or_force` inside
`settle_leases_for_shutdown` observes it and abandons the control-plane wait,
while `ChildSupervisor::shutdown_all` still runs to completion inside
`shutdown_grace_seconds`. Aborting the tasks would drop the reap and orphan the
worker processes.

`deactivate_or_second_signal` gains one select arm:

```rust
() = tokio::time::sleep_until(deadline) => return Err(shutdown_deadline_error()),
```

### What the operator sees

The two waits report through different mechanisms because they do not share a
channel. `ShutdownProgress` is produced only by `wait_for_coordinators` and read
only at `runtime.rs:315`, so it carries the settlement wait's cause and nothing
else — `forced` changes from `bool` to `Option<ShutdownForce>`:

```rust
enum ShutdownForce { Signal, Deadline }
```

`finish_shutdown_lifecycle` maps it: `Signal` keeps today's
`forced_shutdown_error()` ("node-agent shutdown interrupted by a termination
signal"); `Deadline` returns a new `shutdown_deadline_error()` naming the
operation, the bound, and the fix — the control plane did not answer inside the
shutdown deadline.

`deactivate_or_second_signal` never constructs a `ShutdownProgress`; it returns
its error directly, so its deadline arm returns `shutdown_deadline_error()`
itself. The same holds at the two startup-failure call sites, which have no
`ShutdownProgress` on their path at all.

Both errors are `VoomError::ExternalSystemUnavailable`, so the exit code is
unchanged.

No `tracing` call is added. `voom-node-agent` has no `tracing` dependency (only a
dev-dependency on `tracing-subscriber`, added by #592's acceptance sweep), and
the returned error already reaches the operator through the binary's exit path.

### Documentation

`docs/runbooks/operator-node-agent.md` told the operator to "Set the supervisor
stop timeout above the configured shutdown grace". That understates the
requirement now that the total is `shutdown_grace_seconds + 20 s`, and a runbook
that fails when followed is a defect. It gets the arithmetic, the note that a
distribution's default is often below the upstream 90 s (Fedora: 45 s) with the
command to check, and the sentence that a shutdown blocked on an unresponsive
control plane now ends at the deadline rather than waiting for a second signal.

This file was added to the charter's permitted surface by an explicit maintainer
decision on 2026-08-28, recorded in the amended `WORK:SCOPE` annotation on issue
#452. The original surface did not include `docs/runbooks/`.

## Failure modes

| Condition | Behaviour |
|---|---|
| Control plane healthy | Unchanged. Settlement, reap, deactivation, `Retired`/`GracefulShutdown`, `Ok(())`. |
| Control plane unresponsive, no second signal | Settlement forced at the deadline; children reaped inside grace; deactivation **not attempted** — `finish_shutdown_lifecycle` returns at `runtime.rs:315` as soon as `progress.forced` is set, exactly as it does on a second-signal force. `shutdown_deadline_error()`. |
| Settlement completes inside the deadline, deactivation does not | The deactivation arm fires and returns `shutdown_deadline_error()` directly. This is the only path on which that arm is reachable. |
| Control plane unresponsive, second signal arrives first | Unchanged: `forced_shutdown_error()`. |
| Control plane slower than 20 s but healthy | The `Retired` write is lost and TTL expiry reconciles it — the same outcome the second-signal force already produced. Accepted; recorded in ADR 0088's consequences. |
| Deadline expires while a child is mid-reap | The reap completes. The deadline forces through the `ShutdownKind` watch, which settlement observes and reaping does not. |
| Deadline already elapsed when `deactivate_or_second_signal` is entered | The `sleep_until` arm is ready immediately, so deactivation is abandoned without being attempted. Correct: the budget for the tail is spent. |

## Testing

All three new tests live in `crates/voom-node-agent/src/runtime_test.rs`, beside
the existing second-signal tests, using the existing `FakeControlPlane` and its
`deactivate_gate`. `#[tokio::test(start_paused = true)]` is already used in that
file, so the 20 s constant costs no wall-clock time. `check-paused-time-db` is
satisfied: the file references neither `SqlitePool` nor the exact identifier
`ControlPlane`.

| Test | Requirement | Proves |
|---|---|---|
| `shutdown_deadline_abandons_a_blocked_deactivation` | R3, R5 | A gated `deactivate` that never returns yields `shutdown_deadline_error()`, wrapped in a `tokio::time::timeout` well past the deadline so the pre-change build fails on `Elapsed` rather than hanging. |
| `shutdown_deadline_forces_lease_settlement` | R1, R5 | `wait_for_coordinators`, against a coordinator that never settles, returns `ShutdownProgress { forced: Some(ShutdownForce::Deadline), .. }` and publishes `ShutdownKind::Forced`. |
| `a_second_signal_still_outranks_the_shutdown_deadline` | R4 | With a deadline far in the future, a second signal still produces `forced_shutdown_error()` — the deadline did not replace the escape. |

R2 is covered by the existing tests that assert the ordered graceful path; they
must keep passing unmodified apart from the new argument. The existing
second-signal tests (`second_signal_interrupts_deactivation_only_after_reap`,
`second_signal_interrupts_a_non_graceful_deactivation`,
`restart_exhausted_deactivation_requires_a_genuine_second_signal`) get a
far-future deadline so the signal remains the cause under test.

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

The one property worth naming: the deadline makes the existing forced-shutdown
path reachable without an operator signal. Anything that can keep the control
plane from answering for 20 s can now cause a `Retired` write to be skipped. That
same party could already cause it by keeping the control plane from answering for
154 s, and the resulting state — an incarnation reconciled by TTL expiry — is
unchanged. No new state and no new reachability; a shorter time to reach an
outcome that was already reachable.

## AI surfaces

None. No LLM call, prompt, retrieval path, classifier, agent loop, tool-use
chain, or model configuration is added or modified.
