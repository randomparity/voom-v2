# Compile-only default-feature control-plane wiring check (#636)

## Problem

`just test` runs `VOOM_TEST_PREBUILT_WORKERS=1 cargo test -p voom-control-plane` between the
worker prebuild and the all-features workspace run. The step exists only to catch the #359
regression class: an integration target of `voom-control-plane` that calls API gated by
`#[cfg(any(test, feature = "test"))]` without declaring `required-features = ["test"]`. The
workspace `--all-features` run cannot see that class, because it turns the feature on.

The step also executes the whole control-plane suite a second time. Every `feature = "test"`
gate in `crates/voom-control-plane/src` is `any(test, feature = "test")` on an additive accessor
item (`lib.rs`, eleven `pub fn` accessors), and `voom-core/test` and `voom-store/test` are on in
both modes through dev-dependencies. So the second run executes the same control-plane code,
minus the two `required-features` targets, against a smaller unified dependency feature set
(`cargo tree -e features -i tokio`: the `-p` graph lacks tokio's `full`, `signal` and
`parking_lot`, which other workspace members enable). No shipped binary is built from that graph.

## Design

Replace the step with a compile-only check:

```just
    cargo check -p voom-control-plane --tests
```

The package has one `lib` and twelve `test` targets (`cargo metadata`), and no bins, examples or
benches. `--tests` selects all thirteen in test mode, as `cargo test` does, and applies the same
`required-features` filtering. It has no doctests (`cargo test -p voom-control-plane --doc` runs 0 tests at `4fbed721`), so nothing
the old step compiled is lost. The worker prebuild line, `VOOM_TEST_PREBUILT_WORKERS=1`, and
the all-features workspace line stay byte-identical. The recipe comment states that the check
is compile-only and why. The `just test` row in `AGENTS.md` is updated to name the three steps.

### Evidence (Linux x86_64, 48 CPUs, cargo 1.95.0, base `4fbed721`, warm all-features prebuild)

- Feature unification: `cargo tree -p voom-control-plane -e features -i voom-control-plane`
  shows only the `default` feature requested; no dev-dependency enables `test`.
- Controlled fault (remove `required-features` from `commit_use_lease_gate`):
  `cargo check -p voom-control-plane --tests` exits 101 with five `E0599 use_leases`
  errors; `cargo test -p voom-control-plane --no-run` fails identically;
  `cargo check --workspace --all-features --tests` exits 0, confirming the gap the guard fills.
- Cost after the prebuild: old step 127 s compile plus 13–17 s execution; `--no-run` 127 s;
  `cargo check --tests` 42 s cold, 0.2 s warm.

### Considered & rejected

- **`cargo test -p voom-control-plane --no-run`.** verified: same fault detection as the
  check, but 127 s vs 42 s on the evidence host above, because it codegens a second
  default-feature dependency graph.
- **Delete the step.** verified: with the fault applied, `cargo check --workspace
  --all-features --tests` exits 0, so nothing else detects the #359 class.
- **Move it to a separate CI job.** judgment: more CI surface for a 42 s step; `just test`
  is the owner the issue names.

## Failure model

1. Actors and deployments: a developer running `just test`/`just ci` locally; the CI
   `platform-checks` jobs (Ubuntu, macOS) and `test-constrained` matrix, which call `just test`.
2. Invariants at stake: the #359 class must keep failing `just test`; the all-features
   workspace run, worker prebuild and prebuilt-worker env must stay unchanged (approved
   exclusions).
3. Accepted failure classes:
   - Codegen-, link- or runtime-only failures that appear only under the `-p` graph's smaller
     dependency feature set — the guard's job is the #359 compile/wiring class, which
     `cargo check` still type-checks under that exact feature set, and no shipped binary is
     built from that graph.
   - Default-feature compile regressions in crates other than `voom-control-plane` — the old
     step did not cover them either.
4. Covered elsewhere: behavioral execution of every control-plane test — the unchanged
   `cargo test --workspace --all-features` line in the same recipe.

## Success

- With the fault applied, `just test` fails at the check step; with it reverted, it passes.
- The recipe no longer runs the control-plane suite twice; the other two lines are unchanged.
- Before/after local durations for the guard step and for `just test` are reported in the PR
  body; macOS CI durations from the PR run are reported with runner/cache caveats.
- `just ci` exits 0.
