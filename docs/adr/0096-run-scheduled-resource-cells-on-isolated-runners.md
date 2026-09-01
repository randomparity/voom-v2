# 0096 — Run scheduled resource cells on isolated runners

## Status

Accepted

## Context

`scripts/run-constrained.sh` applies CPU affinity, competing CPU load, a memory ceiling, and an
optional block-device write cap through a systemd user scope. Today `just test-constrained` fixes
the command but does not pass resource options to the wrapper, and no CI workflow executes the
real constrained path. Issue #582 requires weekly baseline, CPU-load, disk-throttled, and
reduced-memory coverage plus baseline and disk-throttled stress runs.

The `--load` implementation starts busy loops before entering the systemd scope. A deferred-work
record, `docs/debt/0006-run-constrained-leaks-load-hogs.md`, documents that a completed invocation
can leave those loops alive. Running multiple cells on one host would therefore allow one cell to
contaminate another even if the workflow tried to reap between commands.

## Decision

Run one resource cell per fresh GitHub-hosted Linux runner through a four-entry matrix. Every cell
runs the workspace tests through `just test-constrained`; the baseline and disk-throttled cells
also run the distributed stress harness through `just stress-constrained`. The baseline cell runs
the 100,000-location scan diagnostic through `just scan-session-scale-constrained`.

Change the constrained recipes so their trailing arguments are resource-wrapper options placed
before the `-- COMMAND` boundary. The fixed commands remain owned by the recipes. Do not add a
generic arbitrary-command recipe.

The workflow remains scheduled and manually dispatchable only. It reports scheduled failures by
opening a tracking issue, matching the existing heavy-lane pattern; it is not a pull-request merge
gate. Manual-dispatch and first-schedule evidence are necessarily collected after this workflow
exists on the default branch.

## Consequences

- Each resource condition gets a clean VM and cannot inherit load generators, memory pressure, or
  cgroup state from another cell.
- Four full workspace test runs consume more scheduled runner time than a serial job, but execute
  concurrently and preserve independent failure attribution.
- Stress runs add cost only to the two cells required by issue #582. The scale diagnostic runs
  once because the requirement is reachability under the lane, not a four-cell scale matrix.
- The recipes expose resource controls, not arbitrary command execution, so the scheduled command
  set remains reviewable in the `justfile`.
- The existing load-generator leak remains tracked by debt record 0006; runner isolation bounds
  its impact to the disposable job that created it.

## Considered & rejected

- **Run all cells serially on one runner.** verified: `docs/debt/0006-run-constrained-leaks-load-hogs.md`
  records a reproduced surviving `--load` process and concludes that concurrent or subsequent
  invocations cannot safely share a host until the script owns cleanup.
- **Fix the load-generator lifecycle in this change.** judgment: that fix has its own process-tree,
  exit-status, and live self-test contract; runner isolation satisfies #582 without absorbing the
  separately recorded debt.
- **Add a generic `just constrained -- COMMAND` entry point.** judgment: the scheduled lane needs
  exactly three fixed commands, and an arbitrary-command surface would be broader than the issue.
- **Run the scale diagnostic in every matrix cell.** judgment: it multiplies a 100,000-row
  diagnostic without establishing an additional criterion; one constrained baseline execution
  makes the test reachable in the requested lane.
