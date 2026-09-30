# Test build profile measurements (#638)

[Issue #638](https://github.com/randomparity/voom-v2/issues/638), the
[approved repair and experiment](https://github.com/randomparity/voom-v2/issues/638#issuecomment-5903733342),
and the [shared-source continuation](https://github.com/randomparity/voom-v2/issues/638#issuecomment-5917131886)
authorize this comparison. Measurement source is
`e3947b47c742f5f3fa0c7a9768c7903ef5b90d90`, incorporating prerequisite
[#644](https://github.com/randomparity/voom-v2/issues/644) via
[PR #645](https://github.com/randomparity/voom-v2/pull/645).

## Conditions and method

Both targets use Rust/Cargo 1.95.0 and just 1.58.0. Local macOS arm64 has 18 CPUs,
128 GiB RAM, FFmpeg 9.0.2, MKVToolNix 102 and jq 1.7.1. Local Debian 12 GNU/Linux
arm64 has 18 visible CPUs, 32 GiB allocated memory (about 31.3 GiB guest memory),
1 GiB swap, FFmpeg/ffprobe 5.1.9, MKVToolNix 102 and Debian jq 1.6-2.1+deb12u2.
Linux source uses a host-backed mount; target artifacts use native container storage.
Both fresh compilation and full tests run without root or effective capabilities,
with an owned native ext4 volume at the unchanged forced `.test-tmp` path.
The Linux container is local evidence; hosted Ubuntu/macOS Actions are separate
compatibility evidence. Neither host is an isolated benchmark machine. Unrelated
host activity was observed during this campaign, with unknown overlap. Compare
profiles within each OS; do not compare absolute speed across hosts.

Profiles use isolated targets and explicit `CARGO_PROFILE_DEV_DEBUG` and
`CARGO_PROFILE_TEST_DEBUG` values `2` or `line-tables-only`. Test selection and
concurrency remain unchanged. Compilation order is baseline/candidate then
candidate/baseline. Each arm measures cold, warm, workspace cleanup, then simulated
restored-cache compilation. The three commands run through `just --command`:

1. `cargo build --workspace --all-features --all-targets`
2. `cargo test -p voom-control-plane --no-run`
3. `cargo test --workspace --all-features --no-run`

Each adds `--locked --timings --message-format=json`. Cold targets are empty but
registry downloads are prefetched and excluded. Simulated restore uses
`cargo clean --workspace`, retaining dependency artifacts. It measures neither
hosted archive transfer nor extraction. Artifact sizes are logical file bytes,
including split debug and incremental companions, excluding Cargo timing reports.
Compare matching states; macOS warm targets accumulate incremental companions.

Four direct warm `just test` totals per OS run in ABBA order on round-two targets.
They include compilation and both existing behavioral passes. Cold/restored results
are compile-only; this report does not present projected sums as measured totals.
Full stdout/stderr and atomic source/base/environment/command/exit/duration records
are retained. All prior samples and failures remain historical. The cap is twenty
invocations: ten historical repaired-source invocations preceded the eight completed
fresh observations, consuming eighteen. No extra successful observation was taken.

## Worker safety and test coverage

Two existing test entrypoints bypassed prebuilt mode and rebuilt workers with a
narrower feature graph: conformance fake workers and artifact verification. The
repair checks resolved prebuilt files or reuses the existing
`voom_test_support::worker::cargo_bin_or_build`, reports absent files clearly,
and preserves standalone Cargo fallback. Child-process regressions with Cargo
absent from PATH failed before repair and passed after it. Standalone conformance
and artifact integration checks also passed. No behavioral pass was removed.

A transparent Cargo observer forwards arguments, environment, output and exit
status. An owned controlled exit-7 probe verified forwarding. The observer checks
23 executable workers after completed prebuild and after each test pass. Bytes
and SHA-256 must remain equal; inode and modification time are diagnostic only.
A controlled replacement of an owned copy proves that changed bytes are rejected.
The expected Cargo invocation count is three. Hashing overhead is included in
full totals and reported separately.

## Historical attempts and diagnosed failures

The unrepaired baseline passed in 339.940 seconds without a worker identity
inventory. The unrepaired candidate passed in 433.618 seconds, but its observer
failed because 14 worker hashes changed. Fingerprint evidence on both profiles
showed narrower package-only Tokio features; the bypass predated the profile
experiment. These two runs are separate historical evidence, not acceptance samples.

At repaired source `167259bc`, four macOS full runs completed with stable workers:
baseline 376.969/350.110 seconds and candidate 391.023/352.343 seconds. Each had
4,872 passed and 23 ignored occurrences. They are historical because #644 changed
the shared source; they cannot be mixed with the new Linux comparison.

The first Linux compilation attempt used about 7.75 GiB guest memory. A compiler
was killed, the owned cgroup recorded OOM, swap was exhausted, and memory pressure
was severe. The authorized interrupt returned 130 after 829.590 seconds. This is
an incomplete resource attempt, not a product failure or cold acceptance sample.
The operator authorized 32 GiB memory and restart; old evidence was retained.

Six subsequent Linux full attempts at `167259bc` stopped on their first baseline:

| Seconds | Passed / failed / ignored before stop | Cause and verified correction |
|---:|---:|---|
| 98.230 | 946 / 1 / 0 | Non-UTF-8 fixture creation returned EPERM on the host-backed temp filesystem. Native ext4 at the unchanged pin passed the exact fixture and temp-root guard. |
| 93.481 | 1,198 / 1 / 14 | Missing jq caused spawn ENOENT. Distro jq installation passed the exact health-contract test. |
| 121.042 | 1,291 / 1 / 14 | MKVToolNix 74 violated the worker minimum 80. Official maintainer package 102 passed actual worker preflights, mixed-kind run-local, all 19 real media cases and corpus execution. |
| 238.216 | 3,006 / 1 / 17 | Root bypassed a permission test's explicit non-root assumption. Non-root execution without effective capabilities passed the exact permission test and actual worker preflights. |
| 162.411 | 1,370 / 1 / 14 | Three fixed conformance outputs retained ownership from earlier owned root runs. Exact fixture-hash attribution permitted scoped ownership correction; full conformance passed. |
| 340.979 | 4,607 / 1 / 17 | An uncontracted 25 ms SQLite migration-test deadline observed 174.903 ms. Separate #644 replaced it with deterministic rollback and actual error-classification assertions. |

All six exited 101; their workers stayed unchanged through the failed phase only.
They are incomplete observations, not successful full runs. The jq/media/non-root
misses were measurement preflight omissions. Each correction was diagnosed and
verified before another attempt. The repository timing-test failure stopped the
experiment until its separately approved prerequisite landed; a passing rerun was
not accepted as a fix. The corrected environment is fixed across the fresh pairs.

## Diagnostics

A temporary Cargo package deliberately panics through three non-inlined functions.
Useful diagnostics require the panic text and exact `fail` at `lib.rs:3`, `caller`
at line 8, and `diagnostic` at line 13. Debug-zero is the negative control. Cargo's
actual splitting is preserved: macOS uses unpacked companions; Linux its default
layout. Deliberate panic commands return 101, while the checker must pass.
Line tables omit variable/type debugger information and macOS module qualification;
leaf function identity and exact file/line are required. An earlier matcher wrongly
required qualification; its initial failure and corrected negative control remain
recorded separately from suite outcomes.

Both fresh platform checkers passed: baseline and line tables retain all three
required frames, while debug-zero omits them. Each deliberate panic returned 101.

## Compilation and artifacts

Each cell lists round one / round two. Times are seconds for all three phases;
all 72 commands exited zero at the shared source.

| OS | State | Full debug seconds | Line-table seconds |
|---|---|---:|---:|
| macos | cold | 80.970 / 80.290 | 69.845 / 70.939 |
| macos | restored | 74.127 / 71.197 | 63.286 / 60.575 |
| macos | warm | 7.144 / 7.008 | 6.969 / 6.867 |
| linux | cold | 108.133 / 143.772 | 144.418 / 96.894 |
| linux | restored | 164.836 / 123.701 | 116.133 / 101.139 |
| linux | warm | 21.375 / 28.504 | 41.193 / 20.951 |

| OS | State | Full debug bytes | Line-table bytes |
|---|---|---:|---:|
| macos | cold | 16367504560 / 16367503983 | 10706567935 / 10706569197 |
| macos | restored | 16369584761 / 16369584948 | 10708648406 / 10708648610 |
| macos | warm | 17240766862 / 17240766183 | 10925256467 / 10925257250 |
| linux | cold | 33223357076 / 33223356774 | 14494940969 / 14494940306 |
| linux | restored | 33225579161 / 33225579840 | 14497163135 / 14497163006 |
| linux | warm | 33213764673 / 33213764167 | 14485406635 / 14485406347 |

The first Linux cold pair favors full debug, while the second favors line tables.
The frozen requirement for lower compilation in both repeated cold samples on both
OSs is therefore unmet. Smaller artifacts and macOS improvement do not override
that rule. Ranges are observations, not confidence intervals. A Linux snapshot after
the first cold pair recorded no cgroup OOM events, CPU pressure `some avg60=25.27%`
and memory pressure `full avg60=1.27%`; it is not complete interval telemetry or
proof of the cause of timing variation.

## Units that rebuild

Cargo artifact records match package, target name/kind, feature set, optimization
level and test status between profiles in both rounds on each OS. Debug information
is the deliberate difference: test artifacts report full debug or line tables;
build scripts report debug0 in both arms.

| Phase/state | Compiled units | Reason |
|---|---:|---|
| Cold prebuild, macOS / Linux |441 /440|183 workspace units plus258 /257 dependency units.|
| Cold default control-plane pass |51|28 dependency units and23 workspace units in its narrower feature graph.|
| Cold all-feature pass |41|CLI library/binary/integration units invalidated by its build script.|
| Warm prebuild / default / all-feature |41 /0 /41|CLI invalidation recurs; default-feature variants are cached.|
| Simulated restored prebuild / default / all-feature |183 /23 /41|Workspace outputs were removed; both dependency feature variants remain cached.|

The 28 default-pass dependency units represent 24 packages. `serde_core`, `serde_json`,
`blake3` and `libsqlite3-sys` each contribute a build script and library; the others
are libraries: `zeroize`, `secrecy`, `base64`, `cc`, `serde`, `tracing-serde`, `either`,
`serde_urlencoded`, `deranged`, `tokio-stream`, `tokio-util`, `tracing-subscriber`,
`tokio`, `time`, `sqlx-core`, `h2`, `hyper`, `hyper-util`, `sqlx`, `sqlx-sqlite`.
The 23 workspace units comprise 12 libraries, the control-plane unit-test target,
and 10 integration targets. Direct feature differences include default features on
`zeroize`, `base64`, `serde_core`; `serde_json/raw_value`; `cc/parallel`;
`tokio/full,signal,parking_lot`; `hyper-util/client-proxy,service`; and the
control-plane test feature. Other rebuilds inherit the changed dependency graph.

`crates/voom-cli/build.rs` watches `.git/HEAD` and `.git/refs/heads`, but `.git` is
a file in a linked worktree. An earlier retained Cargo fingerprint reproduction reported a missing watched
file. The current source retains those watches and the fresh artifact records show
the same 41 CLI units rebuilding. This unchanged behavior
occurs in both profiles and is reported as a separate follow-up candidate; this
change does not fix it or count its removal as savings.

The twelve narrower-feature workspace library targets are `voom_artifact`, `voom_control_plane`,
`voom_core`, `voom_events`, `voom_fake_support`, `voom_ffmpeg_worker`, `voom_plan`,
`voom_policy`, `voom_scheduler`, `voom_store`, `voom_test_support`, `voom_worker_protocol`.

The ten control-plane integration targets are `benchmark`, `capability_api`,
`compliance_execute`, `durable_scan_session_flow`, `local_worker_lifecycle`,
`published_grammar_corpus`, `sample_policies_plan`, `sample_policy_plan`, `scan_session_scale`,
`staged_artifact_flow`.

The control-plane library also rebuilds as a unit-test target. CLI has four further
units: `voom_cli` library and `voom` binary, each normal and unit-test variants.

The 37 rebuilt CLI integration targets are `artifact_envelope`, `backup_envelope`,
`bad_args_envelope`, `build_script`, `bundle_envelope`, `chaos_librarian_e2e`,
`compliance_envelope`, `empty_scan_noop`, `external_system_envelope`, `global_flags_envelope`,
`health_envelope`, `init_envelope`, `inspection_envelope`, `issue_envelope`,
`job_cancel_envelope`, `lease_commit_gate_e2e`, `lease_envelope`, `library_envelope`,
`log_format_env_override`, `multi_phase_flow`, `multi_phase_preview_envelope`, `node_envelope`,
`operator_execution_e2e`, `plan_envelope`, `policy_envelope`, `process_harness`,
`profile_envelope`, `published_grammar_execution_e2e`, `run_local_stdout_contract`,
`safety_policy_envelope`, `scan_envelope`, `scan_session_envelope`, `scheduler_envelope`,
`scheduling_policy_envelope`, `scoring_profile_envelope`, `version_envelope`, `worker_envelope`.

## Direct full totals and inventory

All eight full runs exited zero; each included the default control-plane pass and
all-feature workspace pass, three Cargo invocations, 170 test-result groups and
23 unchanged executable worker hashes after prebuild through both passes.

| OS | Profile | Round one / round two seconds | Passed / ignored per run | Hash overhead seconds |
|---|---|---:|---:|---:|
| macos | baseline | 331.588 / 301.147 | 4872 / 23 | 0.348 / 0.354 |
| macos | candidate | 330.205 / 297.536 | 4872 / 23 | 0.334 / 0.332 |
| linux | baseline | 313.905 / 306.590 | 4888 / 22 | 2.300 / 2.075 |
| linux | candidate | 304.712 / 308.443 | 4888 / 22 | 0.773 / 0.734 |

Exact passed and ignored test-name occurrence multisets match across all four runs
within each OS. Linux has 16 additional passed occurrences: 12 node-agent child
tests gated by `cfg(all(test, target_os = "linux"))`, plus two Linux-only
control-plane tests each present in both passes (non-UTF-8 canonical policy path
and failed-startup process-group cleanup). Linux omits the macOS-only ignored
VideoToolbox preflight. These are platform configuration differences, not coverage
removed by the candidate.

Ignored occurrences are unchanged: twelve Chaos Librarian cases, five toxiproxy
cases, two 100,000-row scan diagnostics each encountered twice, one distributed
stress case, and macOS's one VideoToolbox case. That is 23 occurrences / 21 distinct
names on macOS and 22 / 20 on Linux. No opt-in suite was enabled; applicable real
media conformance cases ran without runtime prerequisite skips.

One Linux subprocess printed JSON between a test-start line and its following
`ok`, so the initial one-line name parser missed a passed case. The corrected
checker retains exact test-start occurrence multisets, distinguishes ignored names,
and requires successful terminal status, zero failed groups and exact passed-plus-
ignored totals. Removing one name in a controlled copy fails that check. The initial
mismatch remains recorded; no suite rerun or coverage criterion was waived.

## Outcome and applicability

**Measured no-go: retain the existing full-debug profile.** Both macOS cold and
restored pairs, and both Linux restored pairs, favor line tables; artifact sizes
shrink on both platforms and useful file/line/function diagnostics survive. However,
Linux cold results reverse direction between rounds. That fails the frozen
repeatable-improvement rule. The direct warm totals also vary: macOS candidate is
1.383 / 3.611 seconds lower, while Linux is 9.193 seconds lower / 1.854 seconds
higher. Overlapping ranges do not establish a warm runtime gain or causal regression.
No favorable pair replaces an unfavorable observation.

Cargo profiles, feature selection, the two behavioral passes and CI recipes stay
unchanged. The shipped change retains the two prebuilt-worker test repairs and this
report. No duplicate-pass removal is counted as savings. Historical versus fresh
elapsed differences are not attributed to the repair or prerequisite test correction.
No ADR adopts a profile that failed the approved rule.

After the frozen measurements, commit `74b3a43a89cc16731f52d6f7653b0af2959ac3f2`
strengthened the artifact regression to assert
that `WorkerCommand.program` is the exact expected prebuilt sibling path. A controlled
wrong sibling failed (exit 101); the corrected assertion passed on macOS and Linux.
This changes only the test assertion and child expected-path environment, not worker
resolution, production behavior, profiles or test selection. The measurement source
above remains explicit; final guardrails and hosted checks cover the subsequent commit.

Full `just ci`, configured hooks and exact-head hosted compatibility remain release
gates for these Rust test changes even though the profile outcome is no-go.

Local `just ci` passed at `efa36ca3838280ac57ff72bbfeaae339081a2b97`
(exit 0, 349.027 seconds), including 4,872 passed and 23 explicitly ignored test
occurrences. The following documentation-only gate reference does not change the
verified Rust sources, manifests, profiles or recipes. Configured hooks passed for
the Rust assertion commit; exact-head hosted checks remain a separate release gate.
