# ADR 0126: Abandon timed-out media evidence workers

## Status

Accepted (2026-11-01)

## Context

Issue #835 identifies repeated dispatch and misleading measurements after a harness HTTP
call times out. The worker may still execute that lease. Parent #787 requires truthful
measurement accounting; production scheduling is outside this harness change.

## Decision

Record terminal abandonment under Worker's existing lease lock when a call reports
`transport_timeout`. Later admissions return `worker_abandoned` without dispatch.
Previously admitted calls keep their actual results, including a late success.
Return `unavailable` for the existing per-worker usage metrics after abandonment.
Retain fresh processes per repetition and the current bounded stop/kill fallback.

## Consequences

One timeout can prevent dependent later files from running within the repetition.
Already admitted calls can finish or dispatch after abandonment; this does not prove lease
cancellation. Worker metrics are withheld even if another request later succeeds.

## Considered & rejected

- **Continue dispatching.** verified: `Worker.call` at cbbdb185 and a two-call mocked
  HTTP timeout reproduction send two requests and return timeout twice, violating #835.
- **Restart the worker.** judgment: replacing processes complicates per-repetition accounting
  without being needed for the requested terminal behavior.
- **Serialize entire calls.** judgment: changes the measured concurrency and is unnecessary
  for fencing later admissions.
