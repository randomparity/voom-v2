# Deterministic rollout publication-timing selftest — design (#881)

## Problem

`test_first_durable_observation_is_the_first_published_location` writes the location at 0.3 s
and the completion at 0.6 s from wall-clock threads (`later`). When both land before one poll
of `wait_for_publication`, `first == complete` and `first < complete` fails (macOS CI, PR #880).

## Scope

- In `scripts/media-evidence-rollout-selftest.py`, the existing `scripted` driver (today it
  patches only `sleep` and reads the real clock, so two fast polls can round to one time) gains
  a fake clock: it replaces `rollout.time` with an object whose `sleep(s)` advances the clock by
  `s` and applies the next scripted write, and whose `monotonic()` returns the clock; `started`
  is the fake clock's 0. Each write is seen by the next poll only, and elapsed times are whole
  poll intervals. Every existing `scripted` caller keeps its assertions.
- The first-durable test becomes `scripted([[], [insert], [complete]])` and asserts
  `first == round(2 * POLL_SECONDS, 3)`, `complete == round(3 * POLL_SECONDS, 3)` (the
  instrument rounds), and `first < complete` unchanged. The retired/other-root test, the
  only other `later` caller, moves to `scripted` too: its outcome holds under any
  interleaving, but it shares the wall-clock driver.
- `later` and the `threading` import are removed. `scripts/media-evidence-rollout.py` is
  unchanged.
- No ownership transition: a test-only change to an existing driver.

### Failure model

1. Actors and deployments: `just media-evidence-rollout-selftest` on Linux and macOS CI and
   operator hosts.
2. Invariants: the selftest stays a true check of `wait_for_publication`'s ordering contract;
   the instrument's production behavior and committed records are untouched.
3. Accepted: the fake clock does not exercise real SQLite commit latency; ordering, not
   latency, is the contract under test.
4. Covered elsewhere: `CaseTests` wall-clock edit timing (`--edit-delay` against the fake
   hash's 0.3 s window) is outside this issue; routed to the campaign orchestrator as a
   follow-up candidate under #787.

## Success

- The first-durable test passes regardless of scheduler delay, and fails if the insert and
  completion are observed by the same poll.
- No wall-clock thread drives any `PublicationTimingTests` write.
- `just media-evidence-rollout-selftest` and `just source-checks` exit 0.

## Validation

- focused-test: `scripts/media-evidence-rollout-selftest.py`
  `PublicationTimingTests.test_first_durable_observation_is_the_first_published_location`.
  Old structure ties: #881's CI log (`0.614 not less than 0.614`), reproduced locally by
  slowing the poll so both thread writes land before one poll. Red on the new driver:
  scripting insert and completion into one step fails its exact-time assertions.
  Green: `just media-evidence-rollout-selftest`.
- focused-test: `PublicationTimingTests.test_retired_and_other_root_locations_are_not_publication`
  on the fake clock. Red: counting a retired row as live makes `first` numeric. Green: as above.
