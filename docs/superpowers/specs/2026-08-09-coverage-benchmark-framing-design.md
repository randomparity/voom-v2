# Coverage Benchmark Framing Design

## Scope authority

- **Interaction:** unattended
- **Scope identity:** <https://github.com/randomparity/voom-v2/issues/463> plus
  `c1bd7e0d-b339-4c6f-b9a0-dc9af4962136`
- **Outcome:** the durable-workflow benchmark fixture receives only the framed response from
  the fake worker binary selected for the active coverage build.
- **Completion criteria:** prove the build/launch mismatch, add a regression for the active
  target contract, apply the smallest verified fix, and pass focused and workspace coverage.
- **Provenance:** issue #463 problem statement, evidence, expected behavior, and proposed
  investigation.
- **Exclusions:** no production protocol, concurrency, schema, migration, or dependency change.
- **Surface:** the durable-workflow test fixture and the directly shared worker test helper.
- **Ambiguities:** none.

## Context

`cargo llvm-cov` runs the control-plane test binary from
`target/llvm-cov-target/debug/deps`. The durable-workflow fixture does not receive a
`CARGO_BIN_EXE_*` path in that unit-test context, so it falls back to a nested
`cargo build -p voom-fakes --bins`. That nested command writes to the default target tree,
but the fixture then resolves and launches the worker from `CARGO_TARGET_DIR`, the coverage
tree. A focused coverage run reproduced this split: the outer test ran from the coverage
tree while the nested build compiled a second copy in the default tree.

The nested build therefore does not determine the executable that the test launches. A
missing active-target executable produces a launch failure; a cached incompatible executable
can produce the observed duplicate or malformed response framing. The shared
`voom_test_support::worker::cargo_bin_or_build` helper already exists to keep nested builds
and launched paths in the active profile tree, including `llvm-cov`, custom target roots,
prebuilt-worker test runs, and all-feature workspace builds.

## Approaches considered

### Reuse the shared active-target helper (selected)

Replace the fixture's private binary lookup and nested build with
`cargo_bin_or_build("voom-fakes", name)`. This removes duplicate target-directory logic and
ensures the returned path is the artifact just built or the explicitly prebuilt artifact.
It is the smallest change and follows the repository's existing worker-test boundary.

### Prebuild workers in the coverage recipe

Add a separate workspace build and set `VOOM_TEST_PREBUILT_WORKERS=1` for coverage. This can
make workers available, but it widens the coverage and workflow configuration and leaves the
fixture's incorrect fallback available to other invocations.

### Add another durable-workflow-specific launcher

Teach the local helper to derive the active target root and match all features. This can fix
the immediate path, but it duplicates behavior already maintained in `voom-test-support` and
can drift again.

## Design

The durable-workflow fixture delegates provider binary selection and any required build to
`cargo_bin_or_build`. Its provider process ownership, stdout readiness-line handling, HTTP
NDJSON transport, credentials, registration order, and shutdown behavior remain unchanged.
The obsolete local target-path and build helpers are removed.

The shared helper's active-target invariant receives a focused unit regression: the target
root supplied to nested Cargo builds must own the active profile directory used by
`target_debug_binary`. The existing durable-workflow benchmark remains the end-to-end
regression. Its bite is demonstrated by removing the active-target provider artifacts after
the coverage test binary is built: the old fixture builds elsewhere and fails to launch;
the delegated helper rebuilds into the active target and the benchmark succeeds.

## Failure behavior

Build failures remain loud and include the package/binary name and Cargo exit status.
Prebuilt-worker mode continues to fail with the expected missing-artifact diagnostic instead
of silently relinking. Provider startup, malformed readiness output, protocol failures, and
cleanup errors retain their existing mappings.

## Verification

1. Run the focused helper unit regression and prove it fails against an intentionally broken
   target-root derivation, then restore the implementation and prove it passes.
2. Run the focused durable-workflow benchmark under `cargo llvm-cov`, with the active worker
   artifacts absent before process launch, to prove the old path fails and the new path builds
   and launches the same artifact.
3. Run the ordinary focused durable-workflow benchmark.
4. Run `just ci`.
5. Run `just coverage`, which exercises the exact workspace coverage command from issue #463.

## Non-goals

This change does not alter `ProgressFrame`, `OperationResponse`, HTTP/NDJSON framing, fake
provider behavior, production worker execution, or test concurrency.
