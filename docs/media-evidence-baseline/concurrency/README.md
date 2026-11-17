# Full-pass scan concurrency 2 against 4 (#839)

Captured on 2026-11-17 under the frozen [protocol](../../media-evidence-protocol.md), to replace
the subset-only concurrency-2 evidence behind the decision report's concurrency row. Design:
[the #839 spec](../../workflow/specs/2026-11-17-scan-concurrency-c2-839-design.md). These are
measurements of one host, not a performance budget; [#471](https://github.com/randomparity/voom-v2/issues/471)
owns scan SLOs, and no production default changes here. The raw records are the six JSON files
beside this summary. Every number below comes from them, rounded for display. The verdict
tables are output of `scripts/media-evidence-candidates.py compare`. Its stage-1 selection
line picks c2 at reuse off on summed hash seconds alone; that rule chose #789's hash buffer
and does not govern scheduling, where the protocol ranks first durable observation first.

## What ran

Each cell is the #788 replay (HashFile, then ProbeFile), schema `media-evidence-baseline-run/2`,
3 repetitions. With `--probe-reuse on` the probe carries ADR 0120's guard (ctime and file key
from the hash result), so on btrfs the probe reuses the owner hash and reads no file bytes:
one whole-file read per file instead of three.

| Record | Concurrency | Probe reuse | Corpus, cache | Order |
|---|---:|---|---|---:|
| `2026-11-17-c4-reuse-off-local-corpus-evicted` | 4 | off | local, evicted, all 34 (935.3 GB) | 1 |
| `2026-11-17-c2-reuse-off-local-corpus-evicted` | 2 | off | local, evicted, all 34 | 2 |
| `2026-11-17-c2-reuse-on-local-corpus-evicted` | 2 | on | local, evicted, all 34 | 3 |
| `2026-11-17-c4-reuse-on-local-corpus-evicted` | 4 | on | local, evicted, all 34 | 4 |
| `2026-11-17-c4-reuse-off-network-subset-as-found` | 4 | off | network, as-found, 3 files | 5 |
| `2026-11-17-c2-reuse-off-network-subset-as-found` | 2 | off | network, as-found, 3 files | 6 |

**One build, one host.** All six records carry the same `build` and `host` sections: commit
`cce3d0f5` with no changed tracked paths, harness `527f9e64…`, hash worker `cdc3e819…`, probe
worker `52c5c61f…` (release, built from `main` at `9aa8a7ff`; this branch changes no crate),
ffprobe 8.1.3, Linux x86_64, 32 CPUs, 124 GiB RAM. The current source constants apply: 1 MiB
hash reads, 16 KiB probe reads. No build or test ran during capture. The order alternates
concurrency between the two reuse settings, so neither concurrency always ran later.

**Corpora.** The local corpus is the #788 corpus on NVMe-backed btrfs; a fresh private manifest
matched the committed listing (34 files, same ids and sizes). The network subset is #788's
`f0049,f0074,f0063` (24.66 GB) on a CIFS mount shared with an active rehearsal, from a fresh
manifest of the same 121-file listing. Every local repetition started with 0 resident bytes
for every file. Every row of every cell ended `observed`. In the reuse-on cells all 34 rows
report `hash_source` `reused`; every other row reports `recomputed`. Size, mtime and ctime of
every local file, and the network listing, were unchanged after capture.

Local cells ran 2026-11-17 16:10–17:35 UTC, network cells 17:35–17:39 UTC. Other host activity
was not controlled or recorded.

## Results, local corpus, evicted

| Cell | Wall (s) | First file done (s) | Logical (GB) | Physical (GB) | CPU hash / probe (s) | Peak RSS hash / probe (MiB) |
|---|---|---|---:|---|---|---|
| c4, off | 655.5, 670.8, 659.4 | 10.8, 14.9, 10.1 | 2,805.9 | 2,736.0, 2,774.2, 2,733.3 | 158.9 / 743.1 | 16.8 / 7.9 |
| c2, off | 598.7, 603.3, 606.5 | 28.8, 25.4, 27.1 | 2,805.9 | 2,716.8, 2,662.2, 2,686.9 | 144.7 / 791.0 | 13.7 / 7.6 |
| c4, on | 225.4, 224.8, 225.0 | 6.1, 6.2, 6.3 | 935.3 | 935.5 each | 151.5 / 2.5 | 19.8 / 7.3 |
| c2, on | 200.0, 200.0, 200.1 | 9.3, 9.3, 9.3 | 935.3 | 935.5 each | 141.6 / 2.5 | 15.8 / 7.0 |

CPU and peak RSS are medians. Physical is the workers' `read_bytes`. With reuse on, the probe
worker's `rchar` was 0.15 GB per repetition (0.016% of the corpus), so physical reads were
1.0002× the corpus at either concurrency.

"First file done" is the replay's first completed hash-and-probe. It is **not** first durable
observation, which this instrument does not measure (#841); under ADR 0077 the control plane
publishes evidence only at session completion.

### Verdicts, c2 against c4 at the same reuse setting

| Metric | Reuse off: c4 → c2 median (CV) | Verdict | Reuse on: c4 → c2 median (CV) | Verdict |
|---|---|---|---|---|
| Wall | 659.4 → 603.3 s, −8.5% (1.2%) | inconclusive | 225.0 → 200.0 s, −11.1% (0.1%) | **improvement** |
| First file done | 10.8 → 27.1 s, +151% (21.8%) | inconclusive (CV above 20%) | 6.2 → 9.3 s, +50% (1.2%) | **regression** |
| Physical bytes | 2,736.0 → 2,686.9 GB, −1.8% (0.8%) | inconclusive | 935.5 → 935.5 GB, 0.0% | inconclusive |
| Summed hash seconds | 872.4 → 345.3 s | improvement | 888.7 → 392.4 s | improvement |
| Hash CPU | 158.9 → 144.7 s | inconclusive | 151.5 → 141.6 s | inconclusive |
| Probe CPU | 743.1 → 791.0 s, +6.4% | regression | 2.48 → 2.45 s | inconclusive |

Resource guardrails (peak memory within baseline + 256 MiB; CPU within 1.5× baseline): none
breached in either comparison. No row failed.

Reading the result:

- **Wall.** With reuse on, the read-once pass is about 11% faster at concurrency 2, clearing
  the 10% improvement threshold; with reuse off the 8.5% gain falls short of it.
- **First file done regresses, and the cause is ordering.** Files start in manifest order. At
  concurrency 4 the first window holds `f0004` (6.1 GB), which completes first in every c4
  repetition; at concurrency 2 the first window holds only `f0001` (38.0 GB) and `f0002`
  (21.8 GB), and `f0002` completes first. Every c2 repetition is slower than every c4
  repetition, at both reuse settings. The reuse-off verdict stays inconclusive only because
  the c4 baseline's CV (21.8%) is above 20%; it was not rerun at 5 repetitions.
- **Physical bytes do not change with reuse on**, because each file is read once either way.
  With reuse off, c2 fetched 1.8% less, inside the threshold.
- Summed hash seconds fall at c2 because fewer streams share the disk, so each hash call is
  shorter; it is a per-call measure, not throughput.

**Decision input.** Concurrency 2 gives an 11% shorter read-once full pass. Its later first
replay completion here comes from manifest order: on #789's six-file subset, where both cells
start with `f0004`, c2 completed it sooner (7.0 vs 10.8 s). Neither metric is first durable
observation, so this evidence does not select concurrency 2; a default change is a separate
issue.

## Network subset, as-found (reported, not gating)

Reuse off only: CIFS is outside ADR 0120's reuse list, so `on` would recompute (G5).

| Cell | Rep 1 wall / first file (s) | Reps 2–3 wall (s) | Reps 2–3 first file (s) | Host CIFS read (GB) | Resident before (GB) |
|---|---|---|---|---|---|
| c4 | 45.4 / 36.5 | 16.0, 22.3 | 9.4, 5.9 | 24.7, 13.1, 17.5 | 0, 11.6, 7.2 |
| c2 | 31.4 / 14.0 | 34.3, 31.5 | 15.1, 14.0 | 24.7 each | 11.6 each |

Reps 2–3 compare inconclusive on wall (c4 CV 23.2%), first file (32.5%) and physical bytes
(20.5%). The two cells did not see the same cache state: c4 repetition 1 started cold on the
client and later repetitions found part of the subset resident, while every c2 repetition
found `f0063` (11.6 GB) resident and fetched the whole subset again. The server's cache is not
observable. These cells say nothing about concurrency on this mount.

## Disclosures

- The network subset is shared with an active rehearsal. Its harness load (about 74 GB of
  logical reads per repetition, 129 GB of host CIFS reads across both cells) competed with
  the rehearsal, and host counters include rehearsal traffic. Both cells stay within the
  protocol's 100 GB physical budget per cell.
- Drive and controller caches are not controlled.
- The c4 reuse-off cell differs from #788's c4 record (8 KiB hash reads) and from the G1
  `buf1m` record (an earlier build); it is this run's own matched baseline.
