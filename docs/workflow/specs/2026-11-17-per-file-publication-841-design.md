# Per-file publication readiness (#841) — design

## Problem

ADR 0077 publishes scan identity only inside the completion transaction, so on the local
corpus the first durable observation equals the whole pass (670.8–674.6 s in #794's G3
stage-0 cell, `docs/media-evidence-baseline/rollout/`). The #789 and #801 models put the first
ready file near 6 s, but nobody has measured first durable observation with publication moved
earlier, and ADR 0120 rejected per-file readiness for exactly that reason. Issue #841 asks for
that measurement, from a prototype behind a setting, and a written answer on superseding
ADR 0077.

## Scope

Three deliverables, one research PR, no production source change on `main`:

1. **Prototype, spike branch only.** `spike/per-file-publication-841`, pushed and never
   merged, adds one default-off setting in two places:
   - control plane: `voom-api --scan-per-file-publication` sets a `ControlPlane` flag. With
     it on, `accept_new_scan_batch` publishes the batch's evidence-bearing observations with
     ADR 0077's existing `publish_one` logic inside the batch transaction, after the batch rows
     are accepted; completion then skips publication and still runs absence reconciliation
     (`complete_in_tx`) unchanged. With it off, behaviour is byte-for-byte today's.
   - owner node agent: config key `scan_per_file_publication = true` makes the pump's
     `BatchSubmitter` flush after every pushed observation (flush-one). Off keeps the count and
     byte flush rules.
   The setting is read once at process start. Both halves are needed: publication at batch
   acceptance without flush-one still lands everything in one final batch.
2. **Instrument, on `main`.** `scripts/media-evidence-rollout.py scan` gains
   `--publication {completion,per-file}` (default `completion`). The `Stack` writes the agent
   key and appends the API flag only for `per-file`; a binary without the prototype refuses the
   unknown flag or key (clap and `deny_unknown_fields`), so the treatment cannot silently run
   as the control. Scan records move to schema `media-evidence-rollout-scan/2`, adding
   `cell.publication`, per-repetition `batches` (rows in `scan_observation_batches` for the
   cell's sessions) and `build.binaries_git_commit` / `build.binaries_git_changed_paths` read
   from the `--bin-dir` checkout. Schema `/1` records keep validating under `/1` rules. A `/2`
   per-file record is invalid unless every repetition's `batches` equals its observation count.
   `compare` accepts a pair that differs in exactly one setting — reuse off→on or publication
   completion→per-file — with no kill, and refuses anything else.
3. **Records and decision.** Under `docs/media-evidence-baseline/per-file-publication/`: a
   matched stage-0 control (full local corpus, evicted, reuse off, completion, 3 repetitions)
   captured before the treatment (same cell, per-file), both on one spike release build, plus a
   README with the comparison. Then `docs/adr/0132-per-file-scan-publication.md` (Status
   Proposed) or a recorded no-go, its `docs/adr/README.md` row, and an amended per-file row in
   `docs/media-evidence-decision-report.md`. The ADR is written from the measurement, so it is
   reviewed with the branch, not with this design.

**Decision rule.** Go (ADR 0132 proposes superseding ADR 0077's publication clause) only if
the treatment's first durable observation is a protocol improvement (`max(10%, 2×CV)`, baseline
CV ≤ 20%) and its complete time is not a protocol regression (`max(5%, 2×CV)`); otherwise a
no-go is recorded in ADR 0132 and the report row. Either way the ADR or no-go answers absence
reconciliation, ticket and lease authority, and partial-session failure.

### Not in scope

Production per-file publication and its implementation issues (maintainer); scan SLOs and CI
timing (#471); accepting or superseding ADR 0077 beyond a Proposed record (maintainer);
transform-safety and restart claims (not claimed); the network corpus (not run; reported as not
measured). Reuse-on cells are optional context, not gating.

## Failure model

1. **Actors and deployments**
   - the operator on this host, running the instrument against private local manifests;
   - CI running `just media-evidence-rollout-selftest` (no media, no binaries);
   - the spike binaries run only inside instrument stacks on loopback.
2. **Invariants and assets at stake**
   - the local corpus is read-only: no write, rename or metadata change to media;
   - committed records carry manifest ids only, never host paths, hostnames or users;
   - control and treatment differ only in the publication setting (same binaries, corpus,
     cache state, reuse off), else the comparison is void;
   - every existing record under `docs/media-evidence-baseline/` keeps validating;
   - `main` gains no production Rust.
3. **Accepted failure classes**
   - spike defects outside the measured path (setting changed while a session runs, a crash
     between batch publication and completion): the spike never ships; the ADR states the
     production answer for each;
   - one host, one corpus, three repetitions per cell: the protocol's thresholds bound the
     noise and the record says so;
   - polling at 50 ms quantizes first durable observation by up to 50 ms: small against the
     seconds being compared.
4. **Covered elsewhere**
   - scan SLOs and CI timing: #471;
   - the known rollout-selftest race on slow runners: #881;
   - production publication semantics: ADR 0132 review by the maintainer.

## Success

- Both cells exist as validated `/2` records on one spike build whose binary SHA-256s match
  across the pair, the control captured first.
- `compare` produces a verdict for first durable observation and complete; the README states
  both medians, CVs and verdicts, the spike commit SHA, and the batch counts.
- ADR 0132 (Proposed) or a recorded no-go covers the three named topics; the ADR index
  check passes; the decision-report per-file row cites the records.
- `just source-checks` passes with every committed record validating.

## Validation

- Spike, per contract: a control-plane test that a per-file batch publishes a live location
  before completion and that completion then publishes nothing twice; a test that the flag off
  publishes nothing at batch acceptance; a node-agent test that flush-one submits one
  observation per batch. Run with the spike's focused `cargo test` commands and its hooks.
- Instrument: selftests for the `--publication` stack wiring (agent key and API flag present
  only for per-file), the `/2` validator (missing keys, per-file batch mismatch, `/1` records
  still valid), `compare` pairing (publication pair accepted, two-setting and reversed pairs
  refused), and batch counting from a scratch database.
- Real media: the two captured cells themselves.
