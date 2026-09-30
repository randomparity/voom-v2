# 0099 — Run the test suite with cargo-nextest

## Status

Accepted (2026-09-30)

## Context

`cargo test` runs a workspace's test binaries one after another, and only
parallelizes tests inside each binary. The workspace has 158 test binaries.
With the SQLite contention of ADR 0098 removed, the suite's wall time is the
sum of per-binary times (about 109s on an 8-core Linux host), and several
binaries are dominated by one long test that waits on a real deadline:

| Binary | Long test | Time |
|---|---|---|
| `voom-cli` `published_grammar_execution_e2e` | `published_grammar_corpus_is_executable` | 11.7s |
| `voom-fakes` unit tests | `process_crash_timeout_reaps_stay_alive_worker_after_pending_wait` | 10.3s |
| `voom-control-plane` unit tests | `convergence_deadline_names_pending_intent_and_keeps_record_recoverable` | 10.2s |
| `voom-node-agent` unit tests | `a_sigterm_exits_the_agent_when_the_control_plane_never_answers` | 10.0s |

Each of those waits blocks every later binary. None can be shortened by a
test-only change: the node-agent test takes the default budgets on purpose,
the commit test waits out a production constant that ADR 0012 forbids
virtualizing under a real pool, and the e2e test's time is spent inside the
production CLI.

cargo-nextest schedules individual tests from all binaries on one pool, one
process per test, so the long tests overlap instead of adding up. Measured on
the same host and build (`VOOM_TEST_PREBUILT_WORKERS=1`, warm):

| Runner | Runs | Median wall | Range |
|---|---|---|---|
| `cargo test --workspace --all-features` | 3 | 112.5s | 102.5–133.3s |
| `cargo nextest run --workspace --all-features` | 10 | ~57s | 39.9–89.1s |

All ten nextest runs passed 3919 of 3919 tests. The wide range comes from the
host, not the runner: in the 89s run, unrelated sub-100ms tests all took about
11.6s at the same moment, a host-wide I/O stall. `cargo test` runs showed a
similar spread.

The first nextest run also flagged a real defect that libtest never reported:
`capacity_failure_reaps_remaining_processes` orphaned a `sleep 60` grandchild
that held the test's stdout and stderr on every run (fixed in `a9647771`).

## Decision

`just test` runs both of its passes, the `-p voom-control-plane` wiring pass
and the workspace pass, with `cargo nextest run`. Doctests, which nextest does
not run, run afterwards with `cargo test --doc --workspace --all-features`.
The recipe's pre-build and `VOOM_TEST_PREBUILT_WORKERS=1` are unchanged.

nextest is pinned to 0.9.143 in `just setup` and in the `ci.yml` test job,
through the `taiki-e/install-action` pin the workflow already uses. 0.9.143 is
the newest version that pin's manifest knows. It requires Rust 1.91, so it
builds under the workspace's 1.95.0 toolchain.

`.config/nextest.toml` defines a `process-providers` test group with
`max-threads = 1` covering `workflow::durable_workflow::tests::` in
`voom-control-plane`. Those tests take `PROCESS_PROVIDER_TEST_LOCK`, a static
mutex that serializes them inside one libtest process and does nothing across
nextest's per-test processes. The group carries that serialization. It covers
the whole module (10 tests, 7.6s serialized, shorter than the longest test)
rather than a list of the guarded tests, so a newly guarded test cannot be left
out. The static lock stays because bare `cargo test` still relies on it.

## Consequences

- `just test` needs `cargo-nextest` installed. `just setup` installs it, and CI
  installs a prebuilt binary.
- Bare `cargo test` still works and still runs everything, doctests included.
  No test depends on nextest.
- Process-wide `OnceLock` caches in test files (fake tool binaries in
  `commit_use_lease_gate`, `artifact_envelope` and others) are now initialized
  once per test rather than once per binary. They are small shell scripts in
  files of one to four tests, and their `TempDir`s are never dropped either way.
- nextest reports a test whose child processes outlive it as `LEAK`. That is
  now a visible signal instead of a silent orphan.
- `test-serial`, `test-parallel`, `test-repeat` and `test-constrained` still use
  `cargo test`. They reproduce specific CI job shapes (the coverage job's
  `--test-threads=1` among them), and move with those jobs.

## Considered & rejected

- **Shorten the long tests instead.** judgment: every candidate is either
  deliberate (the node-agent test documents its 10s as the point) or needs a
  production timing knob added for a test's sake. Neither removes the serial
  sum across 158 binaries, which is the underlying cost.
- **Merge integration tests into fewer binaries.** judgment: reduces link
  time and binary count, but tests inside one binary still wait for that
  binary's longest test, and it restructures many files for a smaller gain.
- **Pin nextest 0.9.146 (current) and bump `taiki-e/install-action`.**
  judgment: the bump moves the ast-grep installs in both jobs as well, or
  leaves two pins of one action. Dependabot owns that bump. The nextest pin
  follows it.
- **List the four guarded tests in the test group.** judgment: a hand-kept list
  drifts the moment a test takes the lock without being added. Grouping the
  module costs no wall time.
