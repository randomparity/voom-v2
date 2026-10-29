# Evidence-reuse and storage-version candidates (#798)

## Problem

#796 and #797 measured only deterministic sampled fingerprints, which a same-size edit with
mtime restored defeats (O1, O13). Change time, seeded-random sampling, evidence reuse and the
residual risk are unmeasured, and #791 needs one consolidated #790 result.

## Scope

- `media-evidence-fingerprint.py` gains `meta-ctime` (`meta` plus ctime ns) and `rnd64-i8` /
  `rnd64-i32` (64 KiB head/tail plus 8 or 32 aligned 64 KiB interior blocks drawn by a
  per-check seed). Before and after use one seed, which models comparing against a stored
  per-block digest table. #796 defaults stay its five candidates; `oracle` gains `--candidates`.
- New `media-evidence-reuse.py`: `probability` (single random interior byte edit, 1000 trials
  per candidate, against analytic `k/M`); `ctime-probe` (64 KiB scratch file on each given
  filesystem: content write, mtime restore, chmod, rename; ctime observed at once and after
  2 s); `signals` (read-only stat and inode-generation survey of a manifest subset).
- Records in `docs/media-evidence-baseline/reuse/`; README there; consolidated
  `docs/media-evidence-790-report.md` with reuse candidates (probe reuse, cross-scan cache),
  per-candidate deployment and threat assumptions, and the residual-risk statement.
- Protocol unchanged: the candidates are disclosed as post-freeze, operator-directed additions.
  CIFS server-side change is not measured (no server access).

### Failure model

1. Actors and deployments: one operator on one host; tmpfs, btrfs and the CIFS mount.
2. Invariants and assets: corpora read-only; no private detail committed; experiment evidence
   never enters production identity types or code.
3. Accepted: privileged ctime forgery is analytic only (needs root); timings are one host.
4. Covered elsewhere: decision #791, production lifecycle #792, tuning #789.

## Success

1. Every new candidate has one validated outcome for each O1–O13 case on tmpfs and btrfs.
2. Corpus cost and stability recorded for the new candidates (local evicted, network subset).
3. Probe observes ctime per step on tmpfs, btrfs and CIFS, or records it not measured.
4. Observed detection fraction per random candidate sits within 3 standard errors of `k/M`.
5. The report keeps constructed misses and observed rates apart and states residual risk.

## Validation

- Candidate sample plans and fingerprints — `focused-test`: blocks are aligned, distinct and
  seed-reproducible; red before the candidates exist; green
  `python3 scripts/media-evidence-fingerprint-selftest.py`.
- Oracle outcomes — `focused-test`: `meta-ctime` detects O1/O13, `rnd64-*` miss O1 with seed
  knowledge; same command.
- `probability`, `ctime-probe`, `signals` records and privacy — `focused-test`: synthetic runs
  validate, scratch is removed, a private field fails validation; green
  `python3 scripts/media-evidence-reuse-selftest.py`.
- Committed records — `focused-test`: every reuse record validates; same command, wired as
  `just media-evidence-reuse-selftest` in `source-checks`.
- READMEs and report — `task-test-not-applicable`: prose read by humans with no executable
  consumer.
