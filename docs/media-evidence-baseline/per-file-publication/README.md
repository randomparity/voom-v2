# Per-file publication against completion publication (#841)

Captured on 2026-11-17 (UTC 2026-11-18 01:56–03:04) under the frozen
[protocol](../../media-evidence-protocol.md), for
[#841](https://github.com/randomparity/voom-v2/issues/841). Design:
[the #841 spec](../../workflow/specs/2026-11-17-per-file-publication-841-design.md). Decision:
[ADR 0132](../../adr/0132-per-file-scan-publication.md) (Proposed). These are measurements of
one host, not a performance budget; [#471](https://github.com/randomparity/voom-v2/issues/471)
owns scan SLOs. The raw records are the two JSON files beside this summary, and every number
below comes from them.

## What ran

Both cells are #794's G3 instrument (`scripts/media-evidence-rollout.py scan`, schema
`media-evidence-rollout-scan/2`): the real API, owner node agent and bundled workers on a fresh
database per repetition, one `voom scan --root` over the corpus root, timed from the request to
the first live rooted file location (first durable observation) and to every session terminal
(complete), polled read-only every 50 ms.

| Record | Publication | Probe reuse | Corpus, cache | Order |
|---|---|---|---|---:|
| `2026-11-17-stage0-completion-local-corpus-evicted` | completion (ADR 0077) | off | local, evicted, all 34 (935.3 GB) | 1 |
| `2026-11-17-per-file-local-corpus-evicted` | per-file (prototype) | off | local, evicted, all 34 | 2 |

The control is G3's stage-0 cell and ran first. The treatment differs only in the publication
setting.

**Prototype.** Branch
[`spike/per-file-publication-841`](https://github.com/randomparity/voom-v2/tree/spike/per-file-publication-841)
at `2638c04af2220c1b9ed807e707f97044e72a32f3`, from `main` at `e3b5f363`; it is never merged.
`voom-api --scan-per-file-publication` publishes each accepted batch's agreed evidence inside
the batch transaction and skips completion publication; absence reconciliation stays at
completion. The owner agent key `scan_per_file_publication = true` flushes every observation
as its own batch. Both default off, and with both off the control-plane and agent paths are
`main`'s. A build without the prototype refuses both switches, so a treatment cannot run as
the control.

**One build, one host.** Both records carry the same `build` and `host` sections: binaries
built `--release` from the clean spike checkout above (`voom-api` `34eb8864…`, `voom-node-agent`
`f75b4e6f…`, hash worker `5d22f8fd…`, probe worker `89151568…`, scan worker `f5c53531…`),
re-read unchanged after the last repetition of each cell; instrument from this branch at
`0dfcf0ce` with no changed tracked paths; ffprobe 8.1.3; Linux x86_64, 32 CPUs, 124 GiB RAM.
No build or test ran during capture.

**Corpus.** The #788 local corpus on NVMe-backed btrfs; a fresh private manifest matched the
committed listing (34 files, 935,300,661,466 bytes). Every repetition started with 0 resident
bytes for every file, and re-read size and mtime unchanged afterwards. Every repetition's 34
observations carried evidence, and all six published the same content-hash set
(`bfb46ce3…`), the same set as #794's stage-0 records.

## Results

| Cell | First durable observation (s) | Complete (s) | Batches | Published before complete |
|---|---|---|---:|---:|
| completion | 685.0, 675.6, 678.1 | 685.0, 675.6, 678.1 | 2, 2, 2 | 0, 0, 0 |
| per-file | 10.7, 18.2, 10.6 | 669.8, 677.9, 668.4 | 34, 34, 34 | 33, 34, 33 |

`compare` (protocol thresholds, baseline CV 0.7%):

| Metric | Completion median | Per-file median | Verdict |
|---|---:|---:|---|
| First durable observation | 678.1 s | 10.7 s (−98.4%) | improvement |
| Complete | 678.1 s | 669.8 s (−1.2%) | inconclusive: inside ±5–10%, no regression |
| Physical reads (hash + probe) | 2,803.1 GB | 2,773.6 GB | inconclusive |
| Probe `rchar` | 1,870.8 GB | 1,870.8 GB | inconclusive |
| Hash CPU | 159.5 s | 157.8 s | inconclusive |
| Probe CPU | 755.8 s | 750.7 s | inconclusive |

"Inconclusive" here is the protocol's neither-improvement-nor-regression: every baseline CV
is under 5%. Peak RSS stayed within 14.8–16.0 MiB (hash) and 7.8–8.7 MiB (probe) in both cells.

**Reading the numbers.**
- Per-file first durable observation is the first file's own hash and probe time: three full
  reads of whichever file finishes first among the four in flight. That was `f0004` (6.08 GB)
  in every repetition: the lowest location id in each repetition's private database, which is
  the one figure here not taken from the committed records. Repetition 2 took 18.2 s
  for the same file; the records do not show why. The verdict needs every treatment repetition
  below the control median, and the slowest is 37 times faster.
- The control's batch count is 2, not 1: the 512 KiB estimated-byte budget splits 34
  evidence-dense observations. Its `published_before_complete` of 0 in every repetition is the
  completion barrier observed: no poll ever saw a live location while the session ran.
- The treatment's 34 batches equal its 34 observations, and 33–34 locations were live before
  the session completed: the agent flushed per file and the control plane published per batch.
  A count of 33 means the last batch and completion committed between two 50 ms polls.
- 34 publication transactions instead of one cost nothing measurable on complete time.

**Against #794.** #794's stage-0 records on `main` at `2ca5dc66` (`rollout/`) took 670.8,
674.6 and 673.1 s; this control took 675.6–685.0 s on a later build. The two do not compare
under the protocol (different binaries) and no verdict is claimed; they agree within 2%.

## Not measured

- The network corpus, and any probe-reuse-on cell: G3's gating cell is the reuse-off stage-0
  cell, and #841 asks for that. With reuse on, #839 records the first file done at 6.2 s, so
  per-file first durable observation would be expected near that, but it was not measured.
- Session failure, cancellation, completion commit-lock conflicts and restarts with per-file
  publication: ADR 0132 states the intended behavior; nothing here exercises it.
- Concurrent policy planning during a running scan, and transform safety: not claimed.
- Everything the `not_measured` section of each record lists.
