# Cargo artifact sharding feasibility (#641)

## Authority and scope

The operator approved “approve 641’s feasibility experiment” on 2026-10-01.
[Scope q641-51776504](https://github.com/randomparity/voom-v2/issues/641#issuecomment-5930266108)
contains the campaign authority and unchanged exclusions. This is feasibility only:
no repetition, adoption, branch-protection change, fixture repair, duplicated-build
fallback, nextest installation or constrained run. No ADR is required for this
experiment. Current production Cargo recipes and serial coverage remain intact.

Allowed execution: focused controls total 10 minutes; one local `just ci` at most
45 minutes before push; one producer per OS at most 45 minutes each; two consumer
shards per OS at most 15 minutes each. Six hosted jobs, at most 150 raw runner-minutes,
80 minutes hosted elapsed including queueing, and 135 minutes combined execution.
Design/review time is separate. Expected failing controls are assertions within the
focused controls, not failed experiment samples. The first unexpected failure,
deadline or exhausted allocation stops; no retry/replacement or implicit repair.

## Experiment

Use the same immutable commit, existing full-debug profiles, Cargo.lock and Rust
1.95.0 on `ubuntu-latest` and `macos-latest`. Record actual image, OS, architecture,
CPU/memory, disk and media-tool facts; do not infer hosted architecture from local
arm64 macOS. Cold means no Rust cache action and a fresh target on the hosted runner;
preinstalled toolchains, registries and media packages are disclosed separately.
Restored-cache behavior is not measured in this stage.

Each producer installs the existing media prerequisites and just, prebuilds workspace
all-feature/all-target workers, and obtains separate Cargo JSON no-run inventories
for default control-plane and all-feature workspace binary arms. Snapshot hashes
before execution. A temporary `CARGO_TARGET_<host>_RUNNER` invokes the collector during
normal Cargo binary test execution, preserving Cargo’s actual package cwd and runtime
environment. It lists names and ignored flags, executes the binary once, reconciles
terminal results, records duration and captures an explicit environment allowlist.
Doctests run separately through Cargo for default control-plane and all-feature
workspace. Any changed prebuilt worker hash or changed all-feature executable after
default preparation/execution rejects the experiment. No Cargo runner config ships.

After a successful baseline, deterministically assign entire all-feature binaries
by descending measured duration to the lighter shard, with package/target/path ties.
Each shard runs its binaries serially; each binary retains Cargo/libtest’s original
within-binary concurrency. Empty and ignored-only binaries remain in the manifest.
Default-feature tests and doctests are producer-only distinct arms, not discarded.

The producer archives only the executable/runtime files required by its manifest,
with target-relative paths and Unix executable modes. Transfer through SHA-pinned
GitHub upload/download artifact actions. The manifest binds source commit, collector
hash, Cargo.lock, toolchain/target, OS/architecture/image, full-debug features/profile,
file SHA-256/modes, package cwd and the runtime environment allowlist. Embedded
workspace paths require equal producer/consumer absolute checkout layouts; compare
path hashes without publishing private path values. Runtime path values are encoded
using workspace/sysroot/home tokens, never a blanket environment dump. Rehydrate
only validated tokens on a matching consumer. Reject symlinks, traversal, unknown
members, changed files and incompatible provenance before executing downloaded code.

Both producers must succeed before four consumers start. Consumers install matching
media tools, checkout the same commit, validate provenance and archive members,
extract to the original target layout, and run assigned whole binaries without Cargo
compilation. Set `VOOM_TEST_PREBUILT_WORKERS=1`; preserve actual package cwd and pinned
on-disk `.test-tmp`. Separate hosted machines isolate databases, lock paths and workers.
Hash workers before/after every executed arm; missing or changed workers are failures.

## Publication and observation

Only a push to `refs/heads/feat/ci-test-sharding-641` activates the experiment in the
existing ci.yml. Ordinary source-guard/test/coverage jobs are skipped for that exact
push ref; their behavior on main and pull requests is unchanged. No PR is opened and
only one experiment push is authorized. Two producer plus four consumer matrix jobs
are the complete hosted execution set; there is no seventh collector/gate job.
Repository-local collection downloads artifacts after completion; no extra hosted
job. Record skipped ordinary job entries as skipped, never as executed verification.

A local supervisor records the first allocated execution time, deadlines and counts,
watches the one workflow run, cancels it on the first failed job or hosted deadline,
and retains native job outcomes/logs. Job timeouts bound consumption even if local
observation fails; loss of monitoring stops further work and requests cancellation.
Record build/no-run preparation, binary execution, doctests, archive size/packing,
artifact action and provisioning times separately. Hosted critical paths include
producer dependencies and transfers. Queue/wait time is explicit. No speedup or
reliability conclusion follows from this single feasibility sample.

## Success and validation

For each OS, reconcile the producer's Cargo JSON target set with recorded binary
runs, then reconcile each binary's named outcomes against its list and ignored list.
The disjoint shard manifest union must equal the all-feature target/test inventory;
consumer results must equal their assigned inventories. Preserve the default-feature,
ignored and doctest arms as separate evidence. Require producer/consumer environment,
commit, file provenance and worker integrity checks. Report unrun arms explicitly.

Focused controls must reject omission, duplication, changed executable/mode, wrong
provenance, unsafe archive names, unexpected environment values and nonzero child
exit; accepted fixtures must pass. A controlled fault in reconciliation must make its
control fail, then be reverted. Validate exact-branch workflow gating structurally.
After review, run the one full local `just ci`; do not push if it fails.

The result is either transferable exact-once execution evidence with measured costs,
or a bounded failure with preserved evidence and unrun criteria. Neither means #641
is complete. Repetition, final gain/cost thresholds and required-gate fault proof need
later authority. Proposed later gates remain 10% median improvement, candidate maximum
below baseline minimum, at most 1.20x runner time, three observations per arm/cache/OS.

## Failure model

1. Actors and deployments: authenticated operator; one feature-branch workflow on
   GitHub-hosted Ubuntu/macOS; same-run producer archives and four consumers.
2. Invariants/assets: exact inventory, feature arms, executable provenance, worker
   integrity, unchanged normal CI/protection, private environment data, bounded cost.
3. Accepted failure classes: portability/tool availability/storage exhaustion or a
   test race can produce a bounded failed feasibility result; never a passing sample.
   Hosted image drift rejects compatibility. Queue exhaustion stops without replacement.
   Timing noise is disclosed; no repeated-performance claim is made in this stage.
4. Covered elsewhere: guard performance #637; build/profile #638; within-runner
   concurrency #639; guard scheduling #640; hooks #485; broader flakes #520.
