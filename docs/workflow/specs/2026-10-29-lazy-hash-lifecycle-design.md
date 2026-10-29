# Lazy full-hash lifecycle prototype (#797)

## Problem

A provisional fingerprint needs a deferred full hash whose recovery behavior and eventual I/O
are unmeasured ([protocol](../../media-evidence-protocol.md) stage 4, O13–O15).

## Scope

`scripts/media-evidence-lazy.py`, experiment-only, imports `media-evidence-baseline.py`
helpers (unchanged); digests are `exp-blake2b:` (1 MiB reads); no store, worker, lease or
absence code. Per-file records persist in a scratch JSON state file, written
temp-then-rename per transition; a digest appears only in the write that sets `verified`.

- `observe` takes stat plus a fixed fingerprint (size, mtime ns, inode key, 64 KiB head/tail
  digest). `unknown`, `stale`, `disagreement` → `provisional`; `verified` stays if the
  fingerprint is unchanged, else → `stale`.
- `verify` reads the whole file between two stats. A start stat unlike the provisional one, or
  drift → `stale`; a head/tail fingerprint recomputed from the pass unlike the provisional one →
  `disagreement`; else `verified`. Interruption leaves the state file untouched; `verify` in
  any other state is a recorded refusal.
- Plans: `held` while provisional, `committable` if verified, else `revoked`, outputs `quarantined`.

`matrix` uses scratch fixtures and an oracle knowing true content [expected result]: clean
[verified]; restart O14, a child SIGKILLs itself mid-read, a fresh reader loads the state file
[provisional, no digest, plan held, partial read wasted]; cancel [same, no reload]; retry, first
open fails [verified, both reads counted]; mutation during the read O10 [stale]; unseen interior
edit during the read, mtime restored [verified: constructed miss]; O15 head edit [disagreement,
revoked, quarantined], interior edit [verified: miss]; O13 verified then head edit [stale],
interior edit [verified: miss]. Re-verification stops after 2 attempts. Starvation: 8
background files, one new on-demand file and one verify per tick, 32 ticks; on-demand-only [8
provisional] versus oldest background every 4th tick [8 verified].

`run` (#788's selected ids) per repetition: observe all, two probe re-observations each, verify
all, 4 in flight; a stale file is a recorded row. `compare` applies protocol thresholds to bytes
against #788 records, refusing a differing label, manifest size or id set. Schemas:
`media-evidence-lazy-run/1`, `media-evidence-lazy-matrix/1`; both are committed.

### Failure model

- Actors and deployments: a local operator on the measurement host; CI runs the self-test.
- Invariants: corpora stay read-only; records carry no host path; digests stay experimental.
- Accepted failure classes: wall and CPU differ from #788's worker instrument (reported, not
  compared); host counters include rehearsal traffic (disclosed).
- Covered elsewhere: fingerprints #796; buffers, concurrency #789; reuse #798; decision #791.

## Success

1. Each matrix case reports expected and observed state, oracle verdict, waste bytes and
   dispositions, matching its bracketed expectation.
2. 3 local evicted and 3 network as-found repetitions give logical bytes by component,
   physical bytes and residency, within budgets.
3. A note gives commands, comparison, misses, exclusions, disclosures (no stage 1–3 selection
   exists; the fingerprint is an instrument); the self-test runs in `source-checks`.

## Validation

- Focused-tests: matrix cases, determinism, schemas, redaction, records, `compare` refusals.
- Corpus read-only: task-test-not-applicable (read-only opens; only scratch copies mutate);
  the note records manifest equality before and after.
