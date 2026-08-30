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
| `VOOM_STRESS_PROCESS_CRASH_PERCENT` | 0 | 0–25 |
| `VOOM_STRESS_SEED` | 581 | 0–`u64::MAX` |
| `VOOM_STRESS_DRAIN_SECONDS` | 120 | 1–600 |

Stall, in-process crash, and process-crash percentages must total at most 25. An in-process crash
abandons a lease in a synthetic lane. A process crash dispatches the selected first attempt to a
dedicated supervised `chaos-worker`, requires a non-zero child exit and explicit reap, then leaves
the lease held. Both paths use the same normal remote recovery and synthetic replacement attempts.
The process arm is opt-in: zero preserves the single-test recipe, while a non-zero value prebuilds
`chaos-worker` before running the ignored library test.

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

For the minimal real-process recovery cell:

```console
VOOM_STRESS_NODES=1 \
VOOM_STRESS_RUNNERS_PER_NODE=1 \
VOOM_STRESS_TICKETS=8 \
VOOM_STRESS_PROCESS_CRASH_PERCENT=25 \
VOOM_STRESS_DEPENDENCY_PERCENT=90 \
VOOM_STRESS_DRAIN_SECONDS=30 \
just stress
```

Reproduce a failure by copying the complete `VOOM stress config` line into the corresponding
environment variables, including `VOOM_STRESS_SEED`. A timeout reports the observed ticket,
held-lease, and execution-record counts. A conservation failure reports all sorted mismatches,
including ticket attempts, duplicate non-abandoned executions, leaked leases, terminal events,
and dependency ordering. A successful process cell reports selected and observed crash counts,
non-zero exits, expired first leases, supervised children remaining after cleanup, typed
PID/node/worker/ticket/lease/exit observations, total attempts, terminal tickets, and held leases.
