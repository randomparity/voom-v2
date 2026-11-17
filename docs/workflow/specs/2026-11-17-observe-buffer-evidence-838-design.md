# Observe-path hash buffer at 16 KiB against 1 MiB — design (#838)

## Problem

ADR 0120 moved the owner hash to 1 MiB reads after #789. Four other full-hash reads still use a
16 KiB buffer and were never measured: `observe_file_facts` in the ffmpeg and mkvtoolnix workers
(transform input, before and after each operation), in the verify-artifact worker (output
verification), and in the ffprobe worker (the re-hash when probe reuse is off). The decision
report lists transform input as not measured.

## Scope

- **Instrument.** One example per crate, `observe-{ffmpeg,mkvtoolnix,verify-artifact,ffprobe}`,
  calls that crate's public `observe_file_facts` on each path read from standard input, on the
  workers' own runtime shape (multi-thread, 2 worker threads). It writes one line per file:
  `ok <seconds> <size> <content_hash>` or `err <seconds>` (no error text: it can hold a path).
  After an empty line it writes `done` and waits for end of input, so the harness can read the
  process's `/proc` counters. Production source does not change.
- **Variants.** `media-evidence-candidates.py prepare observe16k|observe1m` builds the four
  examples in a detached worktree of HEAD. `observe1m` edits the four buffers to
  `vec![0_u8; 1024 * 1024]` (the ffprobe stack array becomes a heap buffer, as the hash worker's).
  Each edit must match once, as for the existing variants.
- **Harness.** `scripts/media-evidence-observe.py run <manifest> --path <name> --worker-dir
  <dir>` replays one whole-file read per selected file through N example processes
  (`--concurrency`, default the source `MAX_IN_FLIGHT`, 4), one file at a time per process, as a
  worker handles one operation. It reuses the #788 manifest, eviction, residency, host counters
  and budget, and writes schema `media-evidence-observe-run/1`: the path, the buffer read from
  that crate's source, per repetition wall, logical bytes, summed process CPU, `rchar` and
  `read_bytes`, the largest process peak RSS, and `hash_set_sha256` (SHA-256 over the sorted
  `id content_hash` lines; per-file hashes are not committed). Rows: id, size, ext, residency
  before, queue delay, observe seconds, outcome `observed` or `observe_failed`.
- **Compare.** `media-evidence-observe.py compare <16k.json> <1m.json>` refuses cells that differ
  in path, corpus, ids, cache state or `hash_set_sha256`, or have equal buffers. It judges wall,
  summed observe seconds, physical bytes, CPU and peak RSS with the protocol thresholds through
  `media-evidence-candidates.py`'s `classify` (as-found: repetitions 2+, repetition 1 reported
  separately) and the resource guardrails.
- **Verdict rule** (fixed here, before any real-media cell). Per path and cache state, 16 KiB is
  the baseline. 1 MiB is **go** when no row failed, hash sets are equal, no guardrail is
  breached, wall, summed observe seconds and physical bytes do not regress, and CPU or summed
  observe seconds improves; **no-go** on a failed row, a guardrail breach or a speed regression;
  otherwise **inconclusive**. Peak RSS is judged by the guardrail only, as in #789.
- **Capture.** Local corpus, all 34 files, 3 repetitions, at one commit: per path, 16 KiB then
  1 MiB evicted, then 16 KiB then 1 MiB as-found (16 cells). A cell over the 60-minute budget is
  split or excluded and recorded, never silently dropped.
- **Report.** `docs/media-evidence-baseline/observe/README.md` with the compare tables and
  verdicts; the decision report gains a row and loses "transform input not measured".

### Failure model

1. Actors and deployments: one local operator on one Linux host; CI runs selftests only.
2. Invariants and assets at stake: media is read-only; committed records carry no host path,
   file name or per-file hash; production buffers and defaults are unchanged; existing
   baseline and candidate records keep validating.
3. Accepted failure classes: drive and controller caches, other host activity, and the
   as-found cache state left by the previous cell are not controlled; disclosed. The examples
   time the function, not a whole transform; disclosed.
4. Covered elsewhere: any production buffer change (a post-verdict issue); transform-input
   reuse (#793); network corpus (maintainer); control-plane and node-agent hashes (adjacent).

## Success

- Sixteen valid records, each with every row `observed` and one `hash_set_sha256` shared by all
  sixteen.
- Per path and cache state, the `compare` table and the 1 MiB verdict, in the README and the
  decision report.

## Validation

| Contract | Mode | Evidence |
|---|---|---|
| Variant edits apply once, per crate | focused-test | candidates selftest: `observe1m` edits match current source; drift refused |
| Replay accounting and redaction | focused-test | observe selftest with a stub example: rows, logical bytes, hash set, counters, `err` row, no path |
| Record validation | focused-test | observe selftest: missing keys, bad outcome, host path rejected; committed records pass |
| Compare refusals and verdict rule | focused-test | observe selftest: path, ids, cache, hash-set mismatch refused; go, no-go, inconclusive |
| Examples call production code | focused-test | release build of both variants; fixture hashes equal across buffers before capture |
| Measurements and verdicts | task-test-not-applicable | real-media timings cannot run in CI |

Green: `just media-evidence-{baseline,candidates,observe}-selftest`, `just source-checks`,
`just platform-checks`.
