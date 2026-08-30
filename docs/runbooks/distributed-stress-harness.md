# Distributed stress harness

Run the opt-in real-HTTP, on-disk SQLite stress harness with:

```console
just stress
```

The default cell creates four remote nodes, eight worker runners on every node, two execution
lanes per worker, and 1,000 tickets. It prints the effective configuration before setup and the
ticket, attempt, and retry counts after conservation succeeds. The recipe is intentionally not
part of `just ci`.

## Configuration

| Variable | Default | Accepted values |
|---|---:|---:|
| `VOOM_STRESS_NODES` | 4 | 1–32 |
| `VOOM_STRESS_RUNNERS_PER_NODE` | 8 | 1–32 |
| `VOOM_STRESS_MAX_PARALLEL` | 2 | 2–16 |
| `VOOM_STRESS_TICKETS` | 1000 | 1–10000 |
| `VOOM_STRESS_DEPENDENCY_PERCENT` | 20 | 0–90 |
| `VOOM_STRESS_STALL_PERCENT` | 0 | 0–25 |
| `VOOM_STRESS_CRASH_PERCENT` | 0 | 0–25 |
| `VOOM_STRESS_SEED` | 581 | 0–`u64::MAX` |
| `VOOM_STRESS_DRAIN_SECONDS` | 120 | 1–600 |

Stall and crash percentages must total at most 25. A crash is an in-process abandoned lease:
the lane leaves the lease held, the harness advances its injected domain clock past the recorded
deadline, runs normal remote recovery, and starts a replacement attempt. Issue #606 tracks real
subprocess termination.

For a quick recovery cell:

```console
VOOM_STRESS_NODES=2 \
VOOM_STRESS_RUNNERS_PER_NODE=2 \
VOOM_STRESS_TICKETS=80 \
VOOM_STRESS_STALL_PERCENT=5 \
VOOM_STRESS_CRASH_PERCENT=5 \
VOOM_STRESS_DRAIN_SECONDS=30 \
just stress
```

Reproduce a failure by copying the complete `VOOM stress config` line into the corresponding
environment variables, including `VOOM_STRESS_SEED`. A timeout reports the observed ticket,
held-lease, and execution-record counts. A conservation failure reports all sorted mismatches,
including ticket attempts, duplicate non-abandoned executions, leaked leases, terminal events,
and dependency ordering.
