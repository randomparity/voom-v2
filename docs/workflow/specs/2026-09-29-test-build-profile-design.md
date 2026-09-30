# Test compilation profile measurement (#638)

## Problem and authority

The test recipe completes an all-feature workspace/all-target prebuild, then runs
control-plane default-feature tests and workspace all-feature tests. Compilation
still occurs between these phases. Issue #638 requires attribution and measured
improvement or a measured no-go, preserving worker safety and diagnostic usefulness.
The operator approved this experiment on 2026-09-29 through campaign #637–#641.
Frozen scope: [WORK:SCOPE](https://github.com/randomparity/voom-v2/issues/638#issuecomment-5902874805).
The later [approval](https://github.com/randomparity/voom-v2/issues/638#issuecomment-5903209786)
authorized the original schedule. A later operator decision, “approve the test-only
repair and fresh experiment” ([scope delta](https://github.com/randomparity/voom-v2/issues/638#issuecomment-5903733342)), authorizes the bounded repair and fresh schedule below. Complexity M; fixed design denominator 250.

## Design and ownership

Keep the current recipe and both behavioral passes. Compare the default profile
against `debug = "line-tables-only"` inherited from `profile.dev` by `profile.test`.
During measurement use `CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG`
overrides, without changing source or the normal target directory. If accepted,
add only `[profile.dev]` with that setting to Cargo.toml; the test profile inherits
it. Full local variable/type debugging remains available using Cargo's environment
overrides. Record that tradeoff explicitly; do not claim equivalent debugger data.

The workspace recipe continues to own worker prebuilding. Tests continue to use
`VOOM_TEST_PREBUILT_WORKERS=1`. Repair two test-only bypasses: conformance
`ensure_fake_worker_bins_built` checks resolved manifest binaries and returns before
Cargo when prebuilt mode is present; missing files report the path and prebuild
command. Its existing standalone Cargo fallback remains. Artifact unit-test
`verify_worker_command` reuses the existing `voom-test-support::cargo_bin_or_build`
and rejects a missing resolved path, replacing its duplicate package-build helper.
This dev dependency already exists; conformance adds no dependency. No production
lookup changes. Focused child-process regressions isolate environment changes, run
with Cargo absent from PATH, and prove existing and missing prebuilt paths for both
entrypoints. The child uses its own executable as a harmless existing fixture;
no shared worker is renamed, overwritten or executed by the probes.
No ownership migration, obsolete production path or new compatibility path is needed.
Feature consistency is examined through Cargo artifacts/features, but a selection
change requires a separate concrete design decision; this experiment changes debug
information only. Do not attribute removed behavioral passes to this issue.

## Experiment

Measure macOS arm64 locally and GNU/Linux with Rust 1.95.0, the repository-pinned
toolchain. Record actual OS/architecture, CPU/memory, storage, tool/media versions,
Cargo settings and source identity. Linux containers are local Linux evidence;
Ubuntu/macOS hosted Actions later establish hosted compatibility, not performance.
Do not compare absolute speed between hosts. Run profile comparisons serially within
each host, keeping execution concurrency and test selection unchanged.

For each OS/profile use an isolated target and repeat the unchanged three-phase
compilation sequence twice in each state. Each sequence is:
`cargo build --workspace --all-features --all-targets`,
`cargo test -p voom-control-plane --no-run`, then
`cargo test --workspace --all-features --no-run`.
Run through `just --command`; capture Cargo JSON artifacts and timings separately.

- Cold: empty target, pre-fetched registry; this is cold compilation, not cold network.
- Simulated restored cache: retain registry dependency artifacts from a populated
  target, remove only workspace-package outputs with `cargo clean -p` for the
  enumerated workspace members. Report this exact simulation; it is not a real
  rust-cache restore or an estimate of archive transfer time.
- Warm: immediately repeat with source/toolchain/profile/target unchanged.

Capture complete stdout/stderr and an atomic terminal record of command, source,
environment, exit and duration for each long run. Record dependency/workspace
artifact freshness and feature sets; explain changed build units with source or
fingerprint evidence. Count bytes recursively, including macOS split debug files,
using the same measurement boundary after each sequence.

Run exactly eight full `just test` measurements: two per profile per OS. Use warm
compilation targets for these totals, all on the repaired source. The two earlier
full runs remain historical and do not count toward these eight. Repeat the
compile-state matrix on repaired source; retain earlier observations separately. These are direct warm build-plus-test totals. Report measured
cold/restored compilation separately. Any sum combining them with observed test
runtime is labeled a projection and is not direct cold/restored full-suite evidence.
Record order and ranges rather than guaranteed savings from two observations.

Before each fresh full run, finish its compilation sequence and hash/stat the
worker binaries; compare bytes/hash after tests and retain inode/mtime observations.
Cargo may replace a same-byte top-level artifact during the initial prebuild; inode
change alone is not byte corruption. Capture identity immediately after the recipe
prebuild as well as after both test passes using an ignored Cargo command observer
that forwards unchanged arguments/env and records identities only at phase exits.
No worker bytes may change after prebuild completes. Any worker byte change, missing
executable, test failure or ignored-set discrepancy
requires investigation before accepting the sample. Preserve existing opt-in skips.

On both OSs/profiles, run a deliberately failing temporary Cargo test package with
nested non-inlined functions and RUST_BACKTRACE=1, using the same effective profile
and Cargo-selected debug splitting as workspace tests; record verbose flags and
companion-file layout. Require the same assertion/panic text,
function names and exact source file/line for the user frames. Also record the
profile reported by Cargo for workspace test artifacts. Delete only owned temporary
fault fixtures; do not introduce a permanently failing repository test.

## Decision and success

Report distributions, complete commands/conditions, build-unit reasons, artifact
sizes, diagnostic excerpts, worker checks and ignored tests in one evidence report.
Adopt only if both hosts show lower compilation time in both repeated cold and
simulated-restored samples, smaller artifacts, preserved useful diagnostics, and
no repeatable regression in direct warm total cost. Where observed ranges overlap,
state uncertainty; do not claim a warm runtime gain. If the evidence is ambiguous
or loses useful diagnostics, retain the current profile and record measured no-go.
Both profile outcomes retain Rust test repairs and therefore owe full `just ci`,
configured hooks, independent review and exact-head Ubuntu/macOS/coverage/audit
Actions. A rejected profile is not a documentation-only change.

## Failure model

- Actors/deployments: developers on macOS and GNU/Linux; Ubuntu/macOS CI; local
  container measurements are explicitly distinct from hosted measurements.
- Invariants/assets: completed worker build before execution, executable integrity,
  default/all-feature test coverage, useful file/line/function diagnostics and
  honest attribution of build cost under fixed conditions.
- Accepted classes: timing noise is reported as ranges and uncertainty; line tables
  omit variable/type debugger data by approved tradeoff; unrelated host architectures
  and opt-in hardware/service suites are outside this bounded experiment.
- Covered elsewhere: duplicate behavioral pass #636; runner/concurrency #639;
  source guards #637/#640; sharding #641; compiler-cache infrastructure/toolchain
  floor changes require future authorization. Linked-worktree CLI rebuild behavior
  is an observed separate candidate, held unchanged in both profile arms.
