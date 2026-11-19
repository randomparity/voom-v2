# Report why observe process counters are unavailable — design (#888)

## Problem

`process_usage` in `scripts/media-evidence-observe.py` reads `/proc/<pid>/stat`, `status` and
`io` inside one `try` and swallows `OSError`, `IndexError` and `ValueError`. A counter left
`unavailable` carries no cause, so a Linux `/proc` read error looks the same as a host with no
`/proc` (macOS). One failed read also hides the later sources: a malformed `stat` leaves
`peak_rss_bytes`, `rchar` and `read_bytes` unavailable although their files are readable.

## Scope

- The cause goes to **stderr**, one line per unavailable counter:
  `media-evidence-observe: process <counter> unavailable: <cause>`. The committed record is
  unchanged (`unavailable` stays the value; no new field), so every record in
  `docs/media-evidence-baseline/observe/` validates as before and the protocol's
  record-format section needs no edit. Stderr is chosen over a record field because the
  issue accepts either and a record field would change `validate_run`, the record format and
  the protocol for a diagnostic the operator reads while running.
- Causes:
  - `no /proc on this host` when the `/proc` root does not exist (macOS);
  - `<source>: <ErrorClass>` when reading or parsing that source raised `OSError`,
    `IndexError` or `ValueError` — `source` is `stat`, `status` or `io`, never the host path;
  - `<source>: field absent` when the source parsed but lacks the field (`VmHWM`, `rchar`,
    `read_bytes`), or `stat` has fewer than 15 fields after the command name (today a short
    `stat` silently sums the fields present and records a wrong CPU time).
- Each source is read and parsed independently, so one failure leaves only its own counters
  unavailable. `os.sysconf("SC_CLK_TCK")` is called outside the per-source guard: POSIX
  always defines it, and a failure there is a harness fault that should raise, not be
  reported as a `stat` problem.
- Lines are printed when the counter is found unavailable, in repetition order; they do not
  name the repetition index (repetitions run one after another, so the order identifies it).
- `process_usage` gains a `proc_root: Path = Path("/proc")` parameter so the selftest can
  point it at a scratch tree; `observe_all` calls it unchanged.
- No ownership transition. The sibling `process_usage` functions in
  `media-evidence-rollout.py` and `media-evidence-lazy.py` (and `baseline.py`'s worker
  sampler) share the silent pattern; they are outside this issue's file scope and are routed
  to the campaign orchestrator as a follow-up candidate under #787.

### Failure model

1. Actors and deployments: an operator running `media-evidence-observe.py run` on Linux or
   macOS; `just media-evidence-observe-selftest` on Linux and macOS CI.
2. Invariants: `unavailable` remains a valid metric value; record schema and validation are
   unchanged; no host path enters the record (stderr names the source, not the path).
3. Accepted: a run on macOS prints four `no /proc on this host` lines per repetition; the
   cause is true and the run still succeeds.
4. Covered elsewhere: sibling harnesses' silent `/proc` reads (follow-up candidate above).

## Success

- A missing pid directory, an unreadable or malformed source, an absent field and a host with
  no `/proc` each yield `unavailable` for exactly the affected counters and one stderr line
  naming that counter and cause; readable sources still yield numbers.
- Every committed observe record still validates; `just media-evidence-observe-selftest` and
  `just source-checks` exit 0.

## Validation

- focused-test: new `ProcessUsageTests` in `scripts/media-evidence-observe-selftest.py`
  against a scratch `proc_root`: full tree (numbers, no stderr); no root (`no /proc on this
  host` for all four); missing pid directory (`stat: FileNotFoundError` etc.); malformed
  `stat` (`stat: ValueError`, the other three still numbers); `stat` without `)`
  (`stat: IndexError`); `VmHWM` with a non-integer value (`status: ValueError`); short `stat`
  (`stat: field absent`); `status` without `VmHWM` and `io` without `read_bytes`
  (`field absent`); `io` line without a colon (`io: ValueError`). Red on the current code: no
  stderr line is written and a malformed `stat` hides the other counters. Bite check: revert
  the per-source split and watch the malformed-`stat` test fail.
- focused-test: existing `ValidateTests.test_committed_records_are_valid` stays green.
