# Node-agent commit copy loop at 16 KiB against 1 MiB — design (#898)

## Problem

`READ_BUFFER_BYTES = 16 * 1024` in `crates/voom-node-agent/src/commit.rs` sizes two loops. #889
extrapolates #838's verdict to the hash-only loop (`try_observe_regular_file`). The other,
`stream_copy_with_hash`, copies a staged artifact to a temp sibling of its target: each chunk is
hashed and written with `write_all`, and the copy ends with `flush` and `sync_all`. Its cost
includes the write and the fsync, so #838 cannot stand in for it, and a change to the shared
constant changes it too. It was never measured.

## Scope

- **Seam.** `stream_copy_with_hash` becomes `#[doc(hidden)] pub` (it was private), with a doc
  comment that it is public only for the bench example. Its body, signature and callers do not
  change. This is the smallest change that lets an example call the function: the example is a
  separate crate, so `pub(crate)` does not reach it, and a wrapper would add a second function
  for the same call. `commit` is already a `pub mod` and the return type `CommitObservedFacts`
  is already public. The issue's alternative, splitting the constant, is excluded (maintainer).
- **Instrument.** Example `crates/voom-node-agent/examples/copy-commit.rs` runs on the node
  agent's runtime shape (`#[tokio::main(flavor = "multi_thread")]`, as `main.rs`) and reuses the
  #838 runner (`voom-ffprobe-worker/examples/support/observe_runner.rs`, by `#[path]`), so the
  line protocol is the observe examples': paths on standard input, one
  `ok <index> <start> <seconds> <size> <content_hash>` or `err <index> <start> <seconds> <name>`
  line per file, then `done`. Arguments: `<concurrency> <scratch-dir>`. Per file it calls
  `stream_copy_with_hash(path, <scratch-dir>/copy-<n>.tmp)`, then removes the copy; `<seconds>`
  therefore covers copy, flush, fsync and removal. Failure names: `artifact_unavailable`,
  `commit_failure`, `other`, and `remove_failed` when the copy succeeded but removal did not.
- **Variants.** `media-evidence-candidates.py prepare copy16k|copy1m` builds the example in a
  detached worktree of HEAD (`-p voom-node-agent --examples`, binary directory
  `target/release/examples`). `copy16k` is the current source; `copy1m` edits the one constant
  line to `1024 * 1024`, matched once as for every variant. The edit also changes the hash-only
  loop, which the example does not call. No buffer edit reaches the branch.
- **Harness.** New `scripts/media-evidence-copy.py`, loading `media-evidence-observe.py`,
  `-baseline.py` and `-candidates.py` as modules and editing none of them.
  `run <manifest> --scratch-dir <dir> --max-bytes N --output <json> [--ids] [--repetitions 3]
  [--evict] [--example-dir]` refuses a scratch directory that is missing, not empty, inside the
  corpus root, or on a different device (`st_dev`) from the corpus root, and refuses a run that
  leaves anything in it after a repetition. Concurrency is fixed at 1: the coordinator drives
  commit intents one at a time, and one copy at a time bounds the scratch space to the largest
  file. Schema `media-evidence-copy-run/1` is the observe record with `path` fixed to
  `node-agent-copy`, `copy_buffer_bytes` read from the `READ_BUFFER_BYTES` line, `wchar` and
  `write_bytes` added to `process`, and per-file `copy_seconds` with outcome `copied` or
  `copy_failed`. The same redaction applies: corpus label, ordinal ids, one `hash_set_sha256`,
  no path and no per-file hash.
- **Compare.** `media-evidence-copy.py compare <16k.json> <1m.json>` refuses cells that differ in
  corpus, ids or cache state, or have equal buffers. It judges wall, summed copy seconds, CPU,
  physical read and write bytes and peak RSS with `candidates.classify` (as-found: repetitions
  2+, repetition 1 reported apart) and observe's resource guardrails.
- **Verdict rule** (fixed before any real-media cell; #838's rule). 16 KiB is the baseline. 1 MiB
  is **go** when CPU improves, wall and summed copy seconds do not regress, no guardrail is
  breached, no row failed and the hash sets are equal; **no-go** when a row failed, the hash
  sets differ, a guardrail is breached, or CPU, wall or summed copy seconds regresses;
  otherwise **inconclusive**. Physical bytes are reported, not judged. 16 KiB, the current
  value, is reported as the baseline in every cell. No cell is rerun at 5 repetitions.
- **Capture.** Local corpus, the same private manifest as #838. The host's corpus disk is a QLC
  NVMe with a dynamic SLC write cache and btrfs `compress=zstd:1`; a full-corpus copy writes
  935 GB per repetition, past any SLC cache, and wear grows with every cell. The run therefore
  uses a recorded subset of 7 ids spanning the size range (6–40 GB, about 150 GB), 3
  repetitions, four cells in alternating order: evicted 16k, 1m; as-found 1m, 16k. The scratch
  directory is a new sibling of the corpus on the same btrfs subvolume, removed at the end.
  Media is opened read-only. A cell over the 60-minute budget is recorded, never dropped.
- **Report.** `docs/media-evidence-baseline/copy/README.md` (compare tables, verdicts,
  disclosures, reproduce) beside the four records; the decision report's note that this loop is
  unmeasured (#889) becomes the measured verdict. The protocol's record-format section gains the
  schema; `just media-evidence-copy-selftest` joins `source-checks`.

### Failure model

1. Actors and deployments: one local operator on one Linux host; CI runs selftests only.
2. Invariants and assets at stake: media is never written; copies exist only in the scratch
   directory and are removed per file; committed records carry no host path, file name or
   per-file hash; production behaviour and defaults are unchanged.
3. Accepted failure classes, disclosed: the drive's SLC cache state, controller caches, btrfs
   metadata and other host activity are not controlled; the alternating order keeps any drift
   from always favouring one buffer. Kernel writeback and btrfs compression run in kernel
   threads, outside the process's CPU. Removal is inside `<seconds>` and process CPU for both
   buffers alike. The example times the copy only, not the coordinator's hard-link install,
   parent fsync or hashes around it.
4. Covered elsewhere: the hash-only loop (#889); a production buffer change or constant split
   (maintainer); network corpus (maintainer).

## Success

- Four valid records with every row `copied` and one `hash_set_sha256` across all four.
- Per cache state, the `compare` table and a verdict for 1 MiB against the 16 KiB baseline, in
  the README and the decision report.

## Validation

| Contract | Mode | Evidence |
|---|---|---|
| Seam changes visibility only | focused-test | diff is the attribute, `pub` and doc comment; `just platform-checks` (node-agent tests) green |
| Variant edits apply once | focused-test | candidates selftest: `copy1m` edit matches current source, `copy16k` empty |
| Scratch refusals and cleanup check | focused-test | copy selftest: missing, non-empty, inside-corpus, other-device and left-behind scratch refused |
| Replay accounting and redaction | focused-test | copy selftest with a stub example: rows, logical bytes, hash set, write counters, `err` row, no path |
| Record validation | focused-test | copy selftest: missing keys, bad outcome, host path rejected |
| Compare refusals and verdict rule | focused-test | copy selftest: ids, cache, equal buffers refused; failed row, hash mismatch, regression give no-go; go; inconclusive |
| Example calls production code | focused-test | release build of both variants; fixture copies hash equal across buffers before capture |
| Measurements and verdicts | task-test-not-applicable | real-media timings cannot run in CI |

Green: `just media-evidence-{candidates,observe,copy}-selftest`, `just source-checks`,
`just platform-checks`.
