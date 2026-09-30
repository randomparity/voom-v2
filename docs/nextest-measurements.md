# Nextest comparison: incomplete no-go (#639)

On 2026-09-30, the [approved experiment](workflow/specs/2026-09-30-nextest-benchmark-design.md)
stopped after its first allocated suite failed. Keep Cargo: adoption was disqualified by
both macOS cold preparation overhead and a failed baseline. This is an incomplete benchmark,
not evidence that nextest execution is slower or unreliable. No nextest test execution ran.
The triggering request is [#639](https://github.com/randomparity/voom-v2/issues/639).

## Conditions and bounded method

Measured source: `a0aa0569c3e91c24258d75f7f1f5fe3df00eefed` (the specification commit;
Rust/configuration bytes equal base `9d7ea8e8582094c214a635dd465f680684c2dbbd`).
Both environments used Rust/Cargo 1.95.0, full-debug repository profiles, Cargo.lock,
just 1.58.0 and isolated [nextest 0.9.146](https://nexte.st/changelog/).
Official release SHA-256 digests were verified before using either archive:

- universal macOS: `39785160b3c2f6ed9a765049cf4fa79f3b39aa02eb7598a5a0e2a1a0b9ffb9a8`.
- GNU/Linux arm64: `b2e33d7c72de7ade0ff7b3a948ac37516b24f8a836b7a8870c1f634a94be9de9`.

| Environment | Resources | Storage and tools |
|---|---|---|
| Native macOS 27, arm64 | 18 CPUs, 128 GiB | Native disk; FFmpeg 9.0.2, MKVToolNix 102.0, jq 1.7.1 |
| Debian 12 container, arm64 | 18 CPUs, 32 GiB allocation | Native ext4 temp volume/overlay target; FFmpeg 5.1.9, MKVToolNix 102.0, jq 1.6 |

Linux ran as a non-root user with zero effective capabilities. Its source was a bind mount;
its target and pinned `.test-tmp` were native storage. This is local GNU/Linux evidence,
not hosted Ubuntu compatibility evidence. Neither environment was an isolated benchmark host.
Cargo passed the pinned-temp guard against both target directories on each OS, using the
existing `.cargo/config.toml` pin. Nextest runtime temp propagation remains unverified.
Five real media workers passed ready/shutdown checks in every combination.
All 23 prebuilt worker executables were inventoried and hashed.

The frozen schedule was C18/N4/N8 screening on each OS, one common candidate selected by
worst-platform execution gain, then N/C/C/N per OS. Hard limits were 14 logical invocations
and 120 minutes, no retries or failure replacements. Each invocation included all-feature/
all-target prebuild, default control-plane binaries/doctests, then all-feature workspace
binaries/doctests. The stop retained one allocated, zero complete and thirteen unstarted
invocations. No candidate was selected, no repetition ran, and no medians can be reported.

Each runner/OS had an empty target directory, one cold and one warm preparation sequence:
`cargo build --workspace --all-features --all-targets --locked`, then default control-plane
and all-feature workspace no-run binary builds. Cargo used `test --lib --bins --tests`;
nextest used `nextest run` with identical selections. Registry fetches preceded timing.
Commands ran through `just --command`, with native exit codes and complete separate logs.
Worker setup, inventories and focused prerequisites were outside suite measurements.

## Measured preparation and failed execution

| OS | Cargo cold (s) | Nextest cold (s) | Cold overhead | Cargo warm (s) | Nextest warm (s) |
|---|---:|---:|---:|---:|---:|
| macOS | 85.546 | 95.748 | +11.93% | 7.004 | 12.626 |
| GNU/Linux | 110.112 | 108.722 | −1.26% | 20.151 | 20.013 |

These are sums of the three measured preparation commands, not complete test totals.
All 24 preparation commands exited zero. The approved cold-overhead ceiling was 5% per OS;
macOS exceeded it. The other adoption gates required every run/inventory/integrity check to
pass, plus at least 10% median execution and 5% median warm-total gain on **both** OSs.
Those execution gates remain unmeasured and cannot be inferred from preparation timings.

The first macOS Cargo18 invocation took 64.289 seconds before stopping: prebuild 4.382 s,
default binary command 59.713 s, plus collector/hash overhead. The binary command exited
101 without timing out. Its 11 binaries reported 965 passed, 1 failed and 2 ignored;
all 968 names/ignored flags matched the listed inventory and all 23 worker hashes remained
unchanged. Sum of Cargo's per-binary execution reports was 59.44 s (two-decimal precision).
No doctest execution or all-feature execution was reached; no passing rerun was attempted.

The failure was `artifact::commit::tests::duplicate_pending_committed_and_recovery_owners_are_rejected_by_repo_constraints`:
`file_versions insert: error returned from database: (code: 5) database is locked` at
`crates/voom-control-plane/src/artifact/commit/mod_test.rs:1705`.
Read-only inspection found deferred `pool.begin()` at line 1684 followed by a read and write.
This is consistent with the lock-upgrade hazard in ADR 0083; the competing writer and exact
cause were not established. The broader failure family has existing owner
[#520](https://github.com/randomparity/voom-v2/issues/520). No repair or test exclusion was made.

## Coverage inventory and unrun arms

| OS | Default binaries/tests/ignored | All-feature binaries/tests/ignored | Listed doctests |
|---|---:|---:|---:|
| macOS | 11 / 968 / 2 | 135 / 3925 / 21 | 0 default; 2 across 23 all-feature crates |
| GNU/Linux | 11 / 970 / 2 | 135 / 3938 / 20 | 0 default; 2 across 23 all-feature crates |

Cargo harness and nextest JSON lists matched exact package, target, test name and ignored
status within each OS. Doctest lists matched too: `crates/voom-store/src/lib.rs` lines 17 and 21,
which are compile-fail API checks. Listing is not execution coverage.
Linux adds two control-plane tests guarded by `target_os = "linux"` (non-UTF8 canonical policy
path rejection and failed-startup process-group cleanup), plus twelve node-agent child tests.
macOS adds the ignored real VideoToolbox test. These source guards explain the platform totals.

Ignored cases are unchanged. Both default passes ignore the two `scan_session_scale` cases
`empty_scan_reconciles_100k` and `max_ledger_reconciles_100k`; all-feature lists additionally ignore:

- `voom-cli / chaos_librarian_e2e / chaos_librarian_submodule_is_pinned_and_ready`.
- `voom-cli / chaos_librarian_e2e / hardlinked_paths_resolve_to_one_physical_file`.
- `voom-cli / chaos_librarian_e2e / malformed_media_scan_request_stays_accepted_without_worker_side_effects`.
- `voom-cli / chaos_librarian_e2e / observed_state_hash_uses_chaos_librarian_prefix`.
- `voom-cli / chaos_librarian_e2e / observed_state_rejects_paths_outside_library`.
- `voom-cli / chaos_librarian_e2e / policy_seed_creates_durable_ids_from_seeded_source`.
- `voom-cli / chaos_librarian_e2e / static_library_baseline_seeds_exports_and_compares`.
- `voom-cli / chaos_librarian_e2e / step_mutation_rescan_rejects_changed_bytes_at_live_rooted_address`.
- `voom-cli / chaos_librarian_e2e / symlinked_media_scan_request_is_accepted`.
- `voom-cli / chaos_librarian_e2e / transcode_noop_does_not_schedule_worker_mutation`.
- `voom-cli / chaos_librarian_e2e / transcode_required_settles_through_owner_node_and_commits_hevc_mkv`.
- `voom-cli / chaos_librarian_e2e / voom_e2e_support_runs_version_envelope`.
- `voom-fakes / voom_fakes / remote_stress::tests::distributed_stress_conserves_every_ticket`.
- `voom-ffmpeg-worker / voom_ffmpeg_worker / preflight::videotoolbox::tests::real_videotoolbox_preflight_proves_host_pipelines` (macOS only).
- `voom-worker-protocol / net_resilience / dispatch_reset_yields_connection_error_not_timeout`.
- `voom-worker-protocol / net_resilience / dispatch_timeout_yields_timeout_error`.
- `voom-worker-protocol / net_resilience / handshake_reset_yields_connection_error_not_timeout`.
- `voom-worker-protocol / net_resilience / handshake_timeout_yields_timeout_error`.
- `voom-worker-protocol / net_resilience / latency_under_deadline_succeeds`.

## Isolation, integrity and disposition

The reviewed nextest policy serialized the four durable-workflow process-local mutex users,
reserved all slots for the three ffprobe-mutating integration binaries, and charged other
control-plane/CLI tests two slots. It was parsed successfully during preparation but never
exercised by nextest execution. Process-local caches initialize per test; resource assumptions
and reliability therefore remain unproven. Existing ffprobe locks and conformance fixed-output
paths were unchanged; no suites overlapped. Campaign-owned worktree rebuild/output-isolation
findings, #483 and the other specification exclusions remain outside this change.

The collector preserved a controlled exit 7; omitted-name and changed-worker evidence failed
reconciliation, while valid evidence passed. Disabling the comparator made its control fail.
A controlled deadline terminated the owned process group (exit −15, timeout recorded).
Raw records retain commands, source/config/collector bindings, inventories, exits and hashes;
private paths and machine identities are omitted here. One initial Linux Git trust check failed;
an exact owned-worktree trust entry resolved provisioning, without changing host Git settings.

Keep the existing Cargo recipes, separate serial coverage and CI unchanged. No nextest install
is required for repository users. Reconsideration needs separately authorized diagnosis of the
baseline failure and a new bounded experiment; this run supplies no selected concurrency,
complete execution comparison, repeated reliability proof or hosted-runner compatibility proof.
