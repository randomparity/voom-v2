---
name: voom-sprint-16-closeout
description: Sprint 16 closeout evidence for coherent multi-phase real-media policy execution — phase-barrier coordinator, append-only artifact chaining, phase-boundary re-probe, bounded per-phase replanning, durable two-grain workflow summary, partial-barrier failure + resume, and the scan/plan/execute/report CLI surface — verified end-to-end.
status: draft
date: 2026-06-05
sprint: 16
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md
  - docs/superpowers/plans/2026-06-05-voom-sprint-16-closeout.md
  - docs/adr/0005-plan-phase-entry-point.md
  - docs/adr/0006-workflow-summary-schema.md
  - docs/adr/0007-phase-barrier-coordinator.md
  - docs/adr/0008-per-phase-report-regenerated-against-refreshed-facts.md
  - docs/adr/0009-resume-opens-new-job-reconciles-prior-rows.md
  - docs/adr/0010-compliance-report-job-read-mode.md
---

# VOOM Sprint 16 Closeout

> **Status note:** this document is committed in `draft` during the design phase
> (issue #167 step 3). The *Observed result* column is filled from real test
> runs in the implementation phase (plan Task 4, step 2) and the status flips to
> `complete` only when every cited command has been run and `just ci` is green.
> Cells not yet filled read `pending`.

Sprint 16 makes multi-phase real-media policy execution coherent from CLI scan
through report: a control-plane coordinator drives the existing executor one
phase at a time across the whole input set (phases are barriers across files),
chains each phase's committed artifact into the next phase's planning, re-probes
the staged result at each phase boundary, bounds replanning to the declared
phase count, folds each phase's regenerated compliance report into a durable
two-grain workflow summary, and exposes the whole flow through the `compliance`
command family. The matrix below maps each Section 10 acceptance criterion to
the test(s) and command that prove it.

## Testing strategy: the determinism split

The "CLI golden-output" requirement (§9) is satisfied along the determinism
boundary the existing suite already established. Deterministic stages —
`plan dry-run`, the `compliance report` *preview* (run against the declared
input snapshot, before any mutation), and `scan` — produce stable JSON and are
locked with `insta` goldens. The real heterogeneous `compliance execute` run
launches real `ffmpeg`, `mkvmerge`, and `ffprobe`, whose output embeds run- and
version-varying `bitrate`/`duration` that feed the content-addressed
`report_hash`/`plan_hash`/`check_id`; it is therefore verified by **field
assertions over the durable summary**, not an `insta` golden. This mirrors the
documented reasoning in `crates/voom-cli/tests/multi_phase_flow.rs` and
`crates/voom-control-plane/tests/phase_barrier_flow.rs`. The fake-`ffprobe`
golden trick used for the single-phase remux envelope
(`compliance_envelope.rs`) does not extend to transcode, because a fake probe
cannot verify a transcode-to-hevc commit. No new architectural decision is
introduced; the architecture was settled in ADRs 0005–0010 via #160–#166.

## Acceptance Matrix

| Acceptance criterion (spec §10) | Command | Observed result |
|---|---|---|
| A multi-phase policy combining video transcode, remux/track-selection, audio mutation, verification, and commit executes and is inspectable through CLI JSON envelopes | `cargo test -p voom-control-plane --test phase_barrier_combined_flow` and `cargo test -p voom-cli --test multi_phase_flow` | pending |
| Each phase plans and executes against the artifact the prior phase produced and re-probed | `cargo test -p voom-control-plane --test phase_barrier_combined_flow` and `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_chains_committed_artifact_into_the_next_phase` | pending |
| Replanning is bounded by the declared phase count; no phase is added at runtime; an unplannable phase becomes an inspectable blocked issue | `cargo test -p voom-control-plane coordinator` (`run_phase_barrier_drops_unplannable_file_as_blocked`) + `--test phase_barrier_combined_flow` (a 3-phase policy yields exactly 3 phase rows) + `cargo test -p voom-plan` (planner blocked-reason cases) | pending |
| The compliance report reflects produced artifacts per phase with lineage | `cargo test -p voom-control-plane --test phase_barrier_flow assert_reprobe_and_lineage_chain` (via the chain test) and `--test phase_barrier_combined_flow` | pending |
| A durable workflow summary ties every phase to its tickets, artifacts, re-probe snapshots, and compliance report | `cargo test -p voom-store workflow_summaries` and `cargo test -p voom-control-plane --test phase_barrier_combined_flow` (durable re-read of all three grains) | pending |
| A partially-applied policy leaves a coherent, inspectable state (committed files recorded, no orphan/delete); job-failure-mid-barrier resume re-enters only the failed file | `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_records_committed_sibling_when_a_file_fails phase_barrier_resumes_failed_file_without_remutating_committed_sibling` | pending |
| CLI golden-output for the deterministic preview path (`plan` dry-run + `compliance report` preview) | `cargo test -p voom-cli --test multi_phase_preview_envelope` | pending |
| `compliance execute` → `compliance report --job-id` reads the durable multi-phase chain back | `cargo test -p voom-cli --test multi_phase_flow multi_phase_execute_then_report_by_job_id` | pending |
| `just ci` passes | `just ci` | pending |

## §9 testing-bullet coverage

| §9 test bullet | Proven by |
|---|---|
| End-to-end workflow integration test (transcode + remux + audio + verify + commit) | new `phase_barrier_combined_flow` |
| Artifact-chain (phase N+1 against phase N's `FileVersion`, correct `source_lineage`) | `phase_barrier_flow::assert_reprobe_and_lineage_chain` + `phase_barrier_combined_flow` |
| Re-probe (refreshed snapshot keyed to produced version, fed forward) | `phase_barrier_flow` (`snapshots_for_version`) + `phase_barrier_combined_flow` |
| Bounded-replan (one pass per phase, no phase beyond `phase_order`, `run_if`/`skip_if` re-eval, blocked unplannable phase) | coordinator `run_phase_barrier_drops_unplannable_file_as_blocked` + `run_if`/`skip_if` coordinator/planner tests; phase count pinned by `phase_barrier_combined_flow` (3) and `phase_barrier_chains_committed_artifact_into_the_next_phase` (2) |
| Partial-barrier-failure + resume | `phase_barrier_flow` failure + resume tests |
| `on_error` handled per the stated rule (cannot silently regress) | `voom-control-plane` coordinator tests `reject_unhandled_on_error_rejects_continue`, `…rejects_skip`, `…allows_abort_and_unset`, and `resume_phase_barrier_rejects_unhandled_on_error_before_opening_job` (non-default `on_error` is rejected at resolve time, before a job opens) |
| Compliance-report per-phase regeneration, deterministic identity | `phase_barrier_flow` report-id assertions + `compliance_envelope` goldens |
| Durable-summary schema + repo round-trip; half-committed barrier yields rows only for advanced files | `voom-store workflow_summaries` + `phase_barrier_flow` partial test |
| CLI golden-output (`insta`) for scan → plan → execute → report | `multi_phase_preview_envelope` (goldens) + `multi_phase_flow` (real execute, field assertions) |
| Documentation completeness scan | `rg` placeholder scan (record command + result at fill time) |

## Deferred Work

Per spec §11, Sprint 16 defers: phase re-entry, adaptive re-encode loops, and
fixpoint replanning; rollback / active-version reset after a partially-applied
policy; per-file failure isolation and independent per-file phase cursors;
non-default `CompiledPhase.on_error` strategies (continue-on-error, etc.); backup
worker, sidecar ingest, and bundle/sidecar CLI views (Sprint 17); daemon loops,
watcher, scheduler, and recovery (Sprints 18–20); web UI, plugin SDK, production
packaging; and multi-output audio extraction (#99). This closeout asserts only the
§10 acceptance set.
