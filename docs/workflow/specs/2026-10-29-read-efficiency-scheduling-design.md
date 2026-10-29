# Read efficiency and scan scheduling benchmark (#789)

## Problem

Stages 1–2 of the frozen [protocol](../../media-evidence-protocol.md) need results against
#788's baselines; production defaults stay unchanged.

## Scope

- `scripts/media-evidence-candidates.py`, beside the unchanged #788 harness:
  - `prepare <variant> <dir>`: detached worktree of `HEAD` at `<dir>`; exact-once source edits;
    release build of both workers there, whose harness records the readable constants and
    changed paths. Variants: `buf64k`, `buf1m`, `buf4m` (`HASH_CHUNK_BYTES`, heap buffer);
    `batch8k` (8 KiB read loop in one `spawn_blocking` task).
  - `compare <baseline> <candidate>`: refuses unequal file sets or cache states. Per metric
    (wall, summed hash seconds, first file done, physical bytes, CPU and peak RSS per worker)
    applies protocol thresholds and guardrails. Network uses repetitions 2+. A non-`observed`
    row marks the candidate failed.
  - `model <record>`: from per-file completion (queue delay + hash + probe), first durable
    observation under the barrier (ADR 0067/0077), per-file readiness, flush counts 1/10/1000.
- Self-test plus `source-checks` recipe. Records named `<date>-stage<n>-<cell>-<corpus>.json`,
  sorting after #788's. `compare`/`model` output lives only in the summary
  `stages-1-2.md`, which maps each record to its variant, command, and worker SHA-256.
- Cells, 3 repetitions each: stage 1 local evicted full pass per variant. Stage 2 uses the
  stage-1 selection: among non-failed variants without speed regression or guardrail breach, the
  best wall improvement, then best summed-hash improvement; else baseline. Local full pass at
  concurrency 8; local subset `f0004,f0025,f0022,f0005,f0009,f0015` at 1/2/4 (4 is the
  reference); network subset `f0049,f0074,f0063` at 1/2, plus 4 when the selection is not
  baseline. Network 8 is excluded: three files cap it at the concurrency-4 cell.
- Model boundaries: batches are provisional; durability is session completion; per-file
  readiness needs a superseding ADR and never covers absence.
- Not measured: probe-buffer variants; flush byte bound; publication; stages 3–4, 5 (#801).

### Failure model

1. Actors: one operator on one Linux host; local corpus; network corpus shared with a live
   rehearsal; the self-test in CI.
2. Invariants: originals read-only; production worker source and defaults unchanged;
   committed files carry no host path or name; no captured number invented or dropped.
3. Accepted: `as-found` network cache differences across cells (deltas may be
   inconclusive); other host load disclosed, not controlled; no completion-transaction time.
4. Elsewhere: stages 3–4 #790; decision #791; production changes #792; SLO #471.

## Success

1. Each stage-1 variant and named stage-2 cell has a committed record, or a recorded exclusion.
2. Summary gives `compare` verdicts against #788 records or a same-instrument cell, and each
   model labelled modelled; first durable observation's verdict stays inconclusive.
3. Each unmeasured metric reads `not_measured` or `unavailable`.
4. No crate source changes on the branch.

## Validation

- Variant edits — focused-test: each applies once to current source; drifted source fails.
- Compare — focused-test: fixtures yield improvement, regression, inconclusive (CV > 20%),
  memory and CPU guardrails, failed rows, selection order, and mismatched-cell refusal.
- Model — focused-test: fixture times yield expected barrier, readiness, batch times, labels.
- Committed records — focused-test: the #788 self-test validates every record.
- Summary prose — task-test-not-applicable: no executable consumer reads it.
