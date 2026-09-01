# Constrained-resource testing

The `constrained-resources` GitHub Actions workflow exercises the workspace under resource
limits that ordinary pull-request CI does not apply. It runs every Monday at 07:00 UTC and can
also be started manually. It is diagnostic coverage, not a merge gate.

## Scheduled cells

Each cell runs `just test-constrained` on a fresh `ubuntu-latest` runner and stops after 90
minutes. Matrix failures do not cancel sibling cells.

| Cell | Limits | Distributed stress | Scan scale |
| --- | --- | --- | --- |
| baseline | `--cpus 0-3 --memory 16G` | yes | yes |
| cpu-load | `--cpus 0-3 --memory 16G --load 1` | no | no |
| disk | `--cpus 0-3 --memory 16G --write-bps 40M` | yes | no |
| memory | `--cpus 0-3 --memory 8G` | no | no |

The baseline scan step runs both ignored 100,000-location diagnostics. The stress steps use the
same distributed harness as `just stress`.

## Local reproduction

The resource wrapper requires Linux, systemd user services, cgroup v2, `systemd-run`, and
`taskset`. Disk throttling also requires a resolvable backing block device. A missing prerequisite
exits with status 3 and an actionable diagnostic; it never falls back to an unconstrained run.

Use the same fixed commands as the workflow:

```sh
just test-constrained --cpus 0-3 --memory 16G
just test-constrained --cpus 0-3 --memory 16G --load 1
just stress-constrained --cpus 0-3 --memory 16G --write-bps 40M
just scan-session-scale-constrained --cpus 0-3 --memory 16G
```

Run the scale diagnostics without resource controls with `just scan-session-scale`. Inspect a
recipe without executing it by adding `--print-plan`, for example:

```sh
just stress-constrained --write-bps 40M --print-plan
```

The CPU-load cell deliberately omits `--load` from its preflight. Debt record
[`0006`](../debt/0006-run-constrained-leaks-load-hogs.md) documents that load generators outlive
the wrapped command. Fresh, isolated runners contain that leak, and the real test starts the load
only once. Do not run concurrent load-bearing wrapper invocations on one local host until the debt
is resolved.

## Failure reporting

A failed scheduled run opens a GitHub issue containing the Actions run URL. Manual-dispatch
failures do not create issues. The notification job alone receives `issues: write`; test jobs have
only `contents: read` and checkout does not persist credentials.

Treat a failure as evidence to diagnose. Do not weaken or bypass a resource limit to make a cell
green.

## Post-merge evidence

After the workflow lands on the default branch:

1. Manually dispatch `constrained-resources` and record the run URL and all four cell results on
   [issue #582](https://github.com/randomparity/voom-v2/issues/582).
2. Record the first weekly scheduled run and its findings on issue #582.
3. Diagnose any failure from its run artifact; scheduled failures should also have an automatic
   tracking issue.
