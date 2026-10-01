# Plan: compile-only default-feature control-plane wiring check (#636)

Goal: replace the duplicate default-feature control-plane test execution in `just test` with
a compile-only check that still catches the #359 class.
Architecture: one recipe line and its comment in `justfile`; one `AGENTS.md` table row.
Spec: `docs/workflow/specs/2026-10-01-control-plane-wiring-check-design.md`.
Tech stack: just 1.x, cargo 1.95.0.

Expected implementation size: 6–10 changed lines (S) — two justfile lines, one AGENTS.md row.

## Global Constraints

- Leave unchanged: `cargo build --workspace --all-features --all-targets`, the
  `VOOM_TEST_PREBUILT_WORKERS=1 cargo test --workspace --all-features` line, the `setup`
  recipe, and `crates/voom-control-plane/Cargo.toml` (approved exclusions; sibling #648 owns
  `setup`).
- Conventional commits, ≤72-char subject; run gates bare.

## File map

- `justfile` `test` recipe: replace the default-feature line and its comment.
- `AGENTS.md` Commands table: the `just test` row describes the three steps.

## Task 1: replace the guard and prove it bites

Verification:
- Contract: the #359 class fails `just test`. Mode: focused-test. Fault: in
  `crates/voom-control-plane/Cargo.toml` delete the `required-features = ["test"]` line under
  `name = "commit_use_lease_gate"`. Red: `cargo check -p voom-control-plane --tests` exits 101
  with `E0599 ... use_leases`. Green after `git restore crates/voom-control-plane/Cargo.toml`:
  same command exits 0.
- Contract: the AGENTS.md row. Mode: task-test-not-applicable — prose table row with no
  executable consumer.

Steps:
1. In `justfile`, replace

   ```just
       # Guard test-target wiring without the workspace's --all-features override.
       VOOM_TEST_PREBUILT_WORKERS=1 cargo test -p voom-control-plane
   ```

   with

   ```just
       # Guard test-target wiring without the workspace's --all-features override.
       # Compile-only: the all-features run below executes these same tests.
       cargo check -p voom-control-plane --tests
   ```

2. In `AGENTS.md`, replace the row
   `| \`just test\` | \`cargo test --workspace --all-features\`. |` with
   `| \`just test\` | Prebuild all targets, compile-check default-feature \`voom-control-plane\` tests, then \`cargo test --workspace --all-features\`. |`.
3. Apply the fault above; run `just test`; expect failure at the check step with `E0599`.
   Revert with `git restore crates/voom-control-plane/Cargo.toml`; `git status --short` shows
   only `justfile` and `AGENTS.md`.
4. Timing: run `just test` once on the base recipe and once on the new recipe on warm targets
   (`/usr/bin/time -f %e`), record both and the per-step costs from the spec evidence for the
   PR body.
5. Run `just ci`; expect exit 0. Commit `test: compile-check control-plane default-feature
   wiring`.

Rollback: revert the commit; no persisted state.
