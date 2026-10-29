# Read efficiency and scan scheduling benchmark (#789)

## Problem

Stages 1 and 2 of the frozen [protocol](../../media-evidence-protocol.md) need candidate
results against #788's baseline records without changing production defaults.

## Scope

- `scripts/media-evidence-candidates.py`, beside the unchanged #788 harness:
  - `prepare <variant> <dir>`: detached worktree of `HEAD` at `<dir>`; exact-once source edits;
    release build of both workers there. That checkout's harness then records the variant's
    constants and changed paths. Variants: `buf64k`, `buf1m`, `buf4m` (`HASH_CHUNK_BYTES`,
    heap buffer); `batch8k` (8 KiB read loop in one `spawn_blocking` task).
  - `compare <baseline> <candidate>`: refuses unequal file sets or cache states; classifies
    per metric by protocol thresholds and guardrails. Network uses repetitions 2+.
  - `model <record>`: from per-file completion (queue delay + hash + probe), first durable
    observation under the barrier (ADR 0067/0077), per-file readiness, flush counts 1/10/1000.
- Self-test plus `source-checks` recipe. Records in `docs/media-evidence-baseline/`; summary.
- Cells, each 3 repetitions: stage 1 local evicted full pass per variant. Stage 2 uses the
  stage-1 selection (best median wall or hash-time improvement without regression or
  guardrail breach; else baseline): local full pass at concurrency 8; local subset
  `f0004,f0025,f0022,f0005,f0009,f0015` at 1/2/4; network subset `f0049,f0074,f0063` at 1/2/8,
  plus 4 when the selection is not baseline. Measurements hold the exclusive host lock.
- Model boundaries: batches are provisional; first durable observation is completion;
  per-file readiness needs a superseding ADR and covers present files only.
- Not measured: probe-buffer variants; flush byte bound; real publication; stages 3–5.

### Failure model

1. Actors: one operator on one Linux host; local corpus; network corpus shared with a live
   rehearsal; the self-test in CI.
2. Invariants: originals read-only; production worker source and defaults unchanged;
   committed files carry no host path or name; no number invented or dropped.
3. Accepted: cross-cell cache differences on `as-found` network cells, so network deltas
   may be inconclusive; host load outside the lock; model omits completion-transaction time.
4. Elsewhere: fingerprints, lazy hashing #790; decision #791; production changes #792; SLO #471.

## Success

1. Each stage-1 variant and each named stage-2 concurrency cell has a committed record,
   or a recorded exclusion with its reason.
2. Summary gives `compare` verdicts per metric against #788 records or a same-instrument cell,
   plus each cell's model with its boundaries.
3. Every unmeasured metric reads `not_measured` or `unavailable`.
4. No crate source changes on the branch.

## Validation

- Variant edits — focused-test: each edit applies once to the current source; a drifted
  source fails.
- Compare — focused-test: fixtures yield improvement, regression, inconclusive (CV > 20%),
  memory and CPU guardrail regressions, and refusal on mismatched cells.
- Model — focused-test: fixture completion times yield expected barrier, readiness, and
  batch times.
- Committed records — focused-test: the #788 self-test validates every record.
- Summary prose — task-test-not-applicable: no executable consumer reads it.
