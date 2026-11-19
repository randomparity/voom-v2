# Observe-path hash buffer at 1 MiB — design (#879)

## Problem

The four `observe_file_facts` full hashes (transform input, output verification, ffprobe
re-hash) read 16 KiB per call; ffprobe's buffer is a stack array. #838 measured 1 MiB as go:
CPU −54 to −58%, wall unchanged, peak RSS about 4 → 21 MiB, identical hashes.

## Scope

- Each of the four `observe.rs` files reads into `let mut buffer = vec![0_u8; 1024 * 1024];`,
  a heap buffer as in the hash worker. The loop, errors and facts are otherwise unchanged. The
  literal line is kept, not a named constant, so the bench harness patterns read it as they do.
- No consolidation or ownership transition: the issue asks for no shared crate.
- Bench tooling follows the source: `media-evidence-candidates.py` `observe1m` becomes the
  current source (`{}`) and `observe16k` restores the old 16 KiB lines (ffprobe's stack
  array included), as `buf8k` did after #810. `media-evidence-baseline.py` widens the
  `probe_observe_buffer_bytes` regex to accept `vec!`; the record key is unchanged.
  `media-evidence-observe.py` comments (`:9-11`, `:45`, `:65-66`) and `candidates.py:101`
  are updated. Selftests follow.
- Decision report: the observe-buffer row records #879 landed; the phase table's transform
  input, output verification and probe rows name the 1 MiB buffer. ADR 0120 is unchanged.
- Excluded: control-plane and node-agent 16 KiB hashes (#889); transform-input reuse (rejected, ADR 0120).

### Failure model

1. Actors and deployments: node-agent-launched worker processes on operator hosts; bench
   harnesses run by an operator; CI (Linux, macOS).
2. Invariants: `content_hash` is a persisted contract — BLAKE3 over the whole file, the same
   for any buffer size; the pre/post metadata drift check is unchanged.
3. Accepted: about 17 MiB more peak RSS per process at 4 in flight (#838), within its
   baseline + 256 MiB guardrail.
4. Covered elsewhere: other 16 KiB hashes → #889.

## Success

- The four observe paths read 1 MiB per call into a heap buffer; no wire, contract or default
  change.
- For each of the four crates, a file of 2 MiB + 1 bytes yields a pinned literal hash, and
  files of exactly 1 MiB and 2 MiB match `blake3::hash` of the same bytes.
- The four `observe-*` examples build; `media-evidence-observe.py` and the baseline harness
  read 1 MiB from each source; `observe16k` applies once to each source.
- `just ci` exits 0.

## Validation

- focused-test: per-crate `observe_test.rs` multi-chunk test. Red: hashing the whole buffer
  instead of `&buffer[..read]` changes the 2 MiB + 1 hash. Green: `cargo test -p
  voom-<name>-worker observe`.
- focused-test: `media-evidence-observe-selftest.py` `BufferTests` and `RunTests` assert
  1 MiB for all four sources. Red: 16 KiB sources. Green: `just media-evidence-observe-selftest`.
- focused-test: `media-evidence-baseline-selftest.py` real-repo `read_constants`. Red: the
  old stack-array regex raises on the heap line. Green: `just media-evidence-baseline-selftest`.
- focused-test: `media-evidence-candidates-selftest.py` `VariantEditTests`: `observe16k`
  edits each source once back to 16 KiB; `observe1m` is `{}`. Red: the old `observe1m` edit
  finds no match. Green: `just media-evidence-candidates-selftest`.
- task-test-not-applicable: decision report row — prose with no executable consumer.
- task-test-not-applicable: example builds — no example source changes; `just ci` builds them.
