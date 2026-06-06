---
name: voom-sprint-16-closeout
description: Sprint 16 closeout evidence for coherent multi-phase real-media policy execution — phase-barrier coordinator, append-only artifact chaining, phase-boundary re-probe, bounded per-phase replanning, durable two-grain workflow summary, partial-barrier failure + resume, and the scan/plan/execute/report CLI surface — verified end-to-end.
status: complete
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
  - docs/adr/0011-audio-transcode-plannability-vs-preservation.md
---

# VOOM Sprint 16 Closeout

> **Status:** the *Observed result* column is filled from real test runs. Beyond
> the §10 acceptance tests, closing this issue required one production fix — the
> ffprobe-worker normalization now carries the per-stream audio facts the audio
> planner needs (see "Re-probe fidelity fix" below), without which a multi-phase
> policy's audio phase could not commit off a re-probed artifact.

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
boundary the existing suite already established. The **`compliance report`
preview** of the combined multi-phase policy — run against a declared, fixed
input snapshot before any mutation — produces stable JSON and is locked with an
`insta` golden (`crates/voom-cli/tests/multi_phase_preview_envelope.rs`). The
`scan` and `plan dry-run` envelope shapes are already goldened for representative
policies (`scan_envelope.rs`, `plan_envelope.rs`); a combined-policy `plan
dry-run` is not goldened because the `transcode video to hevc` operation resolves
the named `default-hevc` profile, which requires an initialized store and so is
unavailable to the offline source-only dry-run.

The real heterogeneous `compliance execute` run launches real `ffmpeg`,
`mkvmerge`, and `ffprobe`, whose output embeds run- and version-varying
`bitrate`/`duration` that feed the content-addressed
`report_hash`/`plan_hash`/`check_id`; it is therefore verified by **field
assertions over the durable summary**, not an `insta` golden. This mirrors the
documented reasoning in `crates/voom-cli/tests/multi_phase_flow.rs` and
`crates/voom-control-plane/tests/phase_barrier_flow.rs`. No new architectural
decision is introduced; the architecture was settled in ADRs 0005–0010 via
#160–#166.

## Re-probe fidelity fix

The combined end-to-end test surfaced a real gap that no prior test could: an
audio-transcode phase consuming a coordinator **re-probe** snapshot blocked with
`snapshot stream facts are insufficient for audio planning`. The audio planner
requires, per audio stream, `language` + `title` + `channels` + `commentary`
(`crates/voom-plan/src/planner/audio/selection.rs` `has_transcode_preservation_facts`).
The ffprobe-worker normalization (`crates/voom-ffprobe-worker/src/normalize.rs`)
lifted `language` and `channels` but never extracted `tags.title`, and normalized
only the `default`/`forced` disposition flags — never ffprobe's `comment` flag.
So **every** real probe — the initial scan and every phase-boundary re-probe
alike — produced an audio-insufficient snapshot; the single-phase
`audio_transcode_flow.rs` only passed because the test hand-augments the snapshot,
a step the coordinator's internal re-probe path has no equivalent for.

The fix extracts `tags.title → title` and renames `disposition.comment →
commentary` during normalization, so real probes carry the facts the planner
needs. With it, the combined `remux → transcode → audio` chain commits all three
phases. This is the one production change in an otherwise tests-only closeout; it
is covered by a `normalize.rs` unit test and proven end-to-end by
`phase_barrier_combined_flow`.

**Residual strictness (not addressed here).** This fix surfaces `title` and
`commentary` *when the source carries them*; it does not change the planner's gate
itself. `has_transcode_preservation_facts` still requires every selected audio
stream to have a `title` and a `commentary` disposition, so an audio-transcode
phase will block on real media whose audio streams have no `title` tag (common —
muxers do not synthesize one). Whether `title`/`commentary` should gate *transcode*
planning at all is a pre-existing planner question (locked by
`voom-plan` `selection_test::transcode_preservation_facts_require_language_title_channels_and_commentary`),
out of scope for this tests-only closeout and tracked as follow-up. The combined
test's fixture carries audio titles precisely so the chain commits.

> **Resolved by #184 / ADR-0011.** The follow-up landed: investigating the gate
> showed *no* per-stream fact reaches the transcode worker (the request carries
> only stream references), so the genuine plannability floor is the source codec +
> container that `transcode_audio_shape` already enforces. The
> `has_transcode_preservation_facts` gate is removed entirely; `language`, `title`,
> `channels`, and `commentary` are pure preservation passthrough, so title-less
> media plans and commits. The combined-flow fixture no longer bakes audio titles.
> See `docs/adr/0011-audio-transcode-plannability-vs-preservation.md`.

## Acceptance Matrix

| Acceptance criterion (spec §10) | Command | Observed result |
|---|---|---|
| A multi-phase policy combining video transcode, remux/track-selection, audio mutation, verification, and commit executes and is inspectable through CLI JSON envelopes | `cargo test -p voom-control-plane --test phase_barrier_combined_flow` and `cargo test -p voom-cli --test multi_phase_flow` | passed: the combined `remux → transcode → audio` policy commits all three phases (three `Completed` phase rows, three `Committed` per-file rows); `multi_phase_flow` drives `compliance execute` → `report` through the CLI |
| Each phase plans and executes against the artifact the prior phase produced and re-probed | `cargo test -p voom-control-plane --test phase_barrier_combined_flow` and `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_chains_committed_artifact_into_the_next_phase` | passed: combined flow asserts the append-only `scan → v0 → v1 → v2` `produced_from` chain and one re-probe snapshot per produced version; the chain test pins the single-file case |
| Replanning is bounded by the declared phase count; no phase is added at runtime; an unplannable phase becomes an inspectable blocked issue | `cargo test -p voom-control-plane coordinator` (`run_phase_barrier_drops_unplannable_file_as_blocked`) + `--test phase_barrier_combined_flow` (a 3-phase policy yields exactly 3 phase rows) + `cargo test -p voom-plan` (planner blocked-reason cases) | passed: a 3-phase policy yields exactly three phase rows; `run_phase_barrier_drops_unplannable_file_as_blocked` makes an unplannable phase an inspectable blocked row; `voom-plan` blocked-reason cases green |
| The compliance report reflects produced artifacts per phase with lineage | `cargo test -p voom-control-plane --test phase_barrier_flow assert_reprobe_and_lineage_chain` (via the chain test) and `--test phase_barrier_combined_flow` | passed: each phase's recorded report targets that phase's produced version and observes its committed facts |
| A durable workflow summary ties every phase to its tickets, artifacts, re-probe snapshots, and compliance report | `cargo test -p voom-store workflow_summaries` and `cargo test -p voom-control-plane --test phase_barrier_combined_flow` (durable re-read of all three grains) | passed: a fresh repo re-read returns three `Completed` phase rows (each `report_id` matching the embedded report identity) and three `Committed` per-file rows recording the produced versions |
| A partially-applied policy leaves a coherent, inspectable state (committed files recorded, no orphan/delete); job-failure-mid-barrier resume re-enters only the failed file | `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_records_committed_sibling_when_a_file_fails phase_barrier_resumes_failed_file_without_remutating_committed_sibling` | passed: the committed sibling is recorded durably on failure; resume opens a new job, re-enters only the failed file, and does not re-mutate the committed one |
| CLI golden-output for the deterministic preview path (`compliance report` preview) | `cargo test -p voom-cli --test multi_phase_preview_envelope` | passed: the golden locks the combined `compliance report` envelope — all three operation kinds previewed against fixed facts, content-addressed identity stable |
| `compliance execute` → `compliance report --job-id` reads the durable multi-phase chain back | `cargo test -p voom-cli --test multi_phase_flow multi_phase_execute_then_report_by_job_id` | passed: the post-run `report --job-id` returns the same phase chain with `latest_phase_index` at the highest ordinal and matching folded report ids |
| `just ci` passes | `just ci` | passed: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, `audit` all green ("All CI checks passed") |

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
| CLI golden-output (`insta`) for scan → plan → execute → report | `multi_phase_preview_envelope` (multi-phase `compliance report` golden) + existing `scan_envelope`/`plan_envelope` goldens + `multi_phase_flow` (real execute → report, field assertions) |
| Documentation completeness scan | `rg -n "insufficient for audio|re-probe.*can.?not|unfiltered" docs/superpowers/specs/2026-06-05-voom-sprint-16-closeout.md` confirms the closeout describes the re-probe gap only as fixed (the "Re-probe fidelity fix" section), with no doc asserting it as a standing limitation |

## Deferred Work

Per spec §11, Sprint 16 defers: phase re-entry, adaptive re-encode loops, and
fixpoint replanning; rollback / active-version reset after a partially-applied
policy; per-file failure isolation and independent per-file phase cursors;
non-default `CompiledPhase.on_error` strategies (continue-on-error, etc.); backup
worker, sidecar ingest, and bundle/sidecar CLI views (Sprint 17); daemon loops,
watcher, scheduler, and recovery (Sprints 18–20); web UI, plugin SDK, production
packaging; and multi-output audio extraction (#99). This closeout asserts only the
§10 acceptance set.
