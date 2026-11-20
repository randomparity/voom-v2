# Node-agent commit copy loop: 16 KiB against 1 MiB (#898)

Captured on 2026-11-20 under the frozen [protocol](../../media-evidence-protocol.md). The
verdict rule and the subset were fixed before any real-media cell in
[the #898 spec](../../workflow/specs/2026-11-19-node-agent-copy-loop-898-design.md). These are
measurements of one host, not a performance budget, and production source and defaults are
unchanged. The raw records are the four JSON files beside this summary, schema
`media-evidence-copy-run/1`. The tables are output of `scripts/media-evidence-copy.py compare`,
rounded for display.

## What ran

`stream_copy_with_hash` in `crates/voom-node-agent/src/commit.rs` is the loop the commit
coordinator uses to copy a staged artifact to a temp sibling of its target. Each chunk of a
`READ_BUFFER_BYTES` buffer is hashed and written with `write_all`, and the copy ends with
`flush` and `sync_all`. The `copy-commit` example calls that production function on the node
agent's runtime shape (multi-thread, default workers), one process per repetition, one file at
a time: the coordinator drives commit intents one at a time. Each copy goes to a new file in a
scratch directory on the corpus's btrfs subvolume (same device, outside the corpus) and is
removed before the next file starts. The harness refused to start unless that directory was
empty and had room for two copies of the largest file. It checked again after every
repetition that the directory was empty. Media was opened read-only.

Both buffers were built by `media-evidence-candidates.py prepare copy16k|copy1m` in detached
worktrees of `1edfe839`. `copy1m` changes only the `READ_BUFFER_BYTES` line to `1024 * 1024`.
Each 1 MiB record's `git_changed_paths` names `commit.rs`, and `copy_buffer_bytes` is read from
the edited source. The function's visibility is the only production change on the branch
(`#[doc(hidden)] pub`, so a crate example can call it). Harness SHA-256 `f850c555…` in every
record. Later commits move the process counters onto the shared `/proc` helpers from #900. They
read the same fields, and the committed records still validate. Host: Linux x86_64, 32 CPUs,
124 GiB RAM. The corpus disk is a QLC NVMe with a dynamic SLC write cache, and btrfs is
mounted `compress=zstd:1`.

**Subset.** The spec's rule sorts the manifest by size (ties by id) and takes ranks
`round(i × (n − 1) / (k − 1))`, i = 0…k−1. That gives the smallest file, the largest file and
k − 2 evenly spaced files between them. k = 7 (192 GB) would have written about 2.3 TB over
four cells. k = 5 is the largest k inside the write volume the operator approved (operator
confirmation 2026-11-20 (write volume), about 1.8 TB). The selected ids are `f0004`, `f0009`,
`f0015`, `f0026` and `f0031`: 136.7 GB per repetition, 6.1–41.0 GB per file. The rule
replaces the protocol's operator choice of subset, and the orchestrator saw it before capture.
The full corpus is excluded because it would write 935 GB per repetition and wear the drive at
that rate on every cell. The subset bounds wear and time, not exposure to the SLC cache: about
410 GB per cell still runs past it.

**Budget.** An unrecorded calibration copy of the largest subset file (41.0 GB, page cache
evicted, 16 KiB) took 22.4 s, about 1.8 GB/s. At that rate three repetitions of the subset fit
well inside the 50-minute limit the spec set, so k stayed 5. Each cell was preceded by 10
minutes idle and ran in this order, so the two cache states see the buffers in opposite order:
evicted 16k 02:02–02:09 UTC, 1m 02:19–02:25; then as-found 1m 02:35–02:41, 16k 02:51–02:57.
Cells took 339–413 s each against a 60-minute budget.

Every row of every repetition ended `copied`; no file was skipped for budget. Every repetition
of all four records has the same `hash_set_sha256` (`dc9d8610…`), so both buffers hashed the
same bytes for every file. Every evicted repetition started with 0 resident bytes for every
file.

## Results

Medians over 3 repetitions (as-found: repetitions 2–3, repetition 1 below the table). Logical
reads are 136.7 GB per repetition in every cell.

| Cache | Wall 16k → 1m (s) | Summed copy 16k → 1m (s) | CPU 16k → 1m (s) | CPU change | Physical read / write 16k → 1m (GB) | Peak RSS 16k → 1m (MiB) | Verdict 1 MiB |
|---|---:|---:|---:|---:|---:|---:|---|
| evicted | 142.8 → 129.6 | 142.8 → 129.6 | 110.8 → 44.4 | −60.0% | 136.7 / 136.8 → 136.7 / 136.8 | 4.2 → 11.2 | **go** |
| as-found | 118.0 → 117.1 | 118.0 → 117.1 | 126.9 → 41.0 | −67.7% | 135.4 / 136.9 → 135.5 / 136.9 | 4.5 → 11.5 | **go** |

As-found repetition 1: wall 102.3 → 121.2 s, CPU 126.5 → 43.2 s.

Per metric, by the protocol thresholds: CPU is an **improvement** in both cache states
(baseline CV 3.7% evicted, 0.8% as-found). Wall and summed copy seconds are **inconclusive** in
both. The evicted baseline's CV is 23.6%, above the protocol's 20% limit: its repetitions took
102.6, 166.7 and 142.8 s against 111.5, 132.2 and 129.6 s at 1 MiB. As-found, the medians
differ by under 1% at a CV of 2.5%. Physical read and write bytes are equal within 0.03% (each
copy is written once). Peak RSS meets the 5% regression threshold in both, but it grows by
about 7 MiB, far inside the resource guardrail (baseline + 256 MiB). As in #838, memory is
judged by the guardrail. No guardrail was breached and no row failed.

**Verdicts.** 16 KiB, the current value, is the baseline in every cell. **1 MiB is go** in both
cache states. CPU improves by 60–68%, wall and summed copy seconds do not regress, memory stays
within the guardrail, and the hashes are equal. Both cache states are go, so the spec's
**overall verdict is go**. Process CPU covers the loop's user and system time in the process.
It leaves out kernel writeback, btrfs compression and discard, which run in kernel threads and
which this run did not measure. The figures are the process's CPU, not the copy's total CPU
cost on the host; whether host-wide CPU changes too is unknown.

## Observations

- **Wall time varies with something other than the buffer.** Both buffers copied at about
  0.8–1.3 GB/s per repetition, slower than the 1.8 GB/s calibration copy. One 29.6 GB file
  took 15.3 s in one 1 MiB repetition and 63.0 s in another. That spread is consistent with
  the drive's write state (not recorded) and makes the evicted wall CV exceed 20%.
- **CPU per byte.** The 16 KiB loop spends about 0.81–0.93 CPU-seconds per GB in the process;
  1 MiB spends about 0.30–0.32. On a 27.5 GB (corpus-mean) artifact that is about 22–26 against
  8–9 CPU-seconds per commit copy. Derived from bytes over buffer size, the 16 KiB loop makes
  about 8.3 million reads and as many writes per repetition, against about 130,000 of each at
  1 MiB, and tokio hands every call to its blocking pool. That handoff is the likely source of
  the difference, as in #838 (not profiled).
- **16 KiB CPU is higher as-found than evicted** (126.9 against 110.8 s), while 1 MiB is
  slightly lower as-found (41.0 against 44.4 s). The cause was not investigated.

## Disclosures

- The example times the copy loop only. It does not time the coordinator's hard-link install,
  parent-directory fsync or the hashes around the copy. Removing each copy happens inside
  `copy_seconds` and the process counters, for both buffers alike.
- The drive's SLC cache state, controller caches, btrfs metadata and other host activity were
  not controlled. The idle gap and the reversed order across cache states keep drive drift from
  favouring one buffer in both verdicts. Within each cache state the order was fixed. No other
  measurement or build ran on the host during capture.
- `write_bytes` counts bytes the process dirtied, before btrfs compression. Media is not
  compressible, so it is not expected to differ from what reaches the drive, but that was not
  checked.
- The network corpus is excluded and owned by the maintainer.
- No production buffer change is made here. One `READ_BUFFER_BYTES` change changes this loop
  and the hash-only loops (#889) together; a change belongs to a separate implementation issue.

## Reproduce

With a private manifest from the #788 `manifest` command and an empty scratch directory on the
corpus's filesystem, outside the corpus:

```sh
python3 scripts/media-evidence-candidates.py prepare copy16k <scratch>/copy16k
python3 scripts/media-evidence-candidates.py prepare copy1m <scratch>/copy1m
python3 <scratch>/copy1m/scripts/media-evidence-copy.py run <private-dir>/local-manifest.json \
  --scratch-dir <copy-scratch> --example-dir <scratch>/copy1m/target/release/examples \
  --ids f0004,f0009,f0015,f0026,f0031 --max-bytes 3000000000000 --repetitions 3 --evict \
  --output docs/media-evidence-baseline/copy/<date>-copy-1m-local-corpus-evicted.json
python3 scripts/media-evidence-copy.py compare <16k-record.json> <1m-record.json>
git worktree remove --force <scratch>/copy1m
```

As-found cells drop `--evict`. Run each variant's own harness: it refuses an example built
outside its checkout, because it reads the buffer from that checkout's source.
