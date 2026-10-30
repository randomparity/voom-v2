# Hash buffer and ctime stability (#810)

## Problem

ADR 0120 contracts 1 and 2 (stability half) are unimplemented. The hash worker reads 8 KiB per
call (`HASH_CHUNK_BYTES`), and `assert_stable` compares size, mtime, dev, ino and nlink but not
ctime. A same-size interior edit that restores mtime during the hash read passes the check.
ADR 0120 is still `Proposed` although the maintainer accepted it on 2026-10-30.

## Scope

- `crates/voom-hash-worker/src/hash.rs`: `HASH_CHUNK_BYTES` becomes `1024 * 1024`, and the read
  buffer moves from the stack to a heap `Vec`, as #789's `buf1m` cell did. `StatFacts` gains
  `changed_at_ns: Option<i128>` (Unix `ctime * 1e9 + ctime_nsec`, `None` elsewhere). The derived
  equality in `assert_stable` then covers ctime. A mismatch keeps the `hash_drift` failure and its
  fact-free payload. `HashFileResult` and the protocol do not change.
- `docs/adr/0120-...md`: the Status becomes "Accepted (2026-10-30). The maintainer accepted the
  guarded probe-reuse trust change on 2026-10-30; implementation is #810 and #811." The index
  row stays as it is.
- `scripts/media-evidence-candidates.py` and its selftest anchor the #789 variants on the
  8 KiB stack-buffer text, which this change removes. #791 decided those variants, so `buf64k`,
  `buf1m`, `buf4m` and `batch8k` are deleted with their anchors and tests; `buf1m` is now
  `baseline`. A new `buf8k` swaps the constant back to 8 KiB (heap buffer) for #794's comparison.
  `single-read` keeps only its probe edits. Measurement logic and record schemas are unchanged.
  The two baseline READMEs get a one-line note each.
- Excluded: the probe guard, reporting ctime in the hash result, the protocol bump and the
  owner-node setting (#811), and G1 rollout measurement (#794).

### Failure model

- Mutation during the hash: any pre/post difference, ctime included, fails as
  `ARTIFACT_CHECKSUM_MISMATCH` with payload `{"stage":"hash_drift"}`. Nothing is published.
- A filesystem whose ctime follows mtime (the network mount in #798) adds no detection there.
  It also adds no false drift, because an unchanged file keeps its ctime.
- A ctime-only change during the hash (chmod, chown, xattr) now fails as drift. This is
  intended, and the next scan re-observes the file.
- One heap buffer per file; #789 measured 14.9 MiB peak hash RSS at concurrency 4.

## Success

- Each file is hashed with 1 MiB reads, and the digest equals an independent BLAKE3 across chunk
  boundaries.
- A ctime-only difference between the pre and post stats fails stability as `hash_drift`, with
  no facts.
- ADR 0120 reads Accepted, and `just ci` is green.

## Validation

| Contract | focused-test | task-test-not-applicable |
|---|---|---|
| 1 MiB chunk | `hash_chunk_is_one_mebibyte` | — |
| Hash correct across chunks | `file_spanning_several_chunks_hashes_across_chunk_boundaries` | — |
| ctime captured in ns | `stat_facts_capture_ctime_in_nanoseconds` | — |
| ctime drift is `hash_drift` | `ctime_only_change_is_hash_drift_without_facts` | — |
| Variants apply to live source | `media-evidence-candidates-selftest` | — |
| ADR Status | — | Prose status line; `just check-adr-index` covers the index |

Each focused test was shown to fail under a deliberate break of the code it covers.
