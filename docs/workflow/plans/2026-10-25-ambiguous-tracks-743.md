# Implementation plan — #743

Expected implementation size: 350–480 changed lines across one localized classifier extension, focused tests, authored fixture support and real integration assertions. Fixed design denominator: 250 changed lines (M), from the cycle-2 frozen charter; this estimate does not resize it. The initial 280–400 estimate omitted required typed DTO boilerplate and case-specific evidence assertions; these remain C2/C3 scope, with no line-count-driven consolidation.

Base: `main`; applicable full local guardrail: `just ci`; final owner: worker explicit run before push. Required precommit hooks remain enabled. No prepush hook. No numbered records or mandatory shared edits. Execution: Cast, two closely coupled tasks under one approved full specification; no implementer delegation needed.

## Task 1 — ASS classifier and walker

Extend `crates/voom-scan-worker/src/discover.rs` only at its subtitle classifier. Add intent tests in `discover_test.rs` and `walk_test.rs` for case-insensitive ASS, existing role, longest-stem language suffix, orphan skip, and unchanged .ssa/.sub/.vtt support boundary. No ownership transition; existing `walk.rs` is reused unchanged, and ADR 0022 remains immutable.

Verification: `focused-test` — classifier and walk cases must fail before implementation because ASS is unclassified; run `cargo test -p voom-scan-worker ass` for red and green. Then run the entire affected crate via `cargo test -p voom-scan-worker` and `just fmt-check`/`just lint` before commit. Cover C1 and existing discovery consumers; preserve hooks.

## Task 2 — Authored matrix through production evidence and supported planning

Add `crates/voom-cli/tests/support/track_cases.py` to author four pinned upstream flag assets plus one independent healthy control with ASS; materialize through existing ChaosLibrarian helper. Add one ignored integration in `chaos_librarian_e2e.rs`, using existing `real_owner_scan.py --existing-fixtures` and Rust supervisor without edits to those infrastructure helpers. Assert C2's exact receipt, snapshot, observation, disposition and sidecar evidence. Create offline durable inputs with the existing control-plane API from queried real snapshot DTOs after owner shutdown (scan import requires an available root), parse normal remux payloads, use existing public selection helpers against stored input snapshot facts, and assert C3's per-case retained IDs, status, strategies, snapshot linkage and deterministic repeat. Do not add product APIs or use fixture-side projections as observed evidence.

Verification: `focused-test` — named `production_scan_track_matrix` integration asserts ASS grouping; before Task 1's fix its real scan must reject that assertion. Green command: `cargo test -p voom-cli --test chaos_librarian_e2e production_scan_track_matrix -- --ignored --exact`. Controlled omission of a required flag or mismatch of expected retained IDs must fail the matrix assertion; restore evidence afterwards. Run `just chaos-e2e-ci` for affected existing authenticated scan/export/encode consumers; `just fmt-check`, `just lint` and final `just ci` cover assembled source. Optional ignored cases outside the heavy suite remain explicitly reported.

Design prose verification: `task-test-not-applicable` — specification/plan have no executable prose consumer; no search/snapshot test. Independent design review and scope audit provide their verification.

## Checkpoints

Preserve cycle-1 review and parked history; cycle-2 authority resolves ASS prerequisite and the current spec corrects first/none evidence boundaries. After each material task, record commands, outcomes and current SHA in the private ledger. Run full-branch adversarial and security reviews against the frozen scope, disposition findings, simplify only recent changes, and hand off a green mergeable PR with exact-head MERGE-READY. No merge authorization.
