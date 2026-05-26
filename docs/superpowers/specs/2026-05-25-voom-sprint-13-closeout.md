---
name: voom-sprint-13-closeout
description: Sprint 13 closeout evidence for policy-driven MKV remux, track selection, worker execution, staged verification, commit, result snapshot, and reporting.
---

# VOOM Sprint 13 Closeout

| Requirement | Evidence |
|---|---|
| DSL accepts supported container and track policy shapes | `cargo test -p voom-policy`; policy text in `crates/voom-plan/src/fixtures_test.rs::REMUX_TRACK_SELECTION_POLICY`. |
| Planner groups same-phase remux operations | `cargo test -p voom-plan groups_container_and_track_operations_into_one_remux_node`; golden fixture `crates/voom-plan/fixtures/plans/remux_track_selection.json`. |
| Unsupported or missing facts block visibly | `cargo test -p voom-plan defaults_best_blocks_instead_of_joining_executable_group`; remux fact blockers in `crates/voom-plan/src/planner_test.rs`. |
| Worker preflight fails loudly | `cargo test -p voom-mkvtoolnix-worker preflight`; missing/non-executable/version cases in `crates/voom-mkvtoolnix-worker/src/preflight_test.rs`. |
| Worker writes staged MKV only | `cargo test -p voom-mkvtoolnix-worker handler`; real identify parser coverage in `crates/voom-mkvtoolnix-worker/src/mkvmerge_test.rs::reads_real_mkvmerge_container_type_string`. |
| Control plane verifies, commits, and records result snapshot | `cargo test -p voom-control-plane --test remux_flow`; end-to-end fixture in `crates/voom-control-plane/tests/remux_flow.rs`. |
| Compliance report fixture records remux execution eligibility | `cargo test -p voom-plan remux_track_selection`; golden report `crates/voom-plan/fixtures/reports/remux_track_selection.json`. |
| CLI emits stable JSON envelopes | `cargo test -p voom-cli compliance_envelope`; compliance snapshots under `crates/voom-cli/tests/snapshots/`. |
| Full suite passes | `just ci`. |
