# Change-time and seeded-random candidates (#798)

Captured on 2026-10-29 under the frozen [protocol](../../media-evidence-protocol.md), as
operator-directed additions to stage 3 made **after** the protocol froze and after #796's
results were known. No protocol number changed, and no threshold comparison is claimed. The
design is [the #798 spec](../../workflow/specs/2026-10-29-evidence-reuse-design.md). The raw
records are the JSON files beside this note, and every number below comes from them unless a
line says otherwise. The consolidated #790 result is
[media-evidence-790-report.md](../../media-evidence-790-report.md).

**Result: change time detects the O1 construction on local storage and does not on the
network mount; seeded-random sampling detects an interior edit only with probability equal
to the fraction it samples.** Neither is a substitute for the full content hash.

## What ran

`scripts/media-evidence-fingerprint.py` gained three candidates; its five #796 defaults are
unchanged.

| Candidate | Reads | Compares |
|---|---|---|
| `meta-ctime` | `stat` only | `meta` (size, mtime ns, file key) plus ctime ns |
| `rnd64-i8` | first and last 64 KiB, plus 8 random interior 64 KiB blocks | size, mtime ns, file key, sampled bytes |
| `rnd64-i32` | the same with 32 random interior blocks | the same |

A random candidate draws k of a file's M = ceil(size / 64 KiB) − 2 aligned interior blocks
(k ≤ M) from a per-check seed. The before and after reads share that seed, which is equivalent
to checking the drawn blocks against a stored table of per-block digests. Building that table
takes one full read, and it holds 32 bytes per 64 KiB block: 457 MB for the local corpus and
12 MB for the network subset. In the oracle each check's seed is known to the constructor, so
an `rnd64` miss is a **constructed miss with seed knowledge**. In corpus runs each file gets
one secret seed per cell, held across that cell's repetitions so stability compares like with
like.

`scripts/media-evidence-reuse.py` adds two measurements:

- `probability`: 1000 trials per candidate on 8 MiB and 64 MiB tmpfs fixtures. Each trial
  flips one uniformly chosen interior byte with the mtime restored, then checks under a fresh
  seed drawn after the edit position. The editor therefore cannot know the sampled blocks.
- `ctime-probe`: one 64 KiB file in a fresh scratch directory on tmpfs, on the local corpus's
  btrfs (outside the corpus), and on the network mount (outside the corpus, beside it on the
  same share). Each step is a read (the control), a same-size content write, a `utime()` call,
  a content write with the mtime restored (the O1 construction), a `chmod` and a rename.
  ctime is read at once and again after 2 s, beyond the mount's 1 s attribute cache. The
  inode and the inode generation (`FS_IOC_GETVERSION`) are read before and after. The probe
  refuses a scratch directory inside either corpus root.

Commands, run from `db8b90bd` with a clean tree under the host-wide exclusive corpus lock.
Each record's `build` carries the commit and script hashes.

```sh
python3 scripts/media-evidence-baseline.py manifest <local-root> \
  --label local-corpus --output <private-dir>/local-manifest.json
python3 scripts/media-evidence-baseline.py manifest <network-root> \
  --label network-corpus --output <private-dir>/network-manifest.json
python3 scripts/media-evidence-fingerprint.py run <private-dir>/local-manifest.json \
  --candidates meta-ctime,rnd64-i8,rnd64-i32 --repetitions 3 --evict \
  --output docs/media-evidence-baseline/reuse/2026-10-29-reuse-local-corpus-evicted.json
python3 scripts/media-evidence-fingerprint.py run <private-dir>/network-manifest.json \
  --ids f0049,f0074,f0063 --candidates meta-ctime,rnd64-i8,rnd64-i32 --repetitions 3 \
  --output docs/media-evidence-baseline/reuse/2026-10-29-reuse-network-corpus-as-found.json
python3 scripts/media-evidence-fingerprint.py oracle --candidates meta-ctime,rnd64-i8,rnd64-i32 \
  --scratch-parent <tmpfs-dir> \
  --output docs/media-evidence-baseline/reuse/2026-10-29-reuse-oracle-tmpfs.json
python3 scripts/media-evidence-fingerprint.py oracle --candidates meta-ctime,rnd64-i8,rnd64-i32 \
  --scratch-parent <btrfs-dir-outside-the-corpus> \
  --output docs/media-evidence-baseline/reuse/2026-10-29-reuse-oracle-btrfs.json
python3 scripts/media-evidence-reuse.py probability --scratch-parent <tmpfs-dir> \
  --output docs/media-evidence-baseline/reuse/2026-10-29-reuse-probability.json
python3 scripts/media-evidence-reuse.py ctime-probe \
  --manifest <private-dir>/local-manifest.json --manifest <private-dir>/network-manifest.json \
  --scratch-parent <tmpfs-dir> --scratch-parent <btrfs-dir-outside-the-corpus> \
  --scratch-parent <network-share-dir-outside-the-corpus> \
  --output docs/media-evidence-baseline/reuse/2026-10-29-reuse-ctime-probe.json
```

Fresh listings matched #788's manifests file for file (34 local, 121 network), so ids
compare. The network subset is #788's three files, 24.7 GB.
`just media-evidence-reuse-selftest` validates every record here.

## Mutation oracle (constructed cases)

Same constructions as #796 (each record's `constructions` field). The tmpfs and btrfs records
gave identical outcomes, with no `invalid` row:

| Case | `meta` (#796) | `meta-ctime` | `rnd64-i8` | `rnd64-i32` |
|---|---|---|---|---|
| O1 interior edit, mtime restored | missed | **detected** | missed (seed known) | missed (seed known) |
| O2 head edit, mtime restored | missed | detected | detected | detected |
| O3 tail edit, mtime restored | missed | detected | detected | detected |
| O4 short file (100 KiB) | missed | detected | detected | detected |
| O5 empty: append, truncate | false-change | false-change | false-change | false-change |
| O6 edit in sample overlap | missed | detected | detected | detected |
| O7 mtime differs only in ns | detected | detected | detected | detected |
| O8 rename | equal | **false-change** | equal | equal |
| O9 replacement, same size and mtime | detected | detected | detected | detected |
| O10 write between reads | not-applicable | not-applicable | detected | detected |
| O11 ordinary write before use | detected | detected | detected | detected |
| O12 sidecar edit, mtime restored | missed | detected | detected | detected |
| O13 stale cache, mtime restored | missed | **detected** | missed (seed known) | missed (seed known) |

- `meta-ctime` detects every mtime-restored edit on tmpfs and btrfs because the `utime()` that
  restores the mtime moves the ctime. It pays with false changes on any metadata change: the
  rename (O8), and, in the probe, `chmod`. A false change costs one full re-hash.
- These outcomes hold only where ctime is the local kernel's change time. The network mount
  behaves differently (next section), so the O1 and O13 detections do **not** carry over to it.
- The `rnd64` misses are constructed with seed knowledge. Without it, detection is
  probabilistic (below).

## Change time on each filesystem (probe)

| Step | tmpfs | btrfs | network mount |
|---|---|---|---|
| read (control) | unchanged | unchanged | unchanged |
| content write | changed | changed | changed |
| `utime()` | changed | changed | changed |
| content write, mtime restored (O1) | changed | changed | changed at once, **unchanged after 2 s** |
| `chmod` | changed | changed | changed at once, **unchanged after 2 s** |
| rename | changed | changed | unchanged |

"Changed" means the settled ctime differs from the ctime before the step; every local step
also changed at once. No step changed the inode. The inode generation was `unchanged` for
every btrfs step and `unsupported` on tmpfs and on the network mount. A second network probe
at 23:31 UTC, written to a scratch file outside the committed records, gave identical results.

On the network mount the ctime the client reports follows the mtime. The client briefly shows
its own updated ctime, then the server's value replaces it on attribute revalidation. A
read-only `stat` of every corpus file at 23:31 UTC, not in a committed record, gave ctime equal
to mtime for all 121 network files and for none of the 34 local files. So on this share
`meta-ctime` is `meta` with a duplicated field: the O1 construction and the O13 stale cache
are **missed** there.

Not measured (each record's `not_measured`): a change made on the server side or by another
client, since this host has no server access, and whether the server honours a client-sent
ChangeTime.

## Detection probability for random sampling

For one edited interior block, a check that samples k of M interior blocks detects it with
probability f = k / M, the sampled fraction. Independent checks with fresh seeds detect it
after n checks with probability 1 − (1 − f)^n. Measured over 1000 trials each:

| Fixture | Candidate | M | f = k/M | Observed | Standard error |
|---|---|---:|---:|---:|---:|
| 8 MiB | `rnd64-i8` | 126 | 0.0635 | 0.071 | 0.0077 |
| 8 MiB | `rnd64-i32` | 126 | 0.2540 | 0.273 | 0.0138 |
| 64 MiB | `rnd64-i8` | 1022 | 0.0078 | 0.007 | 0.0028 |
| 64 MiB | `rnd64-i32` | 1022 | 0.0313 | 0.020 | 0.0055 |

Every observed rate is within 3 standard errors of f. The record's `analytic` table gives
1 − (1 − f)^n for f from 0.001 to 0.5 and n = 1, 10, 100.

At corpus scale, computed from the run records' file sizes with the same formula, f is tiny.
Locally the median f is 1.75 × 10⁻⁵ for `rnd64-i8` and 6.99 × 10⁻⁵ for `rnd64-i32`; on the
network subset it is 7.29 × 10⁻⁵ and 2.92 × 10⁻⁴. With `rnd64-i32`, a 50% chance of catching
one edited block takes about 9,900 checks of the median local file. Per check, f equals the
fraction of the file read, so random sampling buys no more detection per byte than reading
that fraction. Its value is unpredictability: an editor without the seed cannot aim at unread
bytes. The sampler here is Python's Mersenne Twister seeded from a secret, which is not a
cryptographic generator.

## Cost (corpus runs)

Concurrency 4, three repetitions. Wall and CPU in ms, per repetition 1, 2, 3.

**Local, evicted** (34 files, 935.3 GB; 0 resident bytes before every repetition):

| Candidate | Logical (MB) | Physical (MB) | Wall (ms) | CPU (ms) |
|---|---:|---:|---|---|
| `meta-ctime` | 0 | 0 | 2.7, 1.0, 1.1 | 3.0, 1.2, 1.2 |
| `rnd64-i8` | 22.28 | 27.16 | 38.1, 30.3, 30.3 | 44.2, 43.2, 42.5 |
| `rnd64-i32` | 75.76 | 80.63 | 115.9, 97.6, 96.9 | 135.0, 134.6, 135.6 |

**Network, as-found** (3 files, 24.7 GB):

| Candidate | Logical (MB) | Rep 1 CIFS read (MB) | Wall (ms) | CPU (ms) |
|---|---:|---:|---|---|
| `meta-ctime` | 0 | 0 | 1.8, 0.5, 0.5 | 1.9, 0.5, 0.5 |
| `rnd64-i8` | 1.97 | 2.56 | 7.3, 1.5, 2.3 | 4.8, 3.7, 4.3 |
| `rnd64-i32` | 6.68 | 6.29 | 16.9, 4.0, 4.1 | 11.8, 10.8, 10.6 |

Logical bytes match #796's `ht64-i8` and `ht64-i32` exactly. Network repetitions 2 and 3 read
0 CIFS bytes, served from the client cache. `rnd64-i32`'s first network repetition found
2.6 MB resident from the `rnd64-i8` cell before it. The figures exclude the full read that
builds the block table.

**Observed over this finite corpus**, kept apart from the oracle: every file gave the same
fingerprint in all three repetitions of every cell, with no failure. Every candidate told
apart all 34 local and all 3 network files on size and sampled bytes. Nothing changed during
the runs, so no detection rate was observed.

## Exclusions and deviations

- The candidates and both scripts' new subcommands were added after the protocol froze, at
  the operator's direction (#798). The protocol was not amended, and no improvement or
  regression verdict is claimed against stage 0.
- The protocol excludes immutable snapshot and version evidence. ctime and the inode
  generation are per-file metadata, not snapshot evidence, so they were measured here; the
  exclusion stands.
- O14 and O15 belong to #797. Buffer and concurrency tuning belongs to #789; runs use
  concurrency 4.
- Not measured, as in #796's records: first durable observation, per-file physical bytes,
  peak memory, and probing.

## Disclosures

- The main capture held the exclusive corpus lock from 23:22:46 to 23:23:49 UTC, and the
  confirming probe and `stat` from 23:31:26 to 23:31:40 UTC. Other host activity, including a
  sibling benchmark's builds waiting on the lock, was not controlled.
- The network corpus is shared with an active rehearsal. The corpus runs added 8.8 MB of CIFS
  reads. Each probe wrote 65,538 bytes to a scratch directory on the same share, outside the
  corpus, renamed and chmod-ed that file, and removed the directory; the record's
  `scratch_removed` is true, and a listing afterwards showed no residue. No corpus file was
  written.
- The tmpfs and btrfs probes and oracles wrote only to scratch directories they removed. The
  btrfs scratch directory is on the local corpus's filesystem, outside the corpus.
