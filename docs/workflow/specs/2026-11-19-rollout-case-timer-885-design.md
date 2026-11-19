# Deterministic rollout case-(b) edit in the selftest — design (#885)

## Problem

`edit_during_hash` starts case (b)'s edit on `threading.Timer(delay, edit)` and refuses the
case when the edit lands after the hash read returns. `CaseTests` drives it with
`--edit-delay 0.1` against a fake hash that holds its read open for `time.sleep(0.3)`. A runner
that schedules the timer thread more than 0.2 s late makes case (b) raise `HarnessError`
("edit landed after the hash read ended"), so the test outcome depends on scheduling. Forcing
every rollout timer 0.5 s late reproduces it on `main`.

## Scope

- `scripts/media-evidence-rollout.py`: the timer start moves into a module function
  `start_edit(delay, edit)` that starts and returns the `threading.Timer`; `edit_during_hash`
  calls it and joins what it returns. The production path is byte-for-byte the same timer, delay
  and join, so `construct --edit-delay` behavior, its record and the "edit landed after" refusal
  are unchanged. This is the case-(b) injection point the issue asks for; the selftest
  replaces it with `patch.object`, as it already does for `copy_bytes` and `start_time`.
- `scripts/media-evidence-rollout-selftest.py`:
  - `FakeWorker` gains a class-level `edit` slot. Its hash step takes the pending edit, keeps its
    existing 0.3 s read, runs the edit after that read and before its post-read stat, then
    compares facts as today. The edit is therefore inside the hash window by construction.
  - `CaseTests.setUp` patches `rollout.start_edit` with a function that hands the edit to
    `FakeWorker.edit` and returns a no-op joinable; cleanup clears the slot. `--edit-delay` is
    dropped from the fixture arguments because nothing reads it.
  - `test_an_edit_after_the_hash_read_refuses_case_b` stops relying on a 1.0 s timer outliving
    the 0.3 s read: its `start_edit` returns a joinable whose `join` runs the edit, so the edit
    lands after `hash_file` returns by construction.
  - One new test pins the production seam: `start_edit(0.0, edit)` returns a `threading.Timer`
    whose interval is the given delay, and joining it runs the edit.
- No ownership transition: the timer start keeps its owner (`edit_during_hash`'s module) and
  gains a name.

### Sweep of the rollout selftest for other wall-clock races

- `kill_at` (`test_a_kill_spares_a_pid_that_is_no_longer_the_worker`): delay is ~0 and the test
  joins the timer before asserting, so the kill decision always runs first. Not a race.
- `PublicationTimingTests.test_a_dead_stack_stops_the_wait` and
  `test_a_session_that_never_ends_fails_loudly` read the real clock, but each outcome (raise on
  the first `require_alive`, raise at the 0.2 s limit) holds for any scheduling. Not a race.
- `ConstructionTests` and `ordinary_write` sleep 0.02 s before an edit so the timestamp ticks.
  That is a timestamp-granularity bound, not a race against another thread; unchanged.

### Failure model

1. Actors and deployments: `just media-evidence-rollout-selftest` on Linux and macOS CI and
   operator hosts; operators running `construct` against real workers.
2. Invariants: the selftest still proves case (b) is detected only by the hash's own drift
   check, and that an edit outside the read is refused; production `construct` behavior and
   committed records are untouched.
3. The fake read keeps its 0.3 s sleep: it is not a race window any more but the gap that lets a
   coarse-grained ctime (some Linux kernels tick at 4–10 ms) differ between the pre-read stat and
   the edit, and between the hash and the later probe in cases (a) and (c). Removing it would
   trade the scheduling race for a timestamp-granularity flake.
4. Accepted: the selftest no longer exercises the real timer thread racing a real hash; the new
   seam test pins its delay and join. The real race is the instrument's own documented limit
   (`--edit-delay` must fall inside the real read), which the refusal reports.

## Success

- `CaseTests` passes when every rollout timer is forced 0.5 s late; on `main` the same forcing
  fails case (b).
- No `threading.Timer` drives any `CaseTests` edit.
- `just media-evidence-rollout-selftest` and `just source-checks` exit 0.

## Validation

- focused-test: `CaseTests.test_every_case_behaves_as_its_gate_defines`. Old structure red: a
  scratch driver that subclasses `threading.Timer` with +0.5 s fails it on `main` with the
  "edit landed after the hash read ended" `HarnessError`. New structure: the same driver passes.
  Red on the new driver: a fake that skips the handed-over edit fails the `hash_drift` marker
  assertion.
- focused-test: `CaseTests.test_an_edit_after_the_hash_read_refuses_case_b`. Red: removing the
  refusal from `edit_during_hash` fails the `assertRaises`; deterministic under the same forcing.
- focused-test: the new `start_edit` seam test. Red: a `start_edit` that ignores `delay` fails
  the interval assertion.
- Green: `just media-evidence-rollout-selftest`, `just source-checks`.
