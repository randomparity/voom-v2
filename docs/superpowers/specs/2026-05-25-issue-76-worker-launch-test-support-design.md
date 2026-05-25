# Issue 76 Worker Launch Test Support Design

## Context

Issue #76 tracks duplicated integration-test worker launch code in:

- `crates/voom-cli/tests/support/voom_cli.rs`
- `crates/voom-control-plane/tests/video_transcode_flow.rs`
- `crates/voom-cli/tests/compliance_envelope.rs`

The repeated flow registers a worker, starts a worker process with protocol
environment variables, reads the `BOUND addr=...` line, records capability and
grant rows, and shuts the process down by closing stdin.

## Goals

- Move the shared worker launch/setup flow into one test-support boundary.
- Keep the helper usable from integration tests in both `voom-cli` and
  `voom-control-plane`.
- Preserve existing test behavior and worker names/secrets where tests rely on
  stable diagnostics.
- Keep production crates free of new runtime APIs.

## Non-Goals

- Do not consolidate every `BOUND addr=...` reader in the repository.
- Do not change production worker protocol behavior.
- Do not refactor unrelated scan/artifact worker binary helpers.
- Do not alter compliance or transcode product behavior.

## Design

Add a new workspace crate, `voom-test-support`, intended for dev-dependencies
only. It depends on `voom-control-plane`, `voom-store`, and `serde_json`, and
exposes a small worker module:

- `target_debug_binary(name) -> PathBuf` resolves built workspace binaries.
- `cargo_build_package(package)` builds a package when a test needs an external
  binary that Cargo did not build for that integration target.
- `cargo_bin_or_build(package, binary)` first uses `CARGO_BIN_EXE_<binary>` and
  falls back to `cargo build -p <package> --bin <binary>`.
- `TestWorkerLaunch::start(cp, config)` registers a worker, spawns the process,
  reads the bound address, records one capability and one grant, and returns a
  shutdown handle.

`TestWorkerConfig` carries binary path, worker name, secret, operation name,
and optional max parallel. Tests remain responsible for selecting operation
names and fixtures, but not for repeating process lifecycle code.

Update the three issue-listed files to use `voom_test_support::worker`. During
simplification review, also migrate
`crates/voom-control-plane/tests/compliance_execute.rs` because it had the same
fake-remuxer registration, launch, bound-address, capability/grant, and shutdown
flow. Leave benchmark and durable workflow launchers unchanged because they carry
additional credentials, bound address, and failure-mode behavior beyond this
issue's shared helper.

## Error Handling

Startup should fail loudly if the worker exits before printing a bound address,
prints malformed output, or if capability/grant writes fail. Shutdown should
close stdin, wait up to five seconds, kill on timeout, and report non-zero exit
status. `Drop` should perform a best-effort one-second cleanup for tests that
panic before explicit shutdown.

## Verification

- `cargo test -p voom-cli --test compliance_envelope execute_outputs_report_and_execution_summary`
- `cargo test -p voom-control-plane --test video_transcode_flow`
- `cargo test -p voom-control-plane --test compliance_execute`
- `cargo test -p voom-cli --test chaos_librarian_e2e -- chaos_run_scan_root_follows_materialized_location_prefix`
- `just fmt-check`
- `just lint`
- `just test`
- `just ci`
