# 0102 — Local-worker self-heal retires only on a refused endpoint

## Status

Accepted

## Context

`voom worker run-local` starts a supervisor that registers a node-less worker row and
spawns a bundled mutation worker bound to `127.0.0.1:<port>`. Before registering,
`ControlPlane::self_heal_stale_workers` retires every same-kind `registered`/`active` row
that a previous hard kill left behind. Until now the verdict "this row is stale" was
"its endpoint did not complete a `handshake` within `LIVENESS_PROBE_TIMEOUT` (500 ms)" —
the same probe `compliance execute` uses to reject dead endpoints before dispatch.

That probe is a dispatch heuristic, not evidence of death. A live sibling whose worker is
slow to answer — a loaded CI runner, a stopped process — fails it, and a starting peer then
retires a row whose supervisor is still running. Issue #667 is that case: worker 5's peer
retired it, so worker 5's own `shutdown_and_retire` hit `CONFLICT … already retired` and
`run-local` exited 2. A retired row is never dispatched again, so the live worker is
unusable for the rest of its life.

Retirement is destructive and cross-process; the dispatch probe only skips a worker for one
run. The two decisions need different evidence.

## Decision

1. Self-heal retires a same-kind row that has a recorded endpoint only when a TCP connect to
   that endpoint is refused (`io::ErrorKind::ConnectionRefused`). A loopback port with no
   listener refuses at once; a live worker's listener accepts the connection in the kernel
   even while the process is stopped or busy. A connect that succeeds, times out, or fails
   any other way is inconclusive and the row is kept.
2. A row with no recorded endpoint keeps today's behaviour: it is retired.
3. Self-heal treats a retirement `CONFLICT` as done when re-reading the row shows it
   retired. Worker epochs advance only on retirement, so the conflict means a peer
   retired the row first.
4. Dispatch-time liveness (`probe_live_runtimes`, `LIVENESS_PROBE_TIMEOUT`,
   `reject_dead_endpoint_operations`) is unchanged.

## Consequences

- A live sibling with a recorded endpoint is not retired by a starting peer's self-heal,
  however long it takes to answer.
- A worker whose process is alive but wedged with its port open is no longer self-healed.
  Dispatch still excludes it through the probe. Its supervisor still retires it on
  shutdown, or an operator can retire it by hand.
- A stale row whose port another process has since bound is kept until that port frees.
  The dispatch probe excludes it when that process does not speak the handshake.
- A row registered but not yet given an endpoint (a peer mid-startup) is still retired.
  That race remains open (decision 2).
- A peer that starts while a sibling's worker is shutting down can still retire that row
  correctly, and the sibling's shutdown then reports `CONFLICT`. `shutdown_and_retire` is
  unchanged here.

## Considered & rejected

- **Retry the handshake over a longer budget before retiring.** judgment: it narrows the
  window rather than closing it, and any budget is a timing guess about runner load.
- **Owner-held evidence: persist supervisor PID, start identity and boot id per worker.**
  judgment: it needs a schema change and per-platform process probes (the accelerator-claim
  path already carries both) to settle a question a refused connect answers for loopback
  endpoints.
- **Supervisor heartbeat/lease on the worker row.** judgment: it adds a periodic writer per
  supervisor and a TTL that is again a timing guess.
- **Treat `already retired` as success in `shutdown_and_retire` only.** judgment: the live
  worker stays retired and undispatchable; it hides the wrong verdict rather than fixing it.
- **Do nothing.** verified: the triage repro (stop the first `mkvtoolnix` worker child with
  SIGSTOP, start a second supervisor, resume and shut the first down) exits 2 with a
  `CONFLICT` envelope on stdout at base `08d779bd`, Linux x86_64.
