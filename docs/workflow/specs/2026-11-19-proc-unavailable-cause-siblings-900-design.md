# Report why sibling harnesses record /proc counters as unavailable — design (#900)

## Problem

#888 made `media-evidence-observe.py` read each `/proc` source on its own and print why a
counter is `unavailable`. Three siblings still read `stat`, `status` and `io` in one `try` and
swallow the error: `process_usage(pid)` in `media-evidence-rollout.py`, `process_usage()` in
`media-evidence-lazy.py` and `Worker.usage()` in `media-evidence-baseline.py`. One failed
source blanks every later counter, no cause is reported, and a short `stat` sums whatever
fields are present into a wrong CPU time.

## Scope

- **One reader, owned by `media-evidence-baseline.py`.** Observe, rollout and lazy all load
  baseline already (`_load`/`load`/`module_from_spec`) and use it for `UNAVAILABLE`,
  `host_counters` and record helpers, so baseline is the existing shared module and needs no
  new import edge. A fourth copy of the per-source parser would be the defect surface the issue
  names; a new module would add a fourth load site for one function. Baseline gains
  `proc_usage(process, harness, sources=("stat", "status", "io"))`, moved from observe's
  `process_usage` with its `_stat_counters`, `_status_counters`, `_io_counters`,
  `PROC_SOURCES` and `CLOCK_TICKS`:
  - `process` is a pid or `"self"`; `harness` is the stderr prefix (`media-evidence-<name>`),
    so each harness's lines still name the harness that printed them;
  - `sources` selects which sources to read, so lazy reads only `status` and `io`;
  - the `/proc` root is the module constant `PROC_ROOT`, read at call time, so a selftest
    patches it instead of every caller carrying a `proc_root` parameter.
- **Callers.**
  - observe and rollout: `process_usage(pid)` stays as the one-line binding of the harness
    prefix (`baseline.proc_usage(pid, "media-evidence-<name>")`), so the call sites and a
    wiring test have one name per harness; observe's local parsers, `PROC_SOURCES` and
    `CLOCK_TICKS` are removed. Rollout's CPU is now rounded to 3 places before the delta (as
    observe and baseline already did); `usage_delta` rounds the delta to 3 places anyway, so
    the recorded value is unchanged in practice.
  - lazy: `process_usage()` keeps CPU from `os.times()` (every host provides it, and it is this
    process's own time, not children's) and merges
    `baseline.proc_usage("self", "media-evidence-lazy", ("status", "io"))`.
  - baseline `Worker.usage()`: the abandoned-worker check under the lock is unchanged and still
    returns all four counters `unavailable` without a `/proc` read or a stderr line — the cause
    is already recorded as the row failure `worker_abandoned`. Otherwise it returns
    `proc_usage(self.process.pid, "media-evidence-baseline")`.
- **Causes and format are #888's, unchanged:** one stderr line
  `<harness>: process <counter> unavailable: <cause>` per unavailable counter, cause
  `no /proc on this host`, `<source>: <ErrorClass>` (for `OSError`, `IndexError`, `ValueError`)
  or `<source>: field absent` (including `stat` with fewer than 15 fields after the command
  name). No record field changes; every committed record validates as before.
- The `SOURCE_CONSTANTS` regex in baseline is not touched.

### Failure model

1. Actors and deployments: an operator running the four harnesses on Linux or macOS; the
   selftests on Linux and macOS CI (no `/proc`).
2. Invariants: `unavailable` remains a valid metric value; record keys and validation are
   unchanged; stderr names the source, never the host path; an abandoned baseline worker is
   never sampled.
3. Accepted: on macOS each sample prints one `no /proc on this host` line per `/proc` counter
   (four for rollout/baseline/observe, three for lazy). Observe's recorded `harness_sha256`
   covers only observe's own file, so it no longer covers the reader; that matches how observe
   already depends on baseline for `host_counters` and the record helpers.
4. Covered elsewhere: none.

## Success

- Rollout, lazy and baseline report each unavailable `/proc` counter with its cause and keep
  readable counters when another source fails; a short `stat` is `unavailable`.
- `just source-checks` exits 0, which runs every media-evidence selftest including the
  committed-record validation tests.

## Validation

- focused-test: `ProcessUsageTests` move from the observe selftest to the baseline selftest
  and target `baseline.proc_usage` with a patched `PROC_ROOT`: full tree; no root; missing pid;
  malformed / short / `)`-less `stat`; `status` without `VmHWM` or with a bad value; `io`
  without `read_bytes` or without a colon; plus a `sources` subset that reads only
  `status`/`io`.
- focused-test (wiring, one per caller, run without a real `/proc` via the patched root):
  rollout and observe `process_usage` print with their own prefix; lazy keeps `os.times()` CPU and
  reports only `status`/`io` causes; baseline `Worker.usage()` reports per-source causes and
  stays silent and all-`unavailable` when abandoned.
- Bite check: against the old rollout, lazy and baseline bodies, the wiring tests fail (no
  stderr cause, a bad `stat` blanks the later counters).
