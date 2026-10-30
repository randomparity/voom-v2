# Single read: one full read per file (stage 5, #801)

Captured on 2026-10-29 (local time) under the frozen [protocol](../../media-evidence-protocol.md),
stage 5, "combined owner-side hash and probe in one pass". Design:
[the #801 spec](../../workflow/specs/2026-10-29-single-read-design.md). These are measurements
of one host and one bench-only variant, not a decision. #791 owns the decision. Production
source and defaults are unchanged. The raw records are the two JSON files beside this note.
Every measured number below comes from them, rounded for display. Capture times and the
manual correctness check are operator-recorded, not in the records. The verdicts are output of
`scripts/media-evidence-candidates.py compare`, and the modelled timings are output of its
`model` subcommand.

## What ran

`media-evidence-candidates.py prepare single-read` built both workers in a detached worktree
of commit `805f1729`, with two edits that exist only in that worktree:

- `crates/voom-hash-worker/src/hash.rs`: the #789 stage-2 selection, buf1m (1 MiB heap
  buffer). The hash worker binary is byte-identical to #789's buf1m cells
  (`hash_worker_sha256` `f73aa370…`).
- `crates/voom-ffprobe-worker/src/handler.rs`: `probe_file` no longer calls
  `observe_file_facts`, the full BLAKE3 read, before and after ffprobe. Instead it runs a
  stat check (`symlink_metadata`, regular file only) that requires size and nanosecond mtime
  to equal the expected facts, before and after ffprobe. It echoes the expected content hash
  rather than computing it (`probe_worker_sha256` `6fd05d98…`).

So the hash worker's pass is the only full read of each file. ffprobe still reads what it
needs from the file. Feeding the hash pass's bytes to ffprobe was excluded by the operator,
since it would change ffprobe's semantics.

The #788 harness ran unchanged (`harness_sha256` `07f3550d…`, the same as #789's cells):
HashFile, then ProbeFile, 3 repetitions, concurrency 4. Each record's `git_changed_paths`
names both edited files. Every row of every repetition ended `observed`, and no file was
skipped for budget.

| Cell | Corpus, cache, files | Compared with |
|---|---|---|
| `single-read-local-corpus-evicted` | local, evicted, all 34 (935.3 GB) | #788 local record; #789 `stage1-buf1m` |
| `single-read-network-corpus-as-found` | network, as-found, `f0049,f0074,f0063` (24.7 GB) | #789 `stage2-c4-network`; #788 network record |

### Reads per file

The harness records `reads_per_file: 3` and counts `logical_bytes_read` as three whole-file
reads per `observed` row (2805.9 GB per local repetition). That is the replayed sequence's
constant, not what this variant read. Likewise `probe_observe_buffer_bytes: 16384` is read
from the unchanged `observe.rs`, a read path this variant's probe worker no longer runs.
This variant does one full read per file, against three for #788 and #789:

| | #788 / #789 | single-read |
|---|---:|---:|
| Full reads per file | 3 | 1 |
| Hash worker logical read per local repetition (`rchar`) | 935.3 GB | 935.3 GB |
| Probe worker read of corpus bytes | 2 × corpus | none |

The probe worker's own counters were 0.15 GB `rchar` and 0.33 GB `read_bytes` per local
repetition. This run does not attribute them. ffprobe's own reads are **not measured**: the
per-process counters exclude the ffprobe child (the record's
`ffprobe_child_peak_memory_and_io` entry in `not_measured`), in this cell and in every
compared cell.

## Local, evicted (full pass)

Medians over 3 repetitions. Every repetition started with 0 resident bytes for every file.

| Metric | #788 (8 KiB) | buf1m (#789) | single-read | vs #788 | vs buf1m |
|---|---:|---:|---:|---|---|
| Wall (s) | 613.9 | 619.2 | 212.5 | improvement | improvement |
| First file done (s) | 10.3 | 10.3 | 6.2 | improvement | improvement |
| Physical read (GB) | 2765.8 | 2741.3 | 935.5 | improvement | improvement |
| Summed hash seconds | 896.4 | 798.5 | 839.5 | inconclusive | regression |
| Hash CPU (s) | 684.5 | 157.1 | 154.4 | improvement | inconclusive |
| Probe CPU (s) | 805.0 | 744.7 | 2.5 | improvement | improvement |
| Hash peak RSS (MiB) | 5.2 | 14.9 | 21.3 | regression | regression |
| Probe peak RSS (MiB) | 8.7 | 8.1 | 7.7 | improvement | inconclusive |

Wall was 212.2, 212.7 and 212.5 s, a 65.4% cut from #788 and 65.7% from buf1m. Physical
reads were 1.00× the corpus size, against 2.96× (#788) and 2.93× (buf1m). Summed probe
seconds fell from about 1540 (#788) and 1660 (buf1m) to 2.8. Probe CPU includes reaped
ffprobe children, so the probe worker's earlier CPU was almost all re-hashing.

No resource guardrail was breached. The peak-RSS "regressions" meet the 5% threshold only:
21.3 MiB is far below baseline + 256 MiB. The summed-hash regression against buf1m is +5.1%,
just over the 5% threshold. It is the same hash binary at the same concurrency, so the likely
cause is that four in-flight files now spend nearly all their time in the hash pass and
compete for the same storage. That cause was not isolated. The `compare` output's
"Stage-1 selection rule" line applies #789's stage-1 rule. It is not a stage-5 selection.

Modelled publication timing, labelled **modelled** (first durable observation was not
measured, so its verdict stays inconclusive):

| Cell | Barrier: first durable (s) | Per-file: first ready (s) | Per-file: median ready (s) | First batch at count 1 / 10 / 1000 (s) |
|---|---:|---:|---:|---|
| #788 local (8 KiB, c4) | 613.9 | 10.3 | 342.4 | 10.3 / 169.8 / 613.9 |
| stage1-buf1m (c4) | 619.2 | 10.3 | 346.2 | 10.3 / 171.7 / 619.2 |
| single-read (c4) | 212.5 | 6.2 | 120.8 | 6.2 / 63.9 / 212.5 |

The model's boundaries are those stated in
[stages 1–2](../stages-1-2.md#modelled-publication-timing).

## Network, as-found (bounded subset)

Repetition 1 is reported separately, so verdicts rest on repetitions 2–3. The cells are not
cache-equivalent, which the protocol's failure model accepts.

| Cell | Wall rep 1 (s) | Wall reps 2–3 (s) | Host CIFS read per rep (GB) | Resident before reps (GB) |
|---|---:|---:|---|---|
| #788 (8 KiB) | 29.0 | 15.9, 15.0 | 24.7, 13.1, 5.9 | 0, 11.6, 18.8 |
| stage2-c4 (buf1m) | 16.1 | 22.4, 16.1 | 13.1, 17.5, 13.1 | 11.6, 7.2, 11.6 |
| single-read | 21.1 | 13.7, 16.1 | 24.7, 16.0, 18.8 | 0, 8.7, 5.9 |

Against `stage2-c4-network`: the reference's CV is above the protocol's 20% limit for wall
(22.8%), first file done (30.6%) and physical read (20.5%), so those verdicts are
inconclusive. Probe CPU fell from 19.6 s to 0.24 s (improvement). Summed hash seconds rose
from 24.5 to 33.1 (regression by threshold), confounded by the different resident state.
Hash CPU and peak RSS were inconclusive. Against #788's network record, every speed metric
was inconclusive (CV above 20% or no threshold met). Hash CPU (16.3 to 4.5 s) and probe CPU
(17.5 to 0.24 s) improved. Hash peak RSS rose from 4.8 to 10.0 MiB, a regression by the 5%
threshold and within the guardrail.

Both #788's repetition 1 and this cell's started with nothing resident. Each fetched about the
subset size over CIFS (24.66 and 24.69 GB). As #797 found, the probe's re-reads were already
served by the client page cache, so on this mount removing them saves CPU and local work
but not wire bytes.

## Correctness finding for #791 (separate from the speed result)

The variant is faster because it trusts the hash pass's content hash through the probe. Its
stat-only pre- and post-probe checks see size and mtime only. They cannot detect a same-size
edit with mtime restored that lands between the hash pass and the end of the probe, which is
the O10/O11 shape. The current probe worker re-hashes the file and catches it.

A manual constructed check on 2026-10-29, on a 1 MB scratch file outside both corpora, used
the `baseline` and `single-read` builds of `805f1729`. It hashed the file with the hash
worker, flipped one interior byte, restored mtime with `utime`, then sent ProbeFile with the
hash worker's facts:

| Build | Result |
|---|---|
| `baseline` (current probe worker) | `ARTIFACT_CHECKSUM_MISMATCH` at `pre_probe` |
| `single-read` | `probed`, with the stale content hash echoed in both sides: **missed** |

The single-read probe also accepted an expected content hash that did not match the file
(same size and mtime), because it never reads the bytes. This was a construction, not a
rate. Neither corpus run observed any mutation.

Whether ctime closes part of this gap is #798's question: `utime` cannot roll back ctime,
and in the construction above ctime changed while size and mtime did not. #798's open PR
#805 reports that a size + mtime + ctime fingerprint detects the same-size, mtime-restored
constructions on local filesystems, but that on the network mount the reported ctime follows
mtime and misses them. This note cites that result. It does not measure ctime.

So a single-read design trades this detection for the I/O saving. #791 has to decide whether
some other mechanism (for example ctime where storage supports it, or a later full
verification as in #797) covers the window, or whether the risk is accepted.

## Exclusions and not measured

- Not measured, as in each record's `not_measured`: discovery, first durable observation,
  planning, transformation, verified output, sidecar hashing, retries, and ffprobe child
  memory and I/O.
- Piping the owner's bytes into ffprobe (operator exclusion), production implementation and
  defaults (#791), fingerprint and lazy candidates (#796/#797), reuse and ctime (#798), and
  harness changes (#806).

## Disclosures

- Both captures ran under the exclusive host-wide corpus lock shared with concurrent
  benchmark workers, so no two captures overlapped. Builds by other workers took that lock in
  shared mode and did not run during a capture. Other host activity was not controlled or
  recorded. Local: 00:27–00:38 UTC on 2026-10-30. Network: 00:38–00:39 UTC.
- The network cell ran against the corpus shared with an active rehearsal. Host CIFS reads
  totalled 59.5 GB across the three repetitions, within the 100 GB per-cell budget. That
  load competed with the rehearsal, and host counters include rehearsal traffic.
- The network cell inherited client page cache from earlier activity, so its comparisons are
  not cache-equivalent.

## Reproduce

With private manifests from the #788 `manifest` command:

```sh
python3 scripts/media-evidence-candidates.py prepare single-read <scratch>/single-read
python3 <scratch>/single-read/scripts/media-evidence-baseline.py run \
  <private-dir>/local-manifest.json --max-bytes 3000000000000 --repetitions 3 --evict \
  --concurrency 4 --worker-dir <scratch>/single-read/target/release \
  --output docs/media-evidence-baseline/single-read/<date>-single-read-local-corpus-evicted.json
python3 <scratch>/single-read/scripts/media-evidence-baseline.py run \
  <private-dir>/network-manifest.json --ids f0049,f0074,f0063 --max-bytes 80000000000 \
  --repetitions 3 --concurrency 4 --worker-dir <scratch>/single-read/target/release \
  --output docs/media-evidence-baseline/single-read/<date>-single-read-network-corpus-as-found.json
git worktree remove --force <scratch>/single-read
python3 scripts/media-evidence-candidates.py compare <baseline.json> single-read=<record.json>
python3 scripts/media-evidence-candidates.py model <record.json>...
```
