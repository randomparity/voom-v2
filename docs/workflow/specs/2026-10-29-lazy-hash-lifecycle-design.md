# Lazy full-hash lifecycle prototype (#797)

## Problem

A provisional fingerprint, if ever accepted, needs a deferred full hash. Its lifecycle under
restart, cancel, retry, starvation, concurrent mutation and late disagreement, and its eventual
I/O, are unmeasured. Stage 4 and oracle cases O13–O15 of
[the protocol](../../media-evidence-protocol.md) need that evidence.

## Scope

`scripts/media-evidence-lazy.py`, experiment-only, imports read-only helpers from
`media-evidence-baseline.py` (neither file changes). One per-file record holds state
`unknown`, `provisional`, `verified`, `stale` or `disagreement`:

- `observe`: stat plus a fixed fingerprint (size, mtime ns, inode key, 64 KiB head/tail
  digest) moves any state to `provisional`. Fingerprint search belongs to #796.
- `verify`: one full read with pre/post stat. Stat drift gives `stale`; interruption keeps
  `provisional` and publishes nothing; a fingerprint recomputed from that pass, or a prior
  verified digest, that differs gives `disagreement`; else `verified`.
- Plans from provisional evidence are `held`, `committable` once verified, `revoked` on
  disagreement; outputs are then `quarantined`. Digests are `exp-blake2b:` (1 MiB reads), never `blake3:`; nothing touches the store, workers,
leases or absence reconciliation.

`matrix` runs on scratch fixtures, with deterministic fault hooks and an oracle that knows the
true content: clean, restart (O14), cancel, retry, starvation (on-demand-only versus aged
background), concurrent mutation (O10 drift and an unseen interior edit), late disagreement
(O15 head and interior), stale evidence (O13). `run` measures one lifecycle per repetition:
observe all, two probe re-observations each, then verify all, 4 in flight. `compare` applies
protocol thresholds to logical and physical bytes against the #788 records.

### Failure model

- Actors and deployments: a local operator on the measurement host; CI runs the self-test.
- Invariants and assets at stake: corpora stay read-only; committed records carry no host path;
  experiment digests never reach a production hash field.
- Accepted failure classes: wall and CPU are not comparable with #788's worker instrument
  (reported, not compared); host counters include rehearsal traffic (disclosed).
- Covered elsewhere: fingerprint choice (#796); buffers, concurrency (#789); reuse (#798);
  decision (#791); production lifecycle (#792).

## Success

1. The five states and transitions are implemented, and every listed matrix case is reported
   with expected and observed state, detection versus oracle, waste bytes, dispositions.
2. Records for 3 local evicted repetitions and the 3-file network subset report eventual
   logical bytes by component, physical bytes, and residency, within protocol budgets.
3. A results note gives commands, comparison, constructed misses, exclusions, disclosures.
4. `just media-evidence-lazy-selftest` validates committed records, in `source-checks`.

## Validation

- Transitions and plan dispositions: focused-test in the self-test, one per matrix case.
- Matrix determinism: focused-test runs `matrix` twice and asserts identical output.
- Record schema and redaction: focused-test rejects missing metrics, host paths, `blake3:`.
- Committed records: focused-test validates every `*lazy*.json` record.
- Comparison: focused-test checks `compare` on synthetic medians on both threshold sides.
- Corpus read-only: task-test-not-applicable, because opens are read-only and the matrix
  mutates only scratch copies; the note records pre/post manifest equality.
