# Media evidence protocol and baseline (#788)

## Problem

Epic #787 compares identity-evidence strategies without a frozen protocol, harness, or
baseline; candidates measured first would choose their own thresholds.

## Scope

- `docs/media-evidence-protocol.md`: metrics, milestones, phases, corpus, mutation oracle
  (definition only), staged candidate matrix, frozen thresholds and budgets, #471 boundary.
- `scripts/media-evidence-baseline.py`: `manifest` lists video files under a root read-only
  into a private local file; committed output names files only by ordinal id, size and
  extension. `run` replays primaries only (HashFile, then ProbeFile with the hash facts as
  expected) against the built hash and ffprobe workers over HTTP at concurrency 4, within a
  byte budget. It records build identity, topology, source-read constants, page-cache
  residency per file before each repetition, per-file wall, queue delay and outcome, worker
  CPU including reaped children, worker peak memory and `/proc` I/O, and host network
  counters. Unknown frames are failure rows. `--evict` drops page cache on local storage
  only (`posix_fadvise`) and verifies residency; it refuses network filesystems.
- Self-test `scripts/media-evidence-baseline-selftest.py`, recipe in `source-checks`.
- `docs/media-evidence-baseline/`: committed runs and summary.

Not measured, recorded unavailable: discovery, publication, planning, transformation,
sidecar hashing, ffprobe child memory and I/O. No crate changes; no ADR.

### Failure model

1. Actors: one operator capturing on a Linux host with a local corpus and a
   network corpus shared with a running rehearsal; the self-test on Linux and macOS CI.
2. Invariants: originals are never written, renamed, or re-timestamped; committed files hold
   no host path, server, share, user, or file name; numbers are raw and none dropped.
3. Accepted: host network counters include rehearsal traffic and the harness load affects
   the rehearsal (bounded, disclosed); reads update atime as production scans do; a metric
   absent on a host is recorded `unavailable`, never zero.
4. Elsewhere: candidate runs #789/#790; decision #791; SLO/CI gate #471.

## Success

1. The protocol's thresholds table fixes per-metric thresholds, repetitions, and per-run
   time and byte budgets before candidates run.
2. `run` output carries the Scope fields, failures, and logical versus physical bytes.
3. Committed baselines: a local subset and a budget-bounded network subset, three
   repetitions each, variance per residency class with repetition 1 separate.
4. Committed files identify corpora by label and files by ordinal id.

## Validation

- Redaction and subset — focused-test: committed and fixture output contain no
  fixture root or file name; red before redaction.
- Replay record — focused-test: stub-worker run on a fixture yields required fields, an
  injected failure row, an unknown-frame failure, unchanged fixture mtimes, and per field a
  value or `unavailable`.
- Budget and network refusal — focused-test: the budget stops scheduling; `--evict` refuses
  a non-local filesystem.
- Source constants — focused-test: a missing constant fails.
- Committed baselines — focused-test: the self-test applies record and redaction
  assertions to every committed run.
- Protocol and summary prose — task-test-not-applicable: no executable consumer.
