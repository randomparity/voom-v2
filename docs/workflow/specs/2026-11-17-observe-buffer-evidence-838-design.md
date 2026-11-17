# Observe-path hash buffer at 16 KiB against 1 MiB — design (#838)

## Problem

ADR 0120 moved the owner hash to 1 MiB reads after #789. Four other full-hash reads still use a
16 KiB buffer and were never measured: `observe_file_facts` in the ffmpeg and mkvtoolnix workers
(transform input, before and after each operation), in the verify-artifact worker (output
verification), and in the ffprobe worker (the re-hash when probe reuse is off). The decision
report lists transform input as not measured.

## Scope

- **Instrument.** One example per crate, `observe-{ffmpeg,mkvtoolnix,verify-artifact,ffprobe}`,
  runs as one process on the workers' own runtime shape (multi-thread, 2 worker threads, one
  blocking pool), as one worker process serving concurrent operations. It reads paths from
  standard input until an empty line, then runs `<concurrency>` (its one argument) tasks that
  each take the next path and await `Box::pin(observe_file_facts(path))` from that crate. It
  writes one line per file, `ok <index> <start> <seconds> <size> <content_hash>` or
  `err <index> <start> <seconds> <variant>` (the error variant name only: messages can hold a
  path), then `done`, and waits for end of input so the harness can read the process's `/proc`
  counters. Output goes through `writeln!` on a locked stdout; `main` returns `Result`.
  Production source does not change.
- **Variants.** `media-evidence-candidates.py prepare observe16k|observe1m` builds the four
  examples in a detached worktree of HEAD. `observe1m` edits the four buffers to
  `vec![0_u8; 1024 * 1024]` (the ffprobe stack array becomes a heap buffer, as the hash worker's).
  Each edit must match once, as for the existing variants. Build arguments and the returned
  directory become part of each variant: the observe variants build the four packages with
  `--examples` and return `target/release/examples`.
- **Harness.** `scripts/media-evidence-observe.py run <manifest> --path <name> --worker-dir
  <dir>` replays one whole-file read per selected file through one example process at
  `--concurrency` (default the source `MAX_IN_FLIGHT`, 4, the protocol's stage-0 setting). It
  reuses the #788 manifest, eviction, residency, host counters
  and budget, and writes schema `media-evidence-observe-run/1`: the path, the buffer read from
  that crate's source (a pattern matching both `[0_u8; N]` and `vec![0_u8; N]`; the #788
  `read_constants` is not called), the example's SHA-256, per repetition wall, logical bytes, the
  process's CPU (with reaped children), peak RSS, `rchar` and `read_bytes`, and `hash_set_sha256` (SHA-256 over the sorted
  `id content_hash` lines; per-file hashes are not committed). Rows: id, size, ext, residency
  before, queue delay, observe seconds, outcome `observed` or `observe_failed`, and `failure`
  (`null` or the error variant name). The protocol's record-format section documents the schema;
  a `just media-evidence-observe-selftest` recipe joins `source-checks`.
- **Compare.** `media-evidence-observe.py compare <16k.json> <1m.json>` refuses cells that differ
  in path, corpus, ids, cache state or concurrency, or have equal buffers, and reports whether
  every repetition of both cells has one `hash_set_sha256`. It judges wall,
  summed observe seconds, physical bytes, CPU and peak RSS with the protocol thresholds through
  `media-evidence-candidates.py`'s `classify` (as-found: repetitions 2+, repetition 1 reported
  separately) and the resource guardrails.
- **Verdict rule** (fixed here, before any real-media cell; the rule of the decision report's
  gate G1 for the owner-hash buffer). Per path and cache state, 16 KiB is the baseline and
  `classify` gives each metric improvement, regression or inconclusive (CV above 20% is
  inconclusive). 1 MiB is **go** when CPU improves, wall and summed observe seconds do not
  regress, no resource guardrail is breached (peak RSS within baseline + 256 MiB; CPU), no row
  failed and the hash sets are equal; **no-go** when a row failed, the hash sets differ, a
  guardrail is breached, or CPU, wall or summed observe seconds regresses; otherwise
  **inconclusive**. Physical bytes are reported, not judged: both buffers read every byte once.
  No cell is rerun at 5 repetitions, so an inconclusive metric stays inconclusive. Verdicts are
  reported per path, beside the disclosure that the ffmpeg, mkvtoolnix and verify-artifact loops
  are byte-identical and ffprobe differs only by a stack buffer.
- **Capture.** Local corpus, all 34 files, 3 repetitions, at one commit, 16 cells. Order
  alternates so neither buffer always runs second: evicted ffmpeg 16k, 1m; mkvtoolnix 1m, 16k;
  verify-artifact 16k, 1m; ffprobe 1m, 16k; then as-found in the same path order with each
  pair reversed. The corpus is about 7× RAM, so as-found cells are expected to be mostly cold,
  with repetition 1 inheriting the previous cell's tail; per-repetition residency is reported. A cell over the 60-minute budget is
  split or excluded and recorded, never silently dropped.
- **Report.** `docs/media-evidence-baseline/observe/README.md` with the compare tables and
  verdicts; the decision report gains a row and loses "transform input not measured".

### Failure model

1. Actors and deployments: one local operator on one Linux host; CI runs selftests only.
2. Invariants and assets at stake: media is read-only; committed records carry no host path,
   file name or per-file hash; production buffers and defaults are unchanged; existing
   baseline and candidate records keep validating.
3. Accepted failure classes: drive and controller caches, other host activity, and the
   as-found cache state left by the previous cell are not controlled; btrfs metadata stays
   cached after per-file eviction; disclosed, and the alternating order keeps it from always
   favouring one buffer. The examples time the function, not a whole transform: a real output
   is observed while still cached from its write, and real input observation competes with the
   transform's CPU; disclosed.
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
| Compare refusals and verdict rule | focused-test | observe selftest: path, ids, cache, equal buffers refused; hash-set mismatch, failed row and regression give no-go; go; inconclusive |
| Examples call production code | focused-test | release build of both variants; fixture hashes equal across buffers before capture |
| Measurements and verdicts | task-test-not-applicable | real-media timings cannot run in CI |

Green: `just media-evidence-{baseline,candidates,observe}-selftest`, `just source-checks`,
`just platform-checks`.
