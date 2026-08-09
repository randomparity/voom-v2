# ADR 0066: Observe graceful shutdown as one bounded lifecycle

## Status

Accepted

## Context

The black-box node-agent lifecycle test separately waited for the second agent task to exit
and then polled its durable incarnation state. Under CPU oversubscription the join exposed
an indefinitely pending graceful shutdown, but a bare timeout originally attributed the
failure to the fenced first agent. PR #454 corrected that diagnostic, and PR #460 removed
the production client's unlimited retry path that could keep deactivation pending forever.

The regression still needs to prove both facts that define lifecycle completion: the
runtime future terminates and the control plane records the incarnation as `Retired` with
`GracefulShutdown`. Either observation alone is incomplete, and increasing the timeout
only hides a hang.

## Decision

After the test requests graceful shutdown, it awaits the second agent join and the newest
incarnation's `Retired` state as one operation bounded by the existing 30-second
`HANG_GUARD`. It retains the independent assertions for the terminal reason, the prior
incarnation's superseded state, and retirement of every declared worker.

The timeout diagnostic identifies the second-agent graceful-shutdown path, records which
of the join and durable-state witnesses completed, and includes already-observed request
paths. Diagnostic collection must itself remain bounded.

The fix is verified by deliberately withholding the second shutdown signal so the focused
test fails through this guard, and by running concurrent copies under CPU oversubscription.
The contention exercise is release evidence, not a permanent oversubscribing CI loop.

## Consequences

The test fails if graceful deactivation never records retirement, if the runtime remains
pending after recording it, or if either completes outside the hang bound. Failure output
locates the missing witness without replacing production-path observations with hooks.

The wall-clock guard remains sensitive to a host that receives no scheduling time for 30
seconds. That is intentional: no finite timeout can distinguish total host starvation from
a hung task, while the separate contended-run evidence establishes headroom on the target
development host.

No production interface, runtime ordering, persistent schema, or default timeout changes.

## Considered & rejected

- Increase or retry the guard. The reproduced failures consume every tested bound exactly,
  so this delays the same report without proving progress.
- Add an injectable production shutdown observer. It adds a single-use seam and tests the
  observer instead of the real runtime and control-plane boundary.
- Observe only durable `Retired` state. Code after deactivation could still leave the task
  pending, so this drops the failure #446 actually observed.
- Observe only the task join. This cannot prove the durable terminal state or retirement
  reason that graceful shutdown promises.
