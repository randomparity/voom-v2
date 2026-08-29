# Heartbeat watchdog tie classification — design

Issue: [#590](https://github.com/randomparity/voom-v2/issues/590)
Decision record: [ADR 0089](../../adr/0089-heartbeat-watchdog-wins-equal-deadlines.md)
Scope charter: issue #590, token `q590-8eef885a`

## Problem

`consume_dispatch_stream` creates separate heartbeat and progress timer futures.
Although its `tokio::select!` is biased toward heartbeat, a full parallel suite
observed an equal, already-expired pair classified as `progress_timeout`. The
same elapsed pair can also be checked through `fail_if_watchdog_elapsed`, which
already tests heartbeat first. Production has two mechanisms for one decision.

The pair is not currently equal in absolute time: stream startup assigns
`last_progress` and `last_heartbeat` from consecutive clock reads. With equal
durations, progress starts first. Settling tie semantics therefore also requires
one shared stream-start instant.

## Requirements

| # | Requirement | Source |
|---|---|---|
| R1 | An exact tie classifies as heartbeat `worker_timeout`. | campaign criterion; issue #590 preferred contract |
| R2 | A strictly earlier progress deadline is `progress_timeout`; a strictly earlier heartbeat deadline is `worker_timeout`, including delayed synchronous checks that observe both elapsed. | campaign failure-ordering criterion; ADR 0089 explicit chronological-order decision |
| R3 | Timer and frame-arrival paths use one deterministic classification rule based on one captured `Instant`, and startup initializes both last-observed instants from one captured instant. | necessary consequence of R1 and production determinism |
| R4 | Regression coverage deterministically fails against the former separate-timer design. | campaign regression criterion |
| R5 | `just ci` passes. | campaign guardrail |

No API, persistence, schema, migration, dependency, event-ordering, transaction,
or worker-protocol change is authorized.

## Design

Add a small side-effect-free production seam in `leases.rs` that returns the
next absolute watchdog deadline and its failure class. It accepts last
heartbeat/progress instants and timing options, selects the earlier deadline,
and selects heartbeat on equality. A second helper accepts captured `now` and
returns that class only when the selected deadline is elapsed.
Deadline comparison before the elapsed check preserves strict ordering even if
executor load delays polling until both deadlines have passed.

`fail_if_watchdog_elapsed` calls the elapsed helper once and preserves its existing
failure side effect and error text. This deliberately changes only the helper's
late-check case where unequal deadlines are both elapsed: the earlier absolute
deadline replaces the former unconditional heartbeat-first result.
`consume_dispatch_stream` replaces the two timer branches with one inline sleep
at the deadline returned by the shared selector; the branch then calls the
existing failure helper. The stream/frame
branch stays first and all mutation, frame validation, heartbeat, and terminal
handling order remains unchanged.

At entry, `consume_dispatch_stream` captures `stream_started = Instant::now()`
once and initializes both `last_progress` and `last_heartbeat` from it. Later
updates remain independent, preserving the existing observation semantics.

## Testing

Unit tests for the classifier use paused-free constructed `Instant` values and
prove all three orderings: heartbeat earlier, progress earlier, and equal. The
strict-order cases capture `now` after both deadlines to prove late polling does
not invert the winner. The
equal test is the biting regression: the former timer-branch implementation has
no shared classifier and cannot satisfy this deterministic unit contract.

The existing executor integration test continues to prove the persisted failure
class and, because production now uses a shared initial instant, represents an
actual equal-deadline case. A controlled fault that deliberately initializes
`last_heartbeat` one nanosecond after `last_progress` must make that regression
red as `progress_timeout`; unlike consecutive clock reads, the explicit offset
does not depend on clock resolution. Repeating the corrected test exercises the
full asynchronous path; `just ci` covers the workspace and guardrails.

The direct deadline-selection tests are the deterministic pre-fix proof for the
production seam: before the change, the symbol does not exist and the focused
test must fail with that missing-symbol diagnostic. The existing executor
regression covers the asynchronous production path and persisted class; its
explicit one-nanosecond controlled fault proves the shared-start wiring bites.

## Durable execution context

- Branch: `feat/watchdog-tie-590`
- Base branch: `main`
- Guardrail: `just ci`
- ADR index coupling: coupled; this branch adds the 0089 index row.
