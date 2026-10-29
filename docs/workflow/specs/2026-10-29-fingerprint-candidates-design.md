# Partial-fingerprint candidates (#796)

## Problem

Epic #787 asks if a partial fingerprint can replace a full read; nothing runs the
[protocol](../../media-evidence-protocol.md)'s stage 3 or oracle O1–O13.

## Scope

- `scripts/media-evidence-fingerprint.py` reuses #788 harness helpers. Frozen `Fingerprint`
  (candidate, size, mtime ns, dev/ino, BLAKE2b of sampled ranges) converts into no production
  type. Candidates: `meta` (stat); `ht64`, `ht1m` (64 KiB or 1 MiB head and tail); `ht64-i8`,
  `ht64-i32` (plus 8 or 32 spaced 64 KiB samples). Merged ranges are `pread` inside a
  size/mtime/dev/ino/nlink drift check.
- `oracle` builds O1–O13 per candidate in a removed scratch directory, with fixtures larger
  than any candidate's coverage. Free edits go to the first unread byte (constructed miss).
  Cases set mtimes with `os.utime(ns=…)`; O9 renames a replacement over the open original;
  O10 writes and bumps mtime between reads (`meta`: not-applicable); O11 re-observes
  before use; O12 fingerprints the sidecar; O13 reads a fingerprint-keyed cache. Outcomes:
  `detected`, `missed`, `equal`, `false-change`, `not-applicable`, or `invalid` when a
  precondition check (reference digest, mtime relation, inode) fails.
- `run`: per candidate, a manifest subset at concurrency 4, 3 repetitions: wall, CPU, logical
  bytes, `/proc/self/io`, host counters, residency, stability and distinctness counts.
  Non-local storage refuses `--evict` and a fourth file.
- Records (`media-evidence-fingerprint-oracle/1`, `-run/1`) and `README.md` go in
  `docs/media-evidence-baseline/fingerprint/`, outside the #788 selftest's glob. Records hold
  ids, booleans and counts only: no digest, dev, ino, path or file name.
- `check-media-evidence-fingerprint-guard.sh` + selftest, in `source-checks`, scans the
  fingerprint script, its selftest and records; it fails on an empty scope or `content_hash`,
  `ObservedFileFacts`, `HashFileResult`, `file_versions`, `blake3`, `sqlite`, `voom_store`,
  `INSERT INTO`, `/v1/operations`, `Worker`, `replay_file`, `HASH_FIELDS`, `OBSERVED_FIELDS`.
- Excluded: O14–O15 (#797); harness/protocol edits; real-media oracle copies.

### Failure model

1. Actors: an operator on Linux; selftests on Linux and macOS CI.
2. Invariants: originals unwritten; fingerprints stay out of production; records as above.
3. Accepted: readahead inflates physical reads (measured); host counters include rehearsal
   traffic (disclosed); the textual guard misses obfuscation (code is reviewed).
4. Elsewhere: decision #791; reuse and report #798; read tuning #789.

## Success

1. Oracle record: an outcome per candidate for each case O1–O13, none `invalid`.
2. Local and network records: cost per candidate, three repetitions.
3. Note: commands, exclusions, failures; constructed misses apart from observed rates.
4. Guard red on a forbidden token; `just source-checks` runs guard and both selftests green.

## Validation

- Sample plans — focused-test: empty, short, overlapping, large sizes merge correctly.
- Drift — focused-test: a size-changing write between reads raises drift.
- Oracle — focused-test: small run gives expected outcomes (`meta` misses O1, head/tail
  detect O2–O4, O8 `equal`, none `invalid`), a broken precondition gives `invalid`.
- Cost record — focused-test: an as-found fixture `run` validates, keeps fixture mtimes.
- Network bound — focused-test: non-local storage refuses `--evict` and a fourth file.
- Records — focused-test: committed fingerprint records validate; a digest field fails.
- Guard — focused-test: the selftest plants each forbidden token and an empty scope.
- #788 selftest — focused-test: green with fingerprint records committed.
- Results note — task-test-not-applicable: prose with no executable consumer.
