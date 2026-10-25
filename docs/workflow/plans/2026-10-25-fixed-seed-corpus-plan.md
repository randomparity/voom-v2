# Fixed-seed corpus implementation plan (#746)

Expected implementation size: 350–600 changed lines, based on a Python corpus helper
(200–350), Rust integration (80–150), runner/selftest/docs (70–100).
Fixed design denominator: 250 changed lines (M), live #746 and existing run/compare seams,
recorded in WORK:SCOPE q746-f7d9be4f. BASE_BRANCH main; branch feat/fixed-seed-corpus-746.

1. Extend narrow corpus generation/run/replay support and production assertions together.
   The tasks share fixture and DB evidence and execute as one Cast unit.
   Verification: focused-test — corpus E2E initially absent/fails; exact command
   `cargo test -p voom-cli --test chaos_librarian_e2e fixed_seed_corpus -- --ignored --nocapture`.
   Verify pinned YAML, fifteen cells, eleven applied events, actual replay and semantic
   production evidence; negative missing/null evidence and changed recipe parameter fail.
2. Report local timeline completeness and document simulation/default-lane limits.
   Verification: focused-test — `just chaos-e2e-local-script-test` rejects a prefix reported
   complete and verifies complete event counts. Documentation: task-test-not-applicable,
   explanatory prose has no executable consumer; behavior is verified above.
3. Validate assembled branch: equipped `just chaos-e2e-ci`, `just lint`, explicit final
   `just ci`, mandatory precommit hooks. No prepush hook installed. Keep CARGO_TARGET_DIR
   at this worktree's target for every build, helper and hook.

Current phase: design. No findings, deferrals or follow-up candidates yet.
Final handoff requires independent branch/security reviews and green mergeable PR exact HEAD.
Operator/campaign retains merge and cleanup authority. No numbered files assigned.
