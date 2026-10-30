# Media evidence rollout records (#794)

Captured on 2026-10-30 under the frozen [protocol](../../media-evidence-protocol.md), for the
[rollout gates](../../media-evidence-decision-report.md#rollout-gates) of ADR 0120. The
verdicts and the recommendation are in the
[rollout report](../../media-evidence-rollout-report.md). These are measurements of one host,
not a performance budget; [#471](https://github.com/randomparity/voom-v2/issues/471) owns scan
SLOs.

**Host and corpora.**
- Host: Linux x86_64, 32 CPUs, 124 GiB RAM, ffprobe 8.1.3, mkvmerge v102.0.
- Local corpus (`local-corpus`): 34 files, 935.3 GB, on NVMe-backed btrfs.
- Network corpus (`network-corpus`): 121 files on a CIFS mount shared with an active
  rehearsal. Only the three-file subset `f0049,f0074,f0063` (24.66 GB) was read.
- Both private manifests were regenerated and match the #788 listings exactly, so ids compare
  with earlier records.

**Builds.** Each record carries its commit and binary SHA-256s.
- The full-local, canary, setting-on kill and G4/G5 cells, and G1, ran on release binaries
  built after merging `main` at `3092e7c3`. That merge includes #821, the fix for
  [#816](https://github.com/randomparity/voom-v2/issues/816).
- Cells marked *pre-#816* ran at `f8be728c`. #821 changes only how large a probe snapshot may
  grow, so those cells' scan, guard and transform behavior is the same.

## Instruments

- **`scan` records** (`media-evidence-rollout-scan/1`).
  - Run by `scripts/media-evidence-rollout.py scan` on #339's acceptance stack, with a fresh
    SQLite database per repetition.
  - The stack is the real API, the owner node agent and six bundled workers, with
    `probe_hash_reuse` written explicitly.
  - Each root gets one `voom scan --root` request.
  - *First durable observation* is the first live file location the completion transaction
    publishes, polled read-only every 50 ms from the request. *Complete* is when every
    session is terminal.
  - A full pass scans the corpus root as one session, so first durable observation equals
    complete. A subset scans each file's own directory, one session per file.
  - Worker CPU and I/O include reaped ffprobe children; memory does not.
- **`construct` records** (`media-evidence-rollout-construct/1`).
  - The release hash and probe workers, driven over their protocol on a private copy of
    `f0004` (6.08 GB). The copy is a reflink on btrfs and a byte copy elsewhere.
  - `guard_sent` records that the probe request carried the ctime guard.
- **G1 records** (`media-evidence-baseline-run/1`).
  - The #788 harness, each cell run from its own checkout so that the recorded constants and
    changed paths describe the binary.
  - `buf8k` ran from the variant worktree that `media-evidence-candidates.py prepare buf8k`
    built; the 1 MiB cell ran from this branch.
- **G6, G9 and G7's version pairings** ran on the stack held by `media-evidence-rollout.py serve`
  or `acceptance-bootstrap.py smoke`. They were driven with the `voom` CLI, as in the
  [real-media runbook](../../runbooks/operator-real-media-execution.md), and are summarized
  below. Their databases hold host paths, so they stay private.

Every corpus run held the corpus lock exclusively, and every local cell evicted its files
first. Recorded residency in local cells was 0 for every file and repetition except one file
at 768 KiB in repetition 3 of the #816-fixed stage-0 cell. Network cells are as-found and
were not evicted.

Every `scan` repetition re-read the selected files' metadata afterwards and found size and
mtime unchanged. A final listing of both corpora matched the manifests.

## Cells

| Record | Cell | Reps | Result |
|---|---|---:|---|
| `…-g3-stage0-off-local-corpus-evicted` *(pre-#816)* | full local, off | 3 | Every session **failed** at 179.4–184.4 s: a 73,144 B probe frame exceeded the 64 KiB cap (#816). Nothing was published. |
| `…-g3-stage0-off-local-corpus-evicted-after-816` | G3 stage-0: full local, off | 3 | First durable observation and complete: 670.8, 674.6, 673.1 s. 34/34 published. |
| `…-g2-g3-on-local-corpus-evicted-after-816` | full local, on | 3 | 224.1, 223.7, 222.8 s. 34/34 reused. Physical reads 1.0003× the corpus. Probe `rchar` 0.016% of the corpus. |
| `…-canary-off/on-…-after-816` | 6-file subset, off / on | 5 + 5 | First durable 5.90 → 1.69 s. Complete 103.4 → 26.0 s. Physical 308.2 → 106.8 GB. |
| `…-g1-buf8k-…`, `…-g1-buf1m-…` | G1: harness, full local | 3 + 3 | Hash CPU 688.8 → 162.6 s. Summed hash 965.2 → 870.8 s. Wall 669.3 → 676.6 s. Hash peak RSS 5.1 → 16.2 MiB. First file done 16.0 → 18.2 s. All rows observed. |
| `…-g7-kill-hash-on-…` / `…-probe-on-…` | subset, on, worker SIGKILLed 3 s / 8 s after the request | 1 + 1 | Hash kill: one session failed with nothing published; 5 published with clean-run hashes. Probe kill: 6/6 published with clean-run hashes. |
| `…-g7-kill-hash-off-…` / `…-probe-off-…` *(pre-#816)* | subset, off, same kills | 1 + 1 | Hash kill: 6/6 published with clean-run hashes. Probe kill: one session failed with nothing published. |
| `…-g4-construct-btrfs`, `…-tmpfs` | G4, reflink copy (evicted) and tmpfs copy | 1 | Guard sent each time. Unchanged: reused. (a) `pre_probe_guard`, (b) `hash_drift`, (c) `pre_probe_guard`, all `ARTIFACT_CHECKSUM_MISMATCH`. |
| `…-g5-construct-ext4-fuse` | G5, ext4 image mounted with `fuse2fs` | 1 | Guard sent. Unchanged: recomputed, no hash source. (a) and (c) detected at `pre_probe` by the recomputed hash. (b) `hash_drift`. |
| `…-net-off-…` / `…-net-on-…` *(pre-#816)* | network subset, as-found | 3 + 3 | With the setting on, 0 reused and 3 recomputed in every repetition. Reps 2–3: first durable 9.37 → 8.97 s, complete 38.2 → 37.3 s. |

Every full-local repetition, off and on, published the same `(file, content hash)` set, and
so did every canary repetition. No stored scan row carried a hash source other than `reused`.
The pre-#816 records' classifier counted a null `hash_source` as absent. Later records count
it as a source, and serde never writes a null.
Every setting-off cell wrote no reused row. Each repetition uses a fresh database, so these
cells do not exercise turning the setting off on a database that already holds reused rows.

The kill timer fires on time after the scan request. The records do not show whether the
killed worker was mid-operation at that moment.

## CLI-driven gates *(pre-#816 build)*

- **G6 transform backstop.**
  - Setup: a stack with the setting on and one root holding a reflink copy of `f0004`. The
    scan reused the owner hash.
  - Then: a same-size interior edit to the copy, with the mtime restored, and a one-phase
    policy (`keep audio where channels >= 6`).
  - Result: the remux ticket failed `failed_terminal`, class `ArtifactChecksumMismatch`
    ("source facts disagree": same size, different BLAKE3). No remux file version, no commit
    intent, and no staged or output file.
- **G9 canary.**
  - Setup: a stack with the setting on and six roots holding reflink copies of the subset.
  - Policy: the flagship `reference-user.voom` and the `language-cleanup.voom` sample both
    block all six files at planning, because their `defaults` filters match 0, 3 or 5 kept
    streams. The canary therefore applied a defaults-free normalize phase into a separate
    output root: `container mkv`, `keep audio where language in ["eng","und"]`,
    `keep subtitle where language in ["eng"]`, `order tracks [video, audio, subtitle]`.
  - Job 1 completed `f0004`, `f0015` and `f0022`. `f0009` failed on truncated mkvmerge
    identify output ([#819](https://github.com/randomparity/voom-v2/issues/819)). `f0005`
    failed on an attachment in `clear_default_streams`
    ([#820](https://github.com/randomparity/voom-v2/issues/820)).
  - Resume job 2 completed `f0025` and re-failed both.
  - Resume job 3 dispatched only the two failing files and re-dispatched no completed file.
  - A second scan of the six roots published an identical content-hash set. Four outputs
    were verified, and none of their four `artifact_verifications.report` rows carries a
    hash source.
- **G7 mixed versions.** Protocol-5 release binaries were built at `4a1b37fa` and paired in
  `acceptance-bootstrap.py smoke` on a generated fixture:

  | Pairing | Outcome |
  |---|---|
  | protocol-6 agent with a protocol-5 hash or probe worker | refused at agent startup (`unsupported protocol version: offered=6, expected 5`); no lease |
  | protocol-6 control plane, protocol-5 agent and workers | leased, scanned, published |
  | protocol-5 control plane, protocol-6 agent, setting off | leased, scanned, published |
  | protocol-5 control plane, protocol-6 agent, setting on | leased; first batch rejected (`unknown field hash_source`); session failed, nothing published |

  Tracked as [#817](https://github.com/randomparity/voom-v2/issues/817).

## Timings and interference

Corpus time under the lock (local clock):

| Work | Time |
|---|---|
| Pre-#816 G1 (superseded: its `buf8k` record carried the harness checkout's constants) | 09:10–10:12 |
| Pre-#816 stage-0 | 09:04–09:08 and 10:28–10:37 |
| Pre-#816 canary and kill cells | 10:14–10:25 and 10:38–12:04 |
| Pre-#816 G4/G5 | 10:41–10:56 |
| G6 and G9 | 10:57–11:50 |
| #816-fixed full-local, canary, kill, G4 and G5 cells | 12:19–13:34 |
| G1 rerun | 13:34–14:42 |

The network corpus was touched at three times:
- 10:26: a manifest walk and three ffprobe header reads.
- 10:26–10:28: the off cell.
- 11:50–11:53: the on cell.

The network cells read 74.0 GB (off) and 123.3 GB (on) of host CIFS bytes, about 4.5 minutes
in total. The on cell's first repetition read 74.0 GB, 3× the subset, against 24.66 GB in
the off cell's first repetition. Both recorded zero client residency, and the cause is not
known. It may be the rehearsal's concurrent use of the share. As a result the on cell
exceeded the protocol's 100 GB per-cell physical budget by 23.3 GB. The logical budget
(80 GB per repetition) held. This load competed with the active rehearsal, and the host
counters include rehearsal traffic.

## Disclosures

- The first pre-#816 stage-0 attempt (09:04) was stopped during repetition 2 after
  repetition 1 failed. No record was kept for it.
- Before any stage-0, the instrument was validated with the setting on and off against
  generated fixtures on tmpfs. Those were not corpus cells.
- Ordering rule: on each build, the full-local stage-0 cell was captured before any
  setting-on cell. Setting-on cells on the pre-#816 build exist, but they predate the
  #816-fixed build that G2 and G3 are judged on. On the fixed build, the order of runs was:
  stage-0, the full setting-on cell, the canary and kill cells, the G4/G5 constructs, then
  G1. The fixed-build scan records list the three superseded construct records in
  `git_changed_paths`, because those records were staged for deletion before that batch
  started.
- Records committed before the review fixes say in `not_measured.ffprobe_child` that ffprobe
  I/O is not attributed to the probe worker. It is: Linux folds a reaped child's I/O into
  its parent's `/proc/<pid>/io`. Later records state this correctly.
- The first G6 attempt aborted on a driver error before execution. Two G9 attempts stopped
  at planning: identically named copies collided on branch id, and then the flagship's
  `defaults` blocked. Each rerun started from fresh copies and a fresh database.
- No ext4 mount was writable without privileges, so G5's ext4 arm ran through `fuse2fs`. The
  kernel reports that filesystem as `fuse`, which is also outside the reuse list. #813's CI
  ran the guard tests' recompute arm on a native ext4 test root.
- G9 outputs and all copies lived under an ignored scratch path and have been moved to the
  operator's trash. Nothing was written to either corpus.
