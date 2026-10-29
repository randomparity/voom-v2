# Media evidence protocol and baseline (#788)

## Problem

Epic #787 compares identity-evidence strategies, but no frozen protocol, harness, or
baseline exists. Candidates measured first would let results choose their own thresholds.

## Scope

- `docs/media-evidence-protocol.md`: metrics, useful-work milestones, phase inventory,
  corpus, mutation oracle (definition only), staged candidate matrix, thresholds and
  run/repetition budgets frozen before any candidate result, and the #471 boundary.
- `scripts/media-evidence-baseline.py`: `manifest` lists a corpus root read-only; `run`
  replays the current per-candidate scan sequence (HashFile, then ProbeFile with the hash
  facts as expected facts) against the built hash and ffprobe workers over their HTTP
  protocol, at concurrency 4, for a manifest subset. It records build identity, topology,
  per-file page-cache residency (`fincore`), source-read constants, per-file wall/queue
  delay/outcome, worker CPU, peak memory and `/proc` I/O, plus host network counters.
- `scripts/media-evidence-baseline-selftest.py` and recipe `media-evidence-baseline-selftest`
  in `source-checks`.
- `docs/media-evidence-baseline/`: committed JSON runs and a results summary.

No crate changes; no ADR (experiment tooling, not architecture). Replay excludes discovery,
publication, planning and transformation; the baseline marks those milestones unavailable.

### Failure model

1. Actors and deployments: one operator on a Linux host with read access to a local
   corpus and a network-filesystem corpus shared with a running rehearsal.
2. Invariants and assets: original media is never written, renamed, or re-timestamped; no
   host path, server, share, or user name enters committed artifacts; recorded numbers are
   raw, not fabricated or silently dropped.
3. Accepted: replay omits control-plane publication and scheduling (named unavailable);
   host-wide network counters include rehearsal traffic (disclosed); atime updates from
   reads (production scans do the same); no cold-cache claim (no root to drop caches).
4. Covered elsewhere: candidate runs #789/#790; decision #791; SLO/CI gate #471.

## Success

1. The protocol fixes every number a candidate comparison uses before candidates run.
2. `run` output carries build identity, topology, cache state, concurrency, buffer sizes,
   raw per-file measurements, failures, and logical versus physical bytes.
3. Committed baselines cover a local subset and a bounded network subset with at least
   three repetitions each, plus variance.
4. Recorded corpus identity is a label and per-file path digest only.

## Validation

- Manifest redaction and subset selection — focused-test: selftest asserts no fixture root
  or file name appears in the manifest; red before redaction exists.
- Replay record shape — focused-test: selftest runs `run` against a stub worker on a
  temporary fixture and asserts required fields, an injected failure row, and unchanged
  fixture mtimes.
- Source-constant capture — focused-test: selftest fails when a constant is not found.
- Protocol and results prose — task-test-not-applicable: human-readable decision record
  with no executable consumer.
- Baseline JSON — task-test-not-applicable: measurements of one host; validity comes from
  the reviewed harness, not a test.
