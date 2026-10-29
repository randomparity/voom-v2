# Partial-fingerprint candidates (#796)

Captured on 2026-10-29 under the frozen [protocol](../../media-evidence-protocol.md), stage 3.
The design is [the #796 spec](../../workflow/specs/2026-10-29-fingerprint-candidates-design.md).
The raw records are the JSON files beside this note, and every number below comes from them.
These are measurements of one host, not a performance budget.

**Result: no-go as a substitute for the full content hash; a go only for change hints.** Every
candidate misses a same-size edit with a restored mtime in bytes it does not read (O1, O13).
The fingerprints read four to six orders of magnitude fewer bytes than the full reads they
would replace. Whether a phase can use a fingerprint as a hint that still ends in a full hash is
for #791 to decide. That hint's eventual cost belongs to #797.

## What ran

`scripts/media-evidence-fingerprint.py` builds an experiment-only `Fingerprint`. It holds the
size, the mtime in ns, the file key (dev, ino), and a BLAKE2b digest of the sampled ranges. It
converts into no production type. `just check-media-evidence-fingerprint-guard` keeps
production identity fields, store writers, and the #788 worker replay out of the experiment's
scripts and records.

| Candidate | Reads |
|---|---|
| `meta` | `stat` only |
| `ht64` | first and last 64 KiB |
| `ht1m` | first and last 1 MiB |
| `ht64-i8` | `ht64` plus 8 evenly spaced 64 KiB interior samples |
| `ht64-i32` | `ht64` plus 32 evenly spaced 64 KiB interior samples |

Overlapping ranges are merged and read once with `pread`. A pre- and post-read `fstat` must
agree on size, mtime, dev, ino and nlink, the same fields the hash worker compares. Otherwise
the fingerprint is rejected as drift.

Commands, run from parent commit `205e6292` with this branch's final scripts in the working
tree; each record's `git_changed_paths` lists those scripts and the records being replaced.
Each record carries the script's and the imported #788 helper's SHA-256. The script hash
is that of `scripts/media-evidence-fingerprint.py` at `0c487038`; later commits only split
record validation into smaller functions.

```sh
python3 scripts/media-evidence-baseline.py manifest <local-root> \
  --label local-corpus --output <private-dir>/local-manifest.json
python3 scripts/media-evidence-baseline.py manifest <network-root> \
  --label network-corpus --output <private-dir>/network-manifest.json
python3 scripts/media-evidence-fingerprint.py run <private-dir>/local-manifest.json \
  --repetitions 3 --evict \
  --output docs/media-evidence-baseline/fingerprint/2026-10-29-fingerprint-local-corpus-evicted.json
python3 scripts/media-evidence-fingerprint.py run <private-dir>/network-manifest.json \
  --ids f0049,f0074,f0063 --repetitions 3 \
  --output docs/media-evidence-baseline/fingerprint/2026-10-29-fingerprint-network-corpus-as-found.json
python3 scripts/media-evidence-fingerprint.py oracle \
  --output docs/media-evidence-baseline/fingerprint/2026-10-29-fingerprint-oracle.json
```

Both new manifests listed the same file counts as #788's, 34 local and 121 network. The
network subset is #788's three files, 24.7 GB.

## Mutation oracle (constructed cases)

Each case runs on synthetic fixtures in a scratch directory on tmpfs, which is created and
then removed. Where the protocol leaves the edit position free, the edit goes to the first
byte the candidate does not read, so a `missed` is a **constructed miss**: it shows that the
candidate *can* miss, not how often it does. A whole-file reference digest confirms that each
case's bytes changed (or did not), and a case whose precondition fails is recorded as
`invalid`. None was. The record's `constructions` field states each case's construction.

| Case | Full hash | `meta` | `ht64` | `ht1m` | `ht64-i8` | `ht64-i32` |
|---|---|---|---|---|---|---|
| O1 interior edit, mtime restored | detected | missed | missed | missed | missed | missed |
| O2 head edit, mtime restored | detected | missed | detected | detected | detected | detected |
| O3 tail edit, mtime restored | detected | missed | detected | detected | detected | detected |
| O4 short file (100 KiB) | detected | missed | detected | detected | detected | detected |
| O5 empty: append, truncate | equal | false-change | false-change | false-change | false-change | false-change |
| O6 edit in sample overlap | detected | missed | detected | detected | detected | detected |
| O7 mtime differs only in ns | detected | detected | detected | detected | detected | detected |
| O8 rename | equal | equal | equal | equal | equal | equal |
| O9 replacement, same size and mtime | detected | detected | detected | detected | detected | detected |
| O10 write between reads | drift rejected | not-applicable | detected | detected | detected | detected |
| O11 ordinary write before use | detected | detected | detected | detected | detected | detected |
| O12 sidecar edit, mtime restored | detected | missed | detected | detected | detected | detected |
| O13 stale fingerprint-keyed cache | not applicable | missed | missed | missed | missed | missed |

- O5 is a conservative false change. The mtime moved while the bytes ended up equal, and the
  protocol does not restore it. A full hash would only be recomputed.
- O7 and O9 are detected through mtime ns and the inode, not sampled bytes. O7 ran on tmpfs,
  which keeps ns timestamps. A filesystem with coarser timestamps was not tested, and there
  O7 depends on that granularity.
- O10 is `not-applicable` for `meta` because a single `stat` has no window between reads.
  Every sampling candidate rejected the write as drift.
- O14 and O15 belong to #797 and were not run.

## Cost (corpus runs)

Concurrency 4, three repetitions per candidate, in the order shown. Logical bytes are the
merged ranges read. Physical bytes are the process's `read_bytes` on local storage and the
host's CIFS `Bytes read` on the network mount, where `read_bytes` is 0. Wall and CPU are
medians, CV in parentheses. CPU is the experiment process's user plus system time.

**Local, evicted** (34 files, 935.3 GB; 0 resident bytes before every repetition):

| Candidate | Logical (MB) | Physical (MB) | Wall (ms) | CPU (ms) | Slowest file (ms) |
|---|---:|---:|---:|---:|---:|
| `meta` | 0 | 0 | 1.4 (60%) | 1.6 | 0.4 |
| `ht64` | 4.46 | 9.33 | 8.2 (14%) | 10.6 | 3.1 |
| `ht1m` | 71.30 | 103.22 | 41.9 (4%) | 118.4 | 6.9 |
| `ht64-i8` | 22.28 | 28.27 | 30.8 (10%) | 41.4 | 6.0 |
| `ht64-i32` | 75.76 | 85.07 | 99.9 (11%) | 132.0 | 16.5 |

**Network, as-found** (3 files, 24.7 GB). Repetition 1 is reported separately, as the protocol
requires; repetitions 2 and 3 were served from the client cache, with 0 CIFS bytes read:

| Candidate | Logical (MB) | Rep 1 CIFS read (MB) | Rep 1 wall / CPU (ms) | Reps 2–3 wall (ms) | Reps 2–3 CPU (ms) |
|---|---:|---:|---:|---:|---:|
| `meta` | 0 | 0 | 1.6 / 1.7 | 0.5, 0.5 | 0.6, 0.5 |
| `ht64` | 0.39 | 0.99 | 5.1 / 1.9 | 0.8, 0.8 | 1.3, 1.4 |
| `ht1m` | 6.29 | 24.18 | 24.8 / 12.8 | 4.2, 3.7 | 11.2, 10.1 |
| `ht64-i8` | 1.97 | 1.67 | 5.3 / 4.0 | 2.1, 1.4 | 4.9, 3.5 |
| `ht64-i32` | 6.68 | 6.27 | 16.0 / 11.4 | 4.2, 3.9 | 11.3, 10.7 |

For scale, the #788 baseline read 2,805.9 GB logical in 613.9 s per local repetition and
74.0 GB per network repetition. The instruments differ. The baseline drives the release Rust
workers over HTTP; this experiment reads in-process from Python. Compare orders of magnitude,
not ratios, and no protocol threshold comparison is claimed against stage 0.

**Observed over this finite corpus** (kept apart from the oracle): in every cell, every file
gave the same fingerprint in all three repetitions, with no failures. Compared on size and
sampled bytes only (the inode and mtime would make any two files differ), every candidate told
apart all 34 local files and all 3 network files; `meta` did so by size alone. That says
nothing about collision or mutation-detection rates in general.

- Readahead makes physical reads exceed logical reads on local storage: 2.1× for `ht64` and
  1.1× for `ht64-i32`. On CIFS, `ht1m`'s first repetition read 3.8× its logical bytes.
- `meta`'s first local and network repetitions show about 50 KB of `rchar`, which is the
  process's own module loading, not corpus reads.

## Exclusions and deviations

- Stage 3 starts from stage 0's settings (concurrency 4, the protocol's in-flight bound). #789
  has not yet selected a stage-1/2 configuration.
- O14 and O15 belong to #797, and the oracle used no real-media copies. Detection depends on
  byte position and metadata, not on media content.
- Not measured, as recorded in each run's `not_measured`: first durable observation, per-file
  physical bytes, peak memory, and probing.
- No failures or retries occurred, and no run was discarded.

## Disclosures

- All runs held an exclusive host lock that serializes corpus measurements between parallel
  experiments. The local and network captures took 1 s (21:22:21–21:22:22 UTC), and the
  oracle, which reads no corpus, followed under the same lock. An earlier capture at
  20:27 UTC, made before the distinctness count was corrected, was discarded and not
  committed; its bytes read were identical and its timings of the same magnitude. Other host
  activity, including parallel experiment builds waiting on the lock, was not controlled.
- The network corpus is shared with an active rehearsal, and host counters include its
  traffic. Each capture added about 33 MB of CIFS reads (66 MB in total), negligible load on
  the rehearsal.
- Network cells ran back to back in one process. Each cell's first repetition found pages
  cached by earlier cells (the `resident_bytes_before` field), so later candidates' first
  repetitions are not cold. Server cache state is not observable.
