# Media evidence rollout gates G1–G9 (#794)

## Problem

ADR 0120 selects a 1 MiB owner hash buffer (#810) and guarded probe reuse of the owner hash on
btrfs and tmpfs behind the owner-node `probe_hash_reuse` setting, default off (#811). Its
latency gains are modelled, not measured. No record measures first durable observation:
the #788 replay harness does not publish. The [decision report](../../media-evidence-decision-report.md)
froze rollout gates G1–G9. The setting may be recommended for enabling only if they pass on
real storage. A failed gate keeps the setting off and publishes the no-go evidence.

## Design

### Instruments

- **G3 publishing instrument** (`scripts/media-evidence-rollout.py scan`, new). It runs
  #339's local acceptance stack (`scripts/acceptance-bootstrap.py`) unchanged, with one
  addition: it writes `probe_hash_reuse` explicitly into the agent configuration. Each
  repetition uses a fresh stack and database. It evicts the selected files when local, records
  `fincore` residency, and requests one `voom scan --root` session per root. It then polls the
  database read-only every 50 ms. *First durable observation* is the first live file
  location in the scanned roots. Under ADR 0077, that is the commit of the completion
  transaction. *Complete* is every session terminal. A full pass scans the corpus root.
  A subset scans each selected file's own directory and refuses a directory holding unselected
  files. The instrument records deltas of the supervised hash and probe workers'
  `/proc` CPU, `rchar` and `read_bytes`, plus their memory high-water marks. A worker restart
  makes them `unavailable`. It also records host counters, session states, and stored evidence
  counts by hash source (reused, absent, other). The published `(manifest id, content hash)`
  pairs are recorded as one SHA-256 digest, so repeat scans compare without committing hashes.
  A final check confirms each selected file's size and mtime are unchanged. `--kill` SIGKILLs
  a worker mid-scan (O14).
- **Construction driver** (`… construct`, new). It drives the release hash and probe
  workers over their protocol on a private copy of one media file. The copy is a reflink on
  btrfs and a byte copy elsewhere. Four cases run: unchanged; (a) same-size edit with the
  mtime restored between hash and probe; (b) the same edit during the hash read; (c) an
  mtime-moving write. Only `ARTIFACT_CHECKSUM_MISMATCH` counts as detected. A case (b) edit
  that lands after the read refuses the run.
- **G1** uses the existing harness and `media-evidence-candidates.py` (`baseline` against
  `buf8k`, `compare`).

### Gate procedures (order: stage-0 cell, canary, full local, network)

| Gate | Cell or case | Judged by |
|---|---|---|
| G3 stage-0 | scan, full local, evicted, setting off, ×3 | captured before any setting-on cell |
| canary | scan of the 6-file local subset off and on; G4 and G6 on copies; G7 kill; G9 canary policy | correctness only |
| G1 | harness `buf8k` vs `baseline`, full local, evicted, ×3 | hash CPU improves; summed hash and wall do not regress; hash RSS ≤ +256 MiB |
| G2, G3 | scan, full local, evicted, setting on, ×3 | physical ≤ 1.05× corpus; probe `rchar` < 1%; wall and first durable observation improve over stage 0 |
| G4 | construct on btrfs (reflink copy) and tmpfs | 3 cases each detected |
| G5 | construct on an ext4 image; scan with setting on over the network subset (≤ 3 files) | no reused row or result; case (a) detected |
| G6 | stack on a reflink-copy root, setting on: scan, restored-mtime edit, apply a remux policy | transform input `ARTIFACT_CHECKSUM_MISMATCH`, no committed output |
| G7 | setting-off scan after setting-on; mixed protocol 5/6 binaries; `--kill hash` / `probe` | no reused rows; refusal before lease execution; nothing partial published |
| G8 | stored evidence and `artifact_verifications.report` after G6/G9 | no hash source on recomputed or output rows |
| G9 | canary then complete-library separate-output application and repeat; repeat scan | identical hash digest; repeat dispatches no transform |

Verdicts use the protocol's thresholds. #813's automated tests are cited, not repeated. These
are G4(a)–(c) and G5 on tmpfs and the test root, G7's guardless-`reused` refusal and protocol
refusal, and G8's encodings. Measurements run under the corpus lock (`flock -x`) and builds
under `flock -s`. Both corpora are read-only. Constructions use copies only.

### Records and docs

Records go to `docs/media-evidence-baseline/rollout/` (schemas `media-evidence-rollout-scan/1`
and `…-construct/1`; G1 uses `media-evidence-baseline-run/1`). They hold manifest ids and
labels only. The README there lists cells, timings, cache state and interference. The
before/after report is `docs/media-evidence-rollout-report.md`, with a verdict per gate and
the recommendation. The node-agent runbook gains the enable criteria and the rollback.

## Failure model

- The idle deadline is fixed at 600 s and extended only by an accepted batch. A scan over
  fewer than 1,000 files that runs longer could end `stale` and publish nothing. That
  is a recorded result, not a reason to change the gate.
- A host-wide counter on the shared host includes unrelated traffic. Worker counters are
  per-process, and ffprobe's own reads are not attributed (protocol).
- A stack whose worker restarts mid-scan reports `unavailable` usage, never a partial number.
- Network cells are `as-found` and disclose rehearsal interference.

## Success

Every gate has a verdict (pass, fail or inconclusive) with its numbers and records. The
instrument selftest is in `just source-checks`, and `just ci` is green. The report states the
setting recommendation. Nothing private is committed.

## Validation

`just media-evidence-rollout-selftest`, `just ci`, a host-path scan of every committed record,
and corpus manifests unchanged before and after.
