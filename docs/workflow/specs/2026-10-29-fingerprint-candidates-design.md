# Partial-fingerprint candidates (#796)

## Problem

Epic #787 needs evidence on whether a partial fingerprint could replace a full read. The
[protocol](../../media-evidence-protocol.md) defines stage 3 and oracle O1–O13; nothing runs them.

## Scope

- `scripts/media-evidence-fingerprint.py` imports `media-evidence-baseline.py` helpers.
  Frozen dataclass `Fingerprint` (candidate, size, mtime ns, dev/ino file key, BLAKE2b of
  sampled ranges) has no conversion into any production type.
  Candidates: `meta` (stat only); `ht64`, `ht1m` (head and tail of 64 KiB or 1 MiB);
  `ht64-i8`, `ht64-i32` (`ht64` plus 8 or 32 evenly spaced 64 KiB interior samples).
  Ranges are clipped and merged. `pread` reads sit between pre/post `fstat` drift checks on
  size, mtime, dev, ino and nlink, as the hash worker does.
- `oracle`: builds O1–O13 per candidate in a scratch directory it creates and removes.
  Edits whose placement is free go to the first byte the candidate does not read (a
  constructed miss), mtime restored where the case says so. O10 injects a write between
  reads; O13 looks up a fingerprint-keyed cache. A whole-file reference digest proves each
  case's bytes changed or did not.
- `run`: per candidate, fingerprints a manifest subset at concurrency 4, 3 repetitions;
  records wall, CPU, logical bytes, `/proc/self/io`, host counters, residency, and observed
  stability and distinctness. Local `--evict`; network `as-found`, at most three files.
- Guard `check-media-evidence-fingerprint-guard.sh` + selftest (in `source-checks`) fails on
  production identifiers (`content_hash`, `ObservedFileFacts`, `HashFileResult`,
  `file_versions`, `blake3`, `sqlite`, `voom_store`, `INSERT INTO`, `/v1/operations`) in
  experiment scripts or records, or an empty scope.
- Records and `fingerprint-results.md` in `docs/media-evidence-baseline/`.
- Excluded: O14–O15 (#797); harness/protocol edits; real-media oracle copies (byte position,
  not content, decides detection).

### Failure model

1. Actors: one operator on a Linux host; the selftest on Linux and macOS CI.
2. Invariants: originals never written; no fingerprint enters production evidence;
   committed files hold no host path or file name; every metric is a number or
   `unavailable`.
3. Accepted: metadata-scale reads still trigger readahead (measured, not hidden); host
   counters include rehearsal traffic (disclosed); a textual guard misses an obfuscated
   reference (experiment code is reviewed).
4. Elsewhere: decision #791; reuse and consolidated report #798; full-read tuning #789.

## Success

1. Oracle record: an outcome per candidate for each case O1–O13, with reference.
2. Local and network records: cost per candidate, three repetitions.
3. Results note: commands, exclusions, failures, constructed misses apart from observed rates.
4. Guard red on a production identifier; `just source-checks` runs guard and selftests.

## Validation

- Sample plans — focused-test: empty, short, overlapping, large sizes give merged ranges.
- Drift — focused-test: a write between reads raises drift; a stable file does not.
- Oracle — focused-test: small-fixture oracle gives expected outcomes (`meta` misses O1,
  head/tail candidates detect O2–O4, O8 equal) and removes its scratch directory.
- Cost record — focused-test: a fixture `run` validates, has no host path, keeps mtimes.
- Committed records — focused-test: the selftest validates every fingerprint record.
- Guard — focused-test: the selftest plants each forbidden token and an empty scope.
- Results note prose — task-test-not-applicable: no executable consumer.
