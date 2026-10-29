# Read efficiency and scan scheduling: stages 1–2 (#789)

Captured on 2026-10-29 under the frozen [protocol](../media-evidence-protocol.md) and compared
with the [#788 baseline](README.md). Design: [the #789 spec](../workflow/specs/2026-10-29-read-efficiency-scheduling-design.md).
These are measurements of one host, not a performance budget, and production source and
defaults are unchanged. The raw records are the `2026-10-29-stage*.json` files beside this
summary. Every number below comes from them, rounded for display. The verdict tables are
output of `scripts/media-evidence-candidates.py compare`, and the model table is output of
its `model` subcommand.

## What ran

Each cell is the unchanged #788 harness replay (HashFile, then ProbeFile; three whole-file
reads per file), 3 repetitions, on the host and corpora described in the #788 summary.
Variant workers were built by `media-evidence-candidates.py prepare` in a detached worktree
of the named commit, with one edit to `crates/voom-hash-worker/src/hash.rs`. Each record's
`git_changed_paths` names that file, and `hash_chunk_bytes` is read from the edited source.
The probe worker is the same binary in every cell, and matches #788's
(`probe_worker_sha256` `a9e725bc…`).

| Cell | Variant | Commit | Hash worker SHA-256 | Concurrency | Corpus, cache, files |
|---|---|---|---|---:|---|
| `stage1-buf64k` | 64 KiB heap buffer | `7a8d8060` | `29aa8eb2…` | 4 | local, evicted, all 34 |
| `stage1-buf1m` | 1 MiB heap buffer | `7a8d8060` | `f73aa370…` | 4 | local, evicted, all 34 |
| `stage1-buf4m` | 4 MiB heap buffer | `7a8d8060` | `1ba1736d…` | 4 | local, evicted, all 34 |
| `stage1-batch8k` | 8 KiB loop in one `spawn_blocking` task | `2faead33` | `531e23a4…` | 4 | local, evicted, all 34 |
| `stage2-c8` | buf1m | `7a8d8060` | `f73aa370…` | 8 | local, evicted, all 34 |
| `stage2-subset-c{1,2,4}` | buf1m | `7a8d8060` | `f73aa370…` | 1, 2, 4 | local, evicted, 6 files |
| `stage2-c{1,2,4}-network` | buf1m | `7a8d8060` | `f73aa370…` | 1, 2, 4 | network, as-found, 3 files |

The local subset is `f0004,f0025,f0022,f0005,f0009,f0015`: 106.8 GB, 6.1–41.0 GB per file,
in that order. The network subset is #788's `f0049,f0074,f0063`. Every row of every cell
ended `observed`. No file was skipped for budget. Every evicted repetition started with 0
resident bytes for every file.

Reproduce one cell, with private manifests from the #788 `manifest` command:

```sh
python3 scripts/media-evidence-candidates.py prepare buf1m <scratch>/buf1m
python3 <scratch>/buf1m/scripts/media-evidence-baseline.py run <private-dir>/local-manifest.json \
  --max-bytes 3000000000000 --repetitions 3 --evict --concurrency 4 \
  --worker-dir <scratch>/buf1m/target/release \
  --output docs/media-evidence-baseline/<date>-stage1-buf1m-local-corpus.json
git worktree remove --force <scratch>/buf1m
```

Subset cells add `--ids <ids>`. Network cells add `--ids f0049,f0074,f0063` and
`--max-bytes 80000000000`, and drop `--evict`. Compare and model:

```sh
python3 scripts/media-evidence-candidates.py compare <baseline.json> <name>=<candidate.json>...
python3 scripts/media-evidence-candidates.py model <record.json>...
```

## Stage 1: read efficiency (local, evicted, baseline = #788 local record)

Medians over 3 repetitions. The verdict uses the protocol's thresholds.

| Metric | 8 KiB (#788) | buf64k | buf1m | buf4m | batch8k |
|---|---:|---:|---:|---:|---:|
| Wall (s) | 613.9 | 611.1 inconclusive | 619.2 inconclusive | 621.3 inconclusive | 621.3 inconclusive |
| Summed hash seconds | 896.4 | 805.8 **improvement** | 798.5 **improvement** | 803.9 **improvement** | 818.2 inconclusive |
| First file done (s) | 10.3 | 10.4 inconclusive | 10.3 inconclusive | 10.5 inconclusive | 10.6 inconclusive |
| Physical read (TB) | 2.77 | 2.70 inconclusive | 2.74 inconclusive | 2.75 inconclusive | 2.72 inconclusive |
| Hash CPU (s) | 684.5 | 222.9 **improvement** | 157.1 **improvement** | 170.5 **improvement** | 230.4 **improvement** |
| Probe CPU (s) | 805.0 | 755.7 inconclusive | 744.7 inconclusive | 759.0 inconclusive | 792.2 inconclusive |
| Hash peak RSS (MiB) | 5.2 | 5.7 regression | 14.9 regression | 38.8 regression | 4.8 inconclusive |

No resource guardrail was breached: peak memory stayed far below baseline + 256 MiB, and CPU
fell. The RSS "regression" rows meet the 5% speed threshold only. The protocol judges memory
by its guardrail. **Selection: buf1m**, the best summed-hash improvement. No variant improved
wall time, so the rule fell through to summed hash seconds.

## Stage 2: scheduling (from buf1m)

**Local, full pass, concurrency 8 vs 4** (reference `stage1-buf1m`). Wall 642.3 s vs 619.2 s
(+3.7%, inconclusive). Summed hash seconds 1814.9 vs 798.5 (regression). First file done
17.1 s vs 10.3 s (regression). Physical read and CPU were inconclusive. Peak RSS rose
(hash 23.1 vs 14.9 MiB, probe 9.5 vs 8.1 MiB), within the guardrail.

**Local subset, concurrency 1 and 2 vs 4** (reference `stage2-subset-c4`):

| Metric | c4 | c2 | c1 |
|---|---:|---:|---:|
| Wall (s) | 78.1 | 74.2 inconclusive | 98.6 regression |
| Summed hash seconds | 78.4 | 36.8 improvement | 25.2 improvement |
| First file done (s) | 10.8 | 7.0 improvement | 5.7 improvement |
| Physical read (GB) | 306.2 | 257.6 improvement | 188.8 improvement |
| Hash / probe CPU (s) | 18.0 / 88.3 | 16.5 / 88.6 inconclusive | 16.1 / 89.4 inconclusive |

Logical reads were 320.5 GB per repetition in every subset cell.

**Network subset, concurrency 1 and 2 vs 4** (reference `stage2-c4-network`, as-found).
Repetition 1 is reported separately, so each comparison rests on repetitions 2–3. The
reference's wall CV is 22.8%, above the protocol's 20% limit, so every network concurrency
verdict is **inconclusive**:

| Cell | Wall rep 1 (s) | Wall reps 2–3 (s) | Host CIFS read per rep (GB) | Resident before reps (GB) |
|---|---:|---:|---|---|
| c4 | 16.1 | 22.4, 16.1 | 13.1, 17.5, 13.1 | 11.6, 7.2, 11.6 |
| c2 | 33.9 | 31.6, 31.8 | 24.7, 24.7, 24.7 | 11.6, 11.6, 11.6 |
| c1 | 51.7 | 37.5, 37.8 | 47.8, 24.7, 24.7 | 0, 7.9, 11.6 |

At concurrency 4, buf1m against #788's 8 KiB network record also stays confounded by cache
state. Wall for repetitions 2–3 was 19.3 s vs 15.4 s, a regression by threshold, but #788's
repetitions found 11.6 and 18.8 GB resident. Hash CPU was 4.6 s vs 16.3 s (improvement).
Probe CPU was 19.6 s vs 17.5 s (regression by threshold; this cell's probe binary is identical).
Network concurrency 8 is excluded: three files cap it at the concurrency-4 cell.

## Modelled publication timing

Labelled **modelled**. First durable observation was not measured: the replay does not
publish, so its comparison verdict is **inconclusive** in every cell. The model takes each file's
completion as queue delay + hash + probe seconds, and reports the median over repetitions.

| Cell | Barrier: first durable (s) | Per-file: first ready (s) | Per-file: median ready (s) | First batch at count 1 / 10 / 1000 (s) |
|---|---:|---:|---:|---|
| #788 local (8 KiB, c4) | 613.9 | 10.3 | 342.4 | 10.3 / 169.8 / 613.9 |
| stage1-buf1m (c4) | 619.2 | 10.3 | 346.2 | 10.3 / 171.7 / 619.2 |
| stage2-c8 | 642.3 | 17.1 | 398.2 | 17.1 / 201.4 / 642.3 |
| stage2-subset-c4 | 78.1 | 10.8 | 33.1 | 10.8 / 78.1 / 78.1 |
| stage2-subset-c2 | 74.2 | 7.0 | 22.8 | 7.0 / 74.2 / 74.2 |
| stage2-subset-c1 | 98.6 | 5.7 | 28.1 | 5.7 / 98.6 / 98.6 |
| stage2-c4-network | 16.1 | 9.5 | 14.0 | 9.5 / 16.1 / 16.1 |

Boundaries of the model:

- **Barrier (current).** ADR 0077 publishes identity inside the scan completion
  transaction, and ADR 0067 makes completion the only absence linearization point. Every
  file therefore becomes durable when the last one completes. The model omits enumeration
  (discovery) before the first hash, and the completion transaction itself. Neither was measured.
- **Flush cadence.** The agent's `BatchSubmitter` flushes at 1000 observations or about
  512 KiB of estimated batch bytes. Count 1000 is never reached by these corpora, so one
  batch leaves at the end. The byte bound was not modelled: observation wire size is not
  recorded. A batch's arrival is provisional catalog evidence (ADR 0067). Under the current
  contract, no cadence moves first durable observation.
- **Per-file readiness (hypothetical).** Publishing each file's identity when its own
  evidence agrees would need an ADR superseding ADR 0077's completion-gated publication. It
  covers present files only. Absence reconciliation still requires a complete traversal and
  stays at completion.

## Observations for #791

- On this host's local NVMe, a full pass is storage-bound at concurrency 4. Larger hash
  buffers cut hash-worker CPU by 67–77% and summed hash time by about 10–11%, without
  moving wall time. Physical reads stayed about 2.88–2.96× the corpus size.
- Concurrency 8 slowed the first completed file and per-file hash time, and did not shorten
  the pass.
- On the six-file subset, lower concurrency cut physical reads (188.8 GB at c1 vs 306.2 GB at c4,
  for 320.5 GB logical) and reached the first file sooner. Concurrency 1 lengthened the pass by
  26%; concurrency 2 did not lengthen it.
- Under the barrier, first durable observation equals complete-pass time, and no cadence or
  read-buffer change moved it. Only per-file readiness changes it in the model, and that
  needs a contract change.

## Exclusions and not measured

- Not measured, as in #788's `not_measured`: discovery, first durable observation, planning,
  transformation, verified output, sidecar hashing, retries, ffprobe child memory and I/O.
- Probe-worker buffer variants: the probe worker kept its 16 KiB buffer.
- The flush byte bound, and real publication through the control plane.
- Stages 3–4 belong to #790. Stage 5, a single combined read, is #801.

## Disclosures

- Every capture ran under an exclusive host-wide lock shared with concurrent benchmark
  workers, so no two captures overlapped. Builds by other workers took that lock in shared
  mode and did not run during a capture. Other host activity was not controlled or recorded.
  This issue's captures ran within 19:56–23:23 UTC, interleaved with other workers' captures.
- The network cells ran 22:33–22:38 UTC against the corpus shared with an active rehearsal.
  Host CIFS reads totalled about 215 GB across the three cells: 97.1 GB in c1, at the
  100 GB per-cell budget. That load competed with the rehearsal, and host counters include
  rehearsal traffic. Concurrency 1 repetition 1 read 47.8 GB over CIFS for a 24.7 GB subset
  that started with no resident pages; the cause is not established.
- Network cells inherited client page cache from earlier cells, so cross-cell network
  comparisons are not cache-equivalent.
