# Media evidence rollout records (#794)

Captured on 2026-10-30 under the frozen [protocol](../../media-evidence-protocol.md), for the
[rollout gates](../../media-evidence-decision-report.md#rollout-gates) of ADR 0120. The
verdicts and the recommendation are in the
[rollout report](../../media-evidence-rollout-report.md). These are measurements of one host,
not a performance budget; [#471](https://github.com/randomparity/voom-v2/issues/471) owns scan
SLOs.

Host: Linux x86_64, 32 CPUs, 124 GiB RAM, ffprobe 8.1.3, mkvmerge v102.0. The local corpus
(`local-corpus`, 34 files, 935.3 GB) is on NVMe-backed btrfs. The network corpus
(`network-corpus`, 121 files) is on a CIFS mount shared with an active rehearsal; only the
three-file subset `f0049,f0074,f0063` (24.66 GB) was read. Both private manifests were
regenerated and matched the #788 listings exactly, so ids compare with earlier records.
Binaries were release builds of `main` at `f8be728c` (protocol 6). This branch changes no
crate; each record carries its binaries' SHA-256.

## Instruments

- `scan` records (`media-evidence-rollout-scan/1`): `scripts/media-evidence-rollout.py scan`
  stands up #339's acceptance stack: real API, owner node agent and six bundled workers on a
  fresh SQLite database per repetition. It writes `probe_hash_reuse` explicitly and requests
  `voom scan --root` once per root. *First durable observation* is the first live file
  location the completion transaction publishes, polled read-only every 50 ms from the
  request. *Complete* is every session terminal. A full pass scans the corpus root. A subset
  scans each file's own directory, one session per file, so subset first-durable times are the
  first session to finish, not a whole-library barrier.
- `construct` records (`media-evidence-rollout-construct/1`): the release hash and probe
  workers driven over their protocol on a private copy of `f0004` (6.08 GB). The copy is a
  reflink on btrfs and a byte copy elsewhere.
- G1 records (`media-evidence-baseline-run/1`): the #788 harness with the `buf8k` variant
  (`media-evidence-candidates.py prepare buf8k`) against the 1 MiB release build.
- G6 and G9 ran on `scan`'s stack held by `media-evidence-rollout.py serve`, driven with the
  `voom` CLI as in the [real-media runbook](../../runbooks/operator-real-media-execution.md).
  Their outcomes are summarized below. The databases hold host paths, so they stay private.

Every corpus run held the corpus lock exclusively. Every local cell evicted the selected files
first. Recorded residency was 0 for every file and repetition, except one file at 256 KiB in
`buf8k` repetition 3. Every `scan` repetition re-stated the selected files afterwards and
found size and mtime unchanged.

## Cells

| Record | Cell | Reps | Result |
|---|---|---:|---|
| `…-g3-stage0-off-local-corpus-evicted` | G3 stage-0: full local, off | 3 | Every session **failed** at 179.4–184.4 s: probe result frame 73,144 B > 64 KiB cap ([#816](https://github.com/randomparity/voom-v2/issues/816)); nothing published |
| `…-g1-buf8k-…`, `…-g1-buf1m-…` | G1: harness, full local | 3 + 3 | Hash CPU 685.2 → 153.5 s; summed hash 899.1 → 795.2 s; wall 611.7 → 617.3 s; hash RSS 5.2 → 16.5 MiB |
| `…-canary-off-…` / `…-on-…` | 6-file subset, off / on | 3 + 3 | First durable 5.75 → 1.74 s (baseline CV 20.6%, rerun at 5); complete 102.5 → 25.9 s |
| `…-canary-off-…-5rep` / `…-on-…-5rep` | same, rerun | 5 + 5 | First durable 5.60 → 1.69 s; complete 101.2 → 26.2 s; physical 240.5 → 106.8 GB (1.000× subset); probe `rchar` 0.026% of subset; all 6 reused with the setting on |
| `…-g7-kill-hash-off-…` | subset, off, hash worker SIGKILLed at 3 s | 1 | 6/6 sessions succeeded with the same hash digest as clean runs |
| `…-g7-kill-probe-off-…` | subset, off, probe worker SIGKILLed at 8 s | 1 | The probed file's session failed with nothing published; 5/5 others published |
| `…-g4-construct-btrfs`, `…-tmpfs` | G4 on reflink (evicted) and tmpfs copies | 1 | Unchanged: reused. (a) `pre_probe_guard`, (b) `hash_drift`, (c) `pre_probe_guard`: all `ARTIFACT_CHECKSUM_MISMATCH` |
| `…-g5-construct-ext4-fuse` | G5 on an ext4 image mounted with `fuse2fs` | 1 | Unchanged: recomputed, no hash source. (a) and (c) detected at `pre_probe` by the recomputed hash; (b) `hash_drift` |
| `…-net-off-…` / `…-net-on-…` | network subset, as-found | 3 + 3 | Setting on: 0 reused, 3 recomputed each rep. Reps 2–3 first durable 9.37 → 8.97 s, complete 38.2 → 37.3 s (inconclusive) |

Every stored scan row with recomputed evidence carried no hash source (`other_source` 0
everywhere), and every setting-off cell wrote no reused row, including those run after
setting-on cells.

## CLI-driven gates

- **G6 transform backstop.** Stack with the setting on and one root holding a reflink copy of
  `f0004`. The scan reused the owner hash. A same-size interior edit with the mtime restored
  followed. Then a one-phase policy (`keep audio where channels >= 6`) was applied. The remux
  ticket failed `failed_terminal` with class `ArtifactChecksumMismatch` ("source facts
  disagree", same size, different BLAKE3). No remux file version, no commit intent, and no
  file in staging or output.
- **G9 canary.** Stack with the setting on and six roots holding reflink copies of the
  subset. The flagship `reference-user.voom` and the `language-cleanup.voom` sample block on
  all six files at planning: their `defaults` filters match 0, 3 or 5 kept streams. The
  canary therefore applied a defaults-free normalize phase
  (`container mkv`, `keep audio where language in ["eng","und"]`,
  `keep subtitle where language in ["eng"]`, `order tracks [video, audio, subtitle]`) into a
  separate output root. Results by job:
  - Job 1 completed `f0004`, `f0015` and `f0022`. `f0009` failed on truncated mkvmerge
    identify output ([#819](https://github.com/randomparity/voom-v2/issues/819)). `f0005`
    failed on an attachment in `clear_default_streams`
    ([#820](https://github.com/randomparity/voom-v2/issues/820)).
  - Resume job 2 completed `f0025` and re-failed both.
  - Resume job 3 dispatched only the two failing files. No completed file was re-dispatched.
  - A second scan of all six roots published an identical `(file, content hash)` set. Four
    outputs were verified, and none of the four `artifact_verifications.report` rows carries
    a hash source.
- **G7 mixed versions.** Protocol-5 release binaries were built at `4a1b37fa` and paired
  through the stack's `smoke` mode on a generated fixture:

  | Pairing | Outcome |
  |---|---|
  | protocol-6 agent with a protocol-5 hash or probe worker | refused at agent startup (`unsupported protocol version: offered=6, expected 5`); no lease |
  | protocol-6 control plane, protocol-5 agent and workers | leased, scanned, published |
  | protocol-5 control plane, protocol-6 agent, setting off | leased, scanned, published |
  | protocol-5 control plane, protocol-6 agent, setting on | leased; first batch rejected (`unknown field hash_source`); session failed, nothing published |

  Tracked as [#817](https://github.com/randomparity/voom-v2/issues/817).

## Timings and interference

Corpus time under the lock, local clock: G1 09:10–10:12; stage-0 10:28–10:37 (plus an
abandoned 09:04–09:08 attempt, below); canary and kill cells 10:14–10:25, 10:38–10:41 and
11:53–12:04; G4/G5 10:41–10:56; G6 and G9 10:57–11:50. Network: manifest walk and three
ffprobe header reads at 10:26, the off cell at 10:26–10:28, and the on cell at 11:50–11:53.

The network cells read 74.0 GB (off) and 123.3 GB (on) of host CIFS bytes in about 4.5
minutes in total. The on cell's repetition 1 found the client cache cold and read 74.0 GB, so
that cell exceeded the protocol's 100 GB physical budget per network cell by 23.3 GB. The
logical budget (80 GB per repetition) held. This load competed with the active rehearsal,
and the host counters include rehearsal traffic.

## Disclosures

- The first stage-0 attempt (09:04) was stopped during repetition 2 after repetition 1 failed
  on #816. No record was kept; the committed stage-0 cell is the complete rerun.
- Before stage-0, the instrument was validated with the setting on and off against
  generated two-file fixtures on tmpfs. Those were not corpus cells.
- The first G6 attempt aborted on a driver error before execution. Two G9 attempts stopped
  at planning: copies named alike collided on branch id, and then the flagship `defaults`
  blocked. Each rerun started from fresh copies and a fresh database.
- No ext4 mount was writable without privileges, so G5's ext4 arm ran through `fuse2fs`. The
  kernel reports that filesystem as `fuse`, which is likewise outside the reuse list. #813's
  CI ran the guard tests' recompute arm on a native ext4 test root.
- Outputs from G9 (four remuxed files) and every copy live under an ignored scratch path.
  Nothing was written to either corpus.
