# Media evidence experiment protocol (#788)

Status: **frozen on 2026-10-29, before any candidate result exists.** This protocol governs
the media-evidence experiments of epic [#787](https://github.com/randomparity/voom-v2/issues/787):
the read-efficiency and scheduling benchmark [#789](https://github.com/randomparity/voom-v2/issues/789),
the fingerprint and lazy-hash evaluation [#790](https://github.com/randomparity/voom-v2/issues/790),
and the phase decision [#791](https://github.com/randomparity/voom-v2/issues/791) that consumes
them. A change to any number here is a dated amendment merged *before* the candidate runs it
affects. An amendment made after seeing candidate results voids the comparison it touches; an
inconclusive or no-go result is a valid outcome. Baseline results are in
[media-evidence-baseline/README.md](media-evidence-baseline/README.md); the design is
[the #788 spec](workflow/specs/2026-10-29-media-evidence-baseline-design.md).

## Boundary with #471

[#471](https://github.com/randomparity/voom-v2/issues/471) owns the scan-session scale
performance budget and any CI gate for it. This protocol measures identity-evidence cost on
real media for an engineering decision. It sets no SLO, adds no CI timing assertion, and its
corpora are not CI workloads. Both use the same milestone names below. #471 may adopt them,
but neither owner's numbers bind the other.

## Phase inventory

Current behavior at the protocol's freeze. Every phase reads whole-file bytes to establish or
confirm identity.

| Phase | Current identity evidence | Code |
|---|---|---|
| Discovery | The scan worker enumerates primaries and sidecars | `crates/voom-scan-worker` |
| Probe and plan | Owner hash worker: BLAKE3 over 8 KiB reads with a pre/post stat drift check. Probe worker: BLAKE3 before and after ffprobe over 16 KiB reads. The pump publishes only when both agree. Evidence becomes usable at session completion. | `voom-hash-worker/src/hash.rs:26,193`; `voom-ffprobe-worker/src/handler.rs:59,69`; `voom-node-agent/src/scan_session.rs:37-42,801`; `voom-control-plane/src/scan/sessions.rs:508` |
| Transform input | ffmpeg and mkvtoolnix workers re-observe input facts before and after the operation | `voom-ffmpeg-worker/src/handler.rs:350,365,369`; `voom-mkvtoolnix-worker/src/handler.rs:168,190,194` |
| Output verify | The verify-artifact worker hashes the output against expected facts | `voom-verify-artifact-worker/src/handler.rs:120` |
| Promotion/commit | Commit authorization against verified facts. Ownership relocation belongs to #413/#528. | `voom-control-plane` |
| Repeat scan | A full rescan repeats the probe-and-plan reads. No evidence is reused. | as probe and plan |

## Milestones and metrics

Useful-work milestones are seconds from the start of the measured work:

| Milestone | Definition |
|---|---|
| Discovery complete | The candidate list is known |
| First durable observation | The first file's identity evidence is durable and usable by planning |
| First actionable plan | The first plan that could dispatch work exists |
| First transformation start | The first transform operation is accepted by a worker |
| First verified output | The first output passes verification |
| Complete library | Every selected file has a terminal outcome |

Per-stage metrics (a stage is one worker operation kind or one harness phase):

| Metric | Definition | Source |
|---|---|---|
| Wall time | Monotonic seconds per file per stage, and per repetition | harness |
| Queue delay | Seconds from repetition start until a file's first operation starts | harness |
| CPU | `utime+stime+cutime+cstime` of each worker process; includes reaped children such as ffprobe | `/proc/<pid>/stat` |
| Peak memory | `VmHWM` of each worker process; children excluded | `/proc/<pid>/status` |
| Retries | Count of retried operations per file | harness |
| Logical bytes | Bytes the application asked to read: size × reads performed. `rchar` cross-checks it. | harness, `/proc/<pid>/io` |
| Physical bytes | Bytes fetched from the storage layer by the worker (`read_bytes`). On network storage, host-wide interface and CIFS counters. | `/proc/<pid>/io`, `/proc/net/dev`, `/proc/fs/cifs/Stats` |
| Outcome | `observed`, `disagree`, `hash_failed`, or `probe_failed`, with failure class and code only | harness |

A metric a host cannot provide is recorded as `unavailable`, never zero or omitted. Worker
messages are never recorded because they can contain host paths. Host-wide counters on a
shared host include unrelated traffic. Each result states that.

## Corpus

- **Fixtures**: synthetic files generated per run in a scratch directory the run creates and
  removes. The mutation oracle uses only fixtures and copies.
- **Local corpus** (`local-corpus`): real media on a local disk.
- **Network corpus** (`network-corpus`): real media on a network filesystem mount that is
  shared with an active rehearsal. Runs use a bounded, recorded subset. They disclose
  interference: the harness load affects the rehearsal, and the rehearsal's traffic appears
  in host counters. A full network pass runs only when the operator schedules it.

All real media is read-only. Nothing is written, renamed, or re-timestamped; reads update
access time as production scans do. `media-evidence-baseline.py manifest` lists video files
into a **private** manifest outside the checkout, keeping relative paths, size, and
modification time. Committed records name a corpus by label and a file by its manifest
ordinal id (`f0001`, assigned in byte order of the relative path). Ids are stable while the
corpus listing is unchanged. A changed listing gets a new manifest, and its records do not
compare by id.

**Subsets.** Local: the full manifest when the budget allows. Otherwise, the operator
records the chosen ids. Network: at most three files, chosen by the operator to span the
size distribution and recorded by id.

## Cache states

- `evicted`: local storage only. Before every repetition, `--evict` drops each selected
  file's page-cache pages with `posix_fadvise(POSIX_FADV_DONTNEED)`, then records `fincore`
  residency. Only an `evicted` repetition whose recorded residency is 0 for every file may
  be called page-cache cold. Drive and controller caches are not controlled.
- `as-found`: residency is recorded before each repetition, and no cache claim is made. The
  network corpus is always `as-found`. Eviction there could disturb the rehearsal, and the
  server's cache is not observable.

Variance is reported per cache state. In `as-found` runs, repetition 1 is reported separately
from repetitions 2 and later.

## Budgets

Frozen before any candidate result:

| Budget | Value |
|---|---|
| Repetitions per cell | 3; raised to 5 only to resolve an inconclusive metric (below) |
| Local logical reads per repetition | ≤ 3.0 TB |
| Network logical reads per repetition | ≤ 80 GB and ≤ 3 files |
| Network physical reads per candidate cell | ≤ 100 GB |
| Wall time per cell | ≤ 60 minutes local, ≤ 45 minutes network |
| Retries | none inside a measured run; a failure is a recorded row |

A cell is one candidate configuration × one corpus × one cache state. A run that would exceed
a budget is not started: the harness stops scheduling files past `--max-bytes`, and the
operator splits or schedules the work instead.

## Comparison thresholds

Frozen before any candidate result. For each metric, `CV` is the baseline cell's coefficient
of variation (sample standard deviation divided by mean) over its repetitions.

- **Improvement**: the candidate median is better than the baseline median by at least
  `max(10%, 2 × CV)`, and the candidate's worst repetition is no worse than the baseline
  median.
- **Regression**: the candidate median is worse by at least `max(5%, 2 × CV)`.
- **Inconclusive**: neither holds, or the baseline `CV` exceeds 20%. An inconclusive metric
  may be rerun once at 5 repetitions within budget, and is otherwise reported inconclusive.
- **Resource guardrails**: a candidate regresses if a worker's peak memory exceeds baseline +
  256 MiB, or if its CPU seconds exceed 1.5 × baseline without a ≥ 25% wall-time improvement.
- **Eventual cost**: lazy or background candidates report total eventual logical and physical
  bytes, including deferred full hashes and wasted speculative work. A latency gain does not
  offset unreported eventual I/O.
- **Correctness**: oracle results are reported per case. Missed detections are never folded
  into a speed comparison or a rate.

Primary metrics, in priority order: first durable observation, complete library, total
logical and physical bytes, then CPU and memory. The #788 replay harness cannot measure
first durable observation, because it does not publish. A candidate owner comparing that
metric first captures a stage-0 cell with the same instrument it will use for candidates,
before any candidate run. Without that cell, the metric is reported inconclusive.

## Mutation oracle

Correctness matrix, defined here and executed by #790. Every case is built on fixtures or
copies in a scratch directory. Original media is never mutated. A **constructed miss** is a
case built to defeat a candidate. It shows that the candidate *can* miss, and is reported as a
per-case pass/fail. An **observed rate** comes from unconstructed runs over a finite corpus.
A clean finite corpus is not a universal collision or mutation-detection guarantee, and the
two are never combined. Fingerprints are reported separately from authoritative
`blake3:` content hashes and never stored as one.

| Case | Construction | Current full-hash behavior |
|---|---|---|
| O1 interior edit | Same-size overwrite of one interior block; mtime restored | detected (hash differs) |
| O2 head edit | Same-size edit in the first 64 KiB; mtime restored | detected |
| O3 tail edit | Same-size edit in the last 64 KiB; mtime restored | detected |
| O4 short file | Size below the head+tail window; edit any byte | detected |
| O5 empty file | Zero bytes, then one byte appended and truncated back | equal content, equal hash |
| O6 overlapping samples | Size where head, tail, and interior samples overlap; edit inside the overlap | detected |
| O7 timestamp precision | Content change with mtime equal at 1 s and 2 s granularity but different in ns | detected; mtime alone is not |
| O8 rename | Same bytes moved to a new path | same hash, new locator |
| O9 replacement | New inode at the same path, same size and mtime, different bytes | detected; `file_key` differs |
| O10 change during observation | A writer modifies the file while it is being hashed | drift rejected by the stat check |
| O11 change during use | Modified after observation, before transform input | caught by transform re-observation |
| O12 sidecar change | Sidecar bytes change while the primary is unchanged | detected by sidecar hash |
| O13 stale cache | Cached evidence kept after a content change with restored mtime | not applicable (no evidence reuse) |
| O14 interruption/restart | Worker killed mid-hash, then the scan resumes | no partial evidence published |
| O15 late disagreement | Provisional evidence accepted, then the full hash differs | not applicable (no provisional state) |

#790 reports, per candidate and case, detected or missed. For O14 and O15 it also reports the
disposition of plans and outputs.

## Staged candidate matrix

Each stage starts from the previous stage's selected configuration rather than a Cartesian
product. Stages and costs are planned here; each owner logs exclusions and actual costs.

| Stage | Owner | Cells | Estimated cost per cell |
|---|---|---|---|
| 0 Current behavior | #788 | Baseline: 8 KiB hash reads, 16 KiB probe reads, 4 in flight, flush at 1000 observations or 512 KiB | measured below |
| 1 Read efficiency | #789 | Hash buffer 64 KiB, 1 MiB, 4 MiB; read-loop batching | local full pass ×3 |
| 2 Scheduling | #789 | Concurrency 1, 2, 4, 8; bounded incremental publication; per-file readiness versus the full-library barrier | local full pass ×3 + network subset ×3 |
| 3 Fingerprints | #790 | Metadata only; size + mtime (ns) + head/tail hashes of 64 KiB and 1 MiB; head/tail + 8 or 32 interior samples | metadata-scale I/O + oracle |
| 4 Deferred full evidence | #790 | Provisional fingerprint + background or on-demand full hash; cached full evidence bound to (`file_key`, size, mtime ns) | full eventual I/O + oracle |
| 5 Single read | #789/#790 | Combined owner-side hash and probe in one pass | local full pass ×3 |

Exclusions recorded at freeze:

- Immutable snapshot/version evidence is excluded for both corpora. The network
  filesystem exposes no stable content version to the reader. Local filesystem snapshot
  features are not part of the production storage contract. #791 may reopen this only
  where storage semantics justify it.
- Concurrency above 8 is excluded. It exceeds the production in-flight bound by more than
  2×, with no evidence that storage benefits.
- Stage 3 and 4 cells never run on the network corpus at full scale. They use its bounded
  subset only.

## Reproducing a capture

Build the workers at the commit under test, then run:

```sh
cargo build --release --locked -p voom-hash-worker -p voom-ffprobe-worker
python3 scripts/media-evidence-baseline.py manifest <corpus-root> \
  --label local-corpus --output <private-dir>/local-manifest.json
python3 scripts/media-evidence-baseline.py run <private-dir>/local-manifest.json \
  --max-bytes 3000000000000 --repetitions 3 --evict \
  --output docs/media-evidence-baseline/<date>-local-corpus-evicted.json
python3 scripts/media-evidence-baseline.py run <private-dir>/network-manifest.json \
  --ids <id>,<id>,<id> --max-bytes 80000000000 --repetitions 3 \
  --output docs/media-evidence-baseline/<date>-network-corpus-as-found.json
```

`<private-dir>` must be outside the checkout. The harness refuses a private manifest inside
a Git worktree, except under the ignored `.agent/`. `just media-evidence-baseline-selftest`
validates every committed record.
