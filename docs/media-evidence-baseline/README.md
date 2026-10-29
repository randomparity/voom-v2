# Media evidence baseline: current behavior (#788)

Captured on 2026-10-29 under the frozen [protocol](../media-evidence-protocol.md). These are
measurements of one host, not a performance budget. [#471](https://github.com/randomparity/voom-v2/issues/471)
owns scan SLOs. The raw records are the JSON files beside this summary. Every number below
comes from them.

## What ran

The harness replays the current scan identity sequence for each file: a HashFile on the
release `voom-hash-worker` (BLAKE3, 8 KiB reads), then a ProbeFile on the release
`voom-ffprobe-worker` (BLAKE3 before and after ffprobe, 16 KiB reads). Hash facts are the
probe's expected facts. Four candidates are in flight, matching `MAX_IN_FLIGHT`. Each file
is therefore read three times. Workers were built in release mode from `main` at
`ed738f22`; this branch changes no crate, and each record carries the worker binaries'
SHA-256. The harness bytes (`harness_sha256` `e19c61f4…`) are those committed in `77bb392a`.
That harness listed untracked paths in `git_changed_paths` too; later harness versions list
tracked paths only, so the files named there were new, not modified.

Host: Linux x86_64, 32 CPUs, 124 GiB RAM, ffprobe 8.1.3. The local corpus is on NVMe-backed
btrfs. The network corpus is on a CIFS mount.

Not measured, recorded in each file as `not_measured`: discovery, first durable observation
(publication), first actionable plan, first transformation start, first verified output,
complete library, sidecar hashing, retries, and ffprobe child memory and I/O. The "first
file done" column below is the replay's first completed hash-and-probe. It is not the
first durable observation: the control plane publishes evidence only at session
completion.

## Results

| Cell | Files (GB) | Rep | Wall (s) | First file done (s) | Logical read (GB) | Physical read (GB) | CPU hash / probe (s) | Peak RSS hash / probe (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| local, evicted | 34 (935.3) | 1 | 613.9 | 10.4 | 2805.9 | 2765.8 | 690.8 / 805.0 | 5.0 / 8.2 |
| | | 2 | 612.7 | 10.3 | 2805.9 | 2765.6 | 648.7 / 868.4 | 5.2 / 9.0 |
| | | 3 | 615.0 | 9.9 | 2805.9 | 2767.1 | 684.5 / 772.9 | 5.3 / 8.7 |
| network, as-found | 3 (24.7) | 1 | 29.0 | 19.2 | 74.0 | 24.7 (host CIFS) | 20.5 / 19.4 | 4.7 / 5.4 |
| | | 2 | 15.9 | 13.8 | 74.0 | 13.1 (host CIFS) | 17.3 / 16.9 | 4.9 / 5.5 |
| | | 3 | 15.0 | 8.9 | 74.0 | 5.9 (host CIFS) | 15.2 / 18.1 | 4.8 / 5.6 |

Local physical read is the workers' `read_bytes`. Network physical read is the host-wide CIFS
`Bytes read` delta; on CIFS, worker `read_bytes` was 0. All 34 local files and all 3 network
files ended `observed` in every repetition, with no failures.

Variance (median; coefficient of variation):

- **Local, evicted.** Wall 613.9 s (0.2%). First file done 10.3 s (2.5%). Hash CPU 684.5 s
  (3.4%). Probe CPU 805.0 s (6.0%). Every repetition started with 0 resident bytes for every
  file, verified by `fincore`. Page-cache cold is established; drive cache is not controlled.
- **Network, as-found.** Repetition 1: wall 29.0 s. Repetitions 2–3: wall 15.0 and 15.9 s
  (4.1%), first file done 8.9 and 13.8 s (30.4%). This cell is not cold. Repetitions 2 and
  later found client page cache resident (11.6 GB and 18.8 GB), and server cache state is
  not observable. Under the protocol's thresholds, a network first-file comparison is
  inconclusive until rerun at 5 repetitions (CV above 20%).

## Observations for #789 and #790

- **Each read reaches the disk on the local corpus.** Probe workers fetched 1.83 TB from
  storage, about twice the corpus. Their pre- and post-probe passes did not hit page cache
  after the hash pass: four multi-gigabyte files in flight exceed what the cache keeps.
  Physical I/O is about 2.96× the corpus size.
- **Hash throughput is about 1.0 GB/s per stream** (median per-file, local, evicted) at
  8 KiB reads. A probe operation (two hash passes plus ffprobe) ran at about 1.17 GB/s of
logical reads per stream.
- **Queue delay dominates time to the last file.** With 4 in flight, the last local file
  started 557–560 s into a 613–615 s repetition.
- **On the network corpus, the probe re-reads were served from the client cache.** Host
  CIFS reads were about 1× the subset size in repetition 1, while logical reads were 3×.

## Disclosures

- The network corpus is shared with an active rehearsal. Its harness load (≈ 44 GB of
  CIFS reads at concurrency 4, about 1 minute) competed with the rehearsal. Host counters
  include rehearsal traffic.
- A first network capture of the same three files ran about 2 minutes earlier. It was
  discarded and not committed, because a harness defect corrupted its `git_changed_paths`
  field; the defect is fixed and covered by the self-test. Its measurements were valid:
  repetition 1 took 59.8 s, with first file done at 38.6 s and 24.7 GB of host CIFS reads.
  It may have warmed the server's cache, which is not observable, so the committed
  repetition 1 is not a first touch on the server. The client page cache was cold at that
  repetition: 0 resident bytes for every file, and CIFS reads equal to the subset size. The
  two runs together read ≈ 87 GB over the network, within the 100 GB cell budget.
- The local capture ran 2026-10-29 17:09–17:40 UTC. Other host activity was not
  controlled or recorded. The full local pass is the protocol's `evicted` local cell; no
  `as-found` local cell was captured.
