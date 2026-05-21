---
name: voom-sprint-2-phases-4-5-6-conformance-fill-in-design
description: Sprint 2 Phases 4, 5, 6 follow-up design — fill in the Phase 6 conformance harness first and define the admission gates that later chaos-worker and benchmark-worker implementations must pass before their deeper Phase 4/5 tests are accepted.
status: proposed
date: 2026-05-21
sprint: 2
phases: [4, 5, 6]
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-phases-4-5-6-design.md
parent_sections: §4 Phase 6 conformance expansion; §2 Phase 4 chaos worker; §3 Phase 5 benchmark worker
scope: Phase 6 conformance fill-in plus Phase 4/5 admission gates; deep chaos and benchmark behavior remains deferred
---

# Sprint 2 Phases 4-6 Follow-up — Conformance Fill-in First

## 1. Goal

The previous Phases 4-6 slice put `chaos-worker`,
`benchmark-worker`, `voom-fakes-manifest.toml`, and the Phase 6
conformance extension points on disk as scaffolds. This follow-up
turns `voom-conformance` into a real gate before any deeper chaos or
benchmark behavior lands.

The implementation order is conformance-first:

1. Fill in the typed and raw-wire conformance suites against
   `echo-worker`.
2. Make the harness consume `voom-fakes-manifest.toml`.
3. Keep Phase 4 and Phase 5 workers scaffolded until their
   non-faulting baseline operations can pass the conformance gate.

This document does not introduce a new Sprint 2 phase. It is a
follow-up design for the already-scaffolded Phase 6 work and the
admission rules that protect the later Phase 4/5 deepening work.

## 2. Scope

In scope:

- Add real `typed_suite` and `raw_wire_suite` modules to
  `voom-conformance`.
- Make `Harness::run_typed_suite`, `run_raw_wire_suite`, and
  `run_all` execute named assertions instead of returning empty
  `SuiteResult`s.
- Add manifest parsing so active binaries are tested and scaffold
  binaries are explicitly skipped.
- Keep `echo-worker` as the required passing target for this slice.
- Define promotion rules for `chaos-worker` and `benchmark-worker`:
  once either stops being a scaffold, it must move to an active
  manifest entry and pass conformance before deeper tests are
  accepted.

Out of scope:

- Implementing chaos failure modes.
- Implementing benchmark throughput or latency reporting.
- Replacing Phase 3 fake-provider placeholders.
- Adding supervisor-side recovery assertions beyond what the
  conformance harness can test against a launched worker binary.

Exit criteria:

- `cargo test -p voom-conformance` fails if an active binary produces
  an empty suite.
- `echo-worker` passes the typed and raw-wire suites.
- Manifest handling proves scaffold binaries are skipped
  intentionally, not silently ignored.
- The suite names every assertion that lands now and every Phase 4/5
  assertion that becomes an admission gate later.

## 3. Architecture

`voom-conformance` becomes a small manifest-aware harness plus two
explicit suite modules:

| Module | Responsibility |
|---|---|
| `harness.rs` | Process launch, shutdown, result aggregation, and calls into the suites. |
| `manifest.rs` | Parse `voom-fakes-manifest.toml`, classify active vs scaffold binaries, and fail closed on ambiguous entries. |
| `typed_suite.rs` | Semantic protocol checks through `voom-worker-protocol::HttpClient` and typed envelopes. |
| `raw_wire_suite.rs` | Byte-level protocol checks through `voom-worker-protocol::low_level` and hand-authored HTTP/JSON bytes. |

The crate remains independent from `voom-fake-support` and
`voom-fakes`. The harness only knows binary paths and the public wire
protocol. This preserves the Sprint 2 invariant that shared fake
helpers cannot hide contract drift.

## 4. Data Flow

A normal conformance run proceeds as follows:

1. Load and validate `voom-fakes-manifest.toml`.
2. For each active binary, launch the worker and wait for
   `BOUND addr=...`.
3. Run the typed suite through the public typed client path.
4. Run the raw-wire suite by opening direct loopback connections and
   sending hand-authored HTTP/JSON bytes.
5. Aggregate named pass/fail entries into `SuiteResult`.
6. Fail the active binary if no checks executed.
7. Report scaffold binaries as skipped by name without counting them
   as passing.

For this slice, `echo-worker` is the only active worker expected to
pass. `chaos-worker` and `benchmark-worker` remain listed as
scaffolds until their non-faulting baseline operations exist.

## 5. Assertions

The conformance assertions are split into two tiers.

### 5.1 Tier 1 — Required Now

Tier 1 lands in this follow-up and must pass against `echo-worker`.

Typed assertions:

- `handshake_returns_supported_version`
- `handshake_rejects_below_supported_min`
- `handshake_rejects_above_supported_max`
- `probe_file_accepts_valid_payload`
- `probe_file_rejects_missing_path`
- `unknown_operation_rejected`
- `progress_seq_starts_at_zero`
- `progress_seq_is_monotonic`
- `terminal_frame_is_last`
- `wrong_bearer_rejected`
- `wrong_worker_id_rejected`
- `wrong_worker_epoch_rejected`
- `stdin_eof_terminates_worker`

Raw-wire assertions:

- `golden_handshake_request_round_trips`
- `golden_operation_request_round_trips`
- `missing_auth_headers_rejected`
- `wrong_bearer_header_rejected`
- `wrong_worker_epoch_header_rejected`
- `malformed_json_rejected`
- `wrong_content_length_rejected`
- `unknown_route_returns_404`
- `handshake_version_skew_returns_structured_error`

### 5.2 Tier 2 — Later Phase 4/5 Admission Gates

Tier 2 does not need to land in this follow-up. These checks become
admission gates before Phase 4 or Phase 5 moves a binary from
`[scaffold]` to active:

- Idempotency duplicate-key behavior.
- Cancellation route drains the current operation.
- Oversize NDJSON frame handling.
- Wrong lease id and frame-after-terminal detection.
- Partial response body classification.
- `chaos-worker` non-faulting baseline operation passes Tier 1 before
  fault modes are tested.
- `benchmark-worker` no-op operation passes Tier 1 before throughput
  thresholds are tested.

## 6. Error Handling

Expected protocol rejections are reported as named assertions. They
pass when the worker returns the expected structured rejection and
fail with diagnostic detail when the worker accepts the request,
returns the wrong error shape, or panics.

Worker launch failure, missing `BOUND` line, early process exit, and
shutdown timeout are suite failures tied to the binary name. A manifest
binary listed as both active and scaffold is a manifest validation
failure. A manifest active binary that runs zero checks is a suite
failure, because an empty suite would otherwise make placeholder code
look accepted.

Scaffold binaries are explicit skips. A skipped scaffold is not a pass,
and an unlisted binary is not implicitly skipped.

## 7. Tests

Unit tests:

- Manifest parsing classifies active and scaffold binaries.
- Manifest parsing rejects duplicate or ambiguous entries.
- `SuiteResult` aggregation preserves named pass/fail details.
- Empty active suites are failures.

Integration tests:

- `echo-worker` passes the typed suite.
- `echo-worker` passes the raw-wire suite.
- `run_all` merges both suite results and fails if either layer fails.
- A scaffold binary listed in `[scaffold]` is reported as skipped.
- A nonexistent active binary is a launch failure, not a skip.

Verification:

- Minimum: `cargo test -p voom-conformance`.
- Branch gate: `just ci`.

## 8. Implementation Slices

1. Add manifest parsing and result aggregation tests.
2. Add `typed_suite.rs` and wire `Harness::run_typed_suite`.
3. Add `raw_wire_suite.rs` and wire `Harness::run_raw_wire_suite`.
4. Add integration tests that run `echo-worker` through `run_all`.
5. Update `voom-fakes-manifest.toml` comments so the Phase 4/5
   promotion rule is visible next to the scaffold list.

Every slice keeps `cargo test -p voom-conformance` green before the
next slice starts.

## 9. Follow-ups

After this spec lands and the conformance fill-in is implemented:

- Phase 4 can add `chaos-worker`'s non-faulting baseline operation,
  promote it from scaffold to active, and then implement crash, stall,
  malformed result, non-converging progress, and deadline-exceeded
  scenarios.
- Phase 5 can add `benchmark-worker`'s no-op baseline operation,
  promote it from scaffold to active, and then implement throughput
  and latency measurement.
- Phase 6 can expand Tier 2 into mandatory assertions for every
  active fake-provider binary as those Phase 3 placeholders are
  replaced.
