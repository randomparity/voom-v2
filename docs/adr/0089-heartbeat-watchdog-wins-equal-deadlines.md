# 0089 — Heartbeat watchdog wins equal deadlines

## Status

Accepted (2026-08-29)

## Context

The workflow stream consumer independently awaits heartbeat and progress-idle
deadlines. Both failures are operationally distinct: heartbeat expiry is
`worker_timeout`, while progress-idle expiry is `progress_timeout`. When both
deadlines are equal and elapsed, separate timer futures can become ready in an
order determined by runtime scheduling, so production classification and the
test in issue #590 can disagree under load.

The existing synchronous watchdog check already tests heartbeat before
progress. The asynchronous timer path must express the same ordering without
changing which non-equal deadline expires first.

At stream startup, `last_progress` and `last_heartbeat` are currently captured
by separate `Instant::now()` calls. Equal configured durations therefore do not
create equal absolute deadlines: progress starts slightly earlier and should
win chronological classification. A real tie requires one shared starting
instant.

## Decision

Derive both absolute watchdog deadlines and await one timer for their minimum.
Classification first selects the earlier absolute deadline; equality selects
heartbeat. It then returns that class only when the selected deadline is
elapsed at one captured `Instant`. Therefore an exact tie is always a heartbeat
`worker_timeout`, while a strictly earlier progress deadline remains
`progress_timeout` and a strictly earlier heartbeat deadline remains
`worker_timeout`, even when executor load delays polling until both are elapsed.

Initialize both last-observed instants from one captured stream-start instant.
Later progress and heartbeat observations continue updating only their own
instant, so equality after startup occurs only when their effective absolute
deadlines genuinely match.

Use the same classifier for the timer path and the synchronous check performed
before accepting a frame. The classifier is deterministic and side-effect free;
the caller retains the existing lease-failure operation and error construction.

## Consequences

- Operators receive stable timeout classes for equal deadlines.
- Equal configured startup durations produce equal absolute deadlines rather
  than inheriting call-order skew from two clock reads.
- Timer polling order no longer defines production behavior.
- Equal absolute deadlines gain explicit heartbeat precedence. For unequal
  deadlines, the earlier deadline wins even when a delayed synchronous check
  observes both elapsed; this deliberately replaces that check's former
  heartbeat-first ordering with chronological ordering shared by the timer path.
- Focused tests can exercise classification directly without depending on
  scheduler load or wall-clock races, including a delayed wake after both
  unequal deadlines have elapsed.

## Considered & rejected

- **Keep separate biased `select!` branches.** verified: issue #590 records a
  full-suite run where the progress branch won despite the intended heartbeat
  precedence; scheduler polling is not a durable operational contract.
- **Accept either timeout class in the regression.** judgment: this would hide
  nondeterministic production diagnostics instead of settling them.
- **Always prefer heartbeat when both timeout durations are configured equal.**
  judgment: configuration equality does not imply simultaneous effective
  deadlines after progress or heartbeat activity; classification must compare
  the absolute elapsed deadlines.
