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
- Tier 1 covers active-worker retry safety and worker identity, plus
  conformance-owned negative fixtures for lease isolation and
  terminal-stream correctness. No Phase 4/5 worker can be promoted
  from scaffold to active until it passes the active-worker Tier 1
  checks.

## 3. Architecture

`voom-conformance` becomes a small manifest-aware harness plus two
explicit suite modules:

| Module | Responsibility |
|---|---|
| `harness.rs` | Process launch, shutdown, result aggregation, and calls into the suites. |
| `manifest.rs` | Parse `voom-fakes-manifest.toml`, classify active vs scaffold binaries, and fail closed on ambiguous entries. |
| `typed_suite.rs` | Semantic protocol checks through `voom-worker-protocol::HttpClient` and typed envelopes. |
| `raw_wire_suite.rs` | Byte-level protocol checks through `voom-worker-protocol::low_level` and hand-authored HTTP/JSON bytes. |
| `negative_fixture.rs` | Conformance-owned fixture server / stream source that emits intentionally malformed response bodies below the typed worker helper path. |

The crate remains independent from `voom-fake-support` and
`voom-fakes`. The harness only knows binary paths, the public wire
protocol, and conformance-owned negative fixtures. This preserves the
Sprint 2 invariant that shared fake helpers cannot hide contract
drift.

`echo-worker` stays minimal and positive-path only. It is not modified
to emit malformed response streams. Wrong-lease, frame-after-terminal,
and truncated-response cases are produced by `negative_fixture.rs`, so
the suite does not confuse protocol-reader coverage with active-worker
behavior.

### 3.1 Manifest schema and resolution

Active binaries are listed as `[[binaries]]` entries:

```toml
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true
```

`name` is the human-readable suite label. `target` is the Cargo binary
target name. `status` must be `"active"` for entries under
`[[binaries]]`. `required` must be `true` in Sprint 2 so an active
binary cannot disappear without failing the gate.

Scaffold binaries remain listed under `[scaffold].binaries`:

```toml
[scaffold]
binaries = [
    "chaos-worker",
    "benchmark-worker",
]
```

Resolution rules:

- Active entries resolve through the Cargo integration-test
  environment variable `CARGO_BIN_EXE_<target>`. The `<target>` token
  is the Cargo binary target name exactly as declared, including
  hyphens.
- An active entry may later add an explicit `path`; when present, the
  harness uses `path` instead of the Cargo environment variable.
- Missing active binaries are failures.
- Missing scaffold binaries are explicit skips.
- A binary listed as both active and scaffold is a manifest validation
  error.
- Unlisted binaries are ignored; they are not implicitly skipped and
  cannot count as passing.

## 4. Data Flow

A normal active-worker conformance run proceeds as follows:

1. Load and validate `voom-fakes-manifest.toml`.
2. For each active binary, launch the worker and wait for
   `BOUND addr=...`.
3. Run the typed suite through the public typed client path.
4. Run the active-worker raw-wire suite by opening direct loopback
   connections to the worker and sending hand-authored HTTP/JSON
   requests for request/auth/idempotency checks.
5. Aggregate named pass/fail entries into `SuiteResult`.
6. Fail the active binary if no checks executed.
7. Report scaffold binaries as skipped by name without counting them
   as passing.

The protocol-negative fixture run is separate: the harness starts or
constructs a conformance-owned fixture that emits malformed response
bodies, then asserts `NdjsonReader` and the raw-wire suite classify
those responses correctly. Fixture passes never count as active-worker
passes.

For this slice, `echo-worker` is the only active worker expected to
pass. `chaos-worker` and `benchmark-worker` remain listed as
scaffolds until their non-faulting baseline operations exist and pass
the active-worker Tier 1 checks.

## 5. Assertions

The conformance assertions are split into two tiers. Tier 1 has two
target groups: active-worker checks and protocol-negative fixture
checks.

### 5.1 Tier 1 — Active worker checks required now

These checks land in this follow-up and must pass against
`echo-worker`. They are also the promotion gate for later active
workers such as `chaos-worker` and `benchmark-worker`.

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
- `idempotency_exact_byte_replay_returns_cached_response`
- `idempotency_same_key_different_body_rejected`
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

### 5.2 Tier 1 — Protocol negative fixture checks required now

These checks land in this follow-up but run against the
conformance-owned negative fixture, not `echo-worker` and not any
Phase 4/5 active worker:

- `frame_with_wrong_lease_id_rejected`
- `frame_after_terminal_rejected`
- `partial_response_body_classified`

The fixture emits the malformed response body directly below the typed
worker helper path. These checks prove the conformance reader and
classification path rejects dangerous response streams without making
positive-path workers implement fault modes.

### 5.3 Tier 2 — Later Phase 4/5 Admission Gates

Tier 2 does not need to land in this follow-up. These checks become
mandatory before Phase 4 or Phase 5 declares its deep implementation
complete, but they are not sufficient to promote a binary from
`[scaffold]` to active. Promotion requires the active-worker Tier 1
checks first.

- Cancellation route drains the current operation.
- Oversize NDJSON frame handling.
- `chaos-worker` fault-mode operations pass their scenario-specific
  malformed-frame and timeout assertions.
- `benchmark-worker` throughput operations pass their measurement
  envelope and threshold assertions.

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

An active binary missing from the resolved Cargo binary environment is
a failure unless it has an explicit manifest `path` that exists.
Scaffold binaries are explicit skips. A skipped scaffold is not a pass,
and an unlisted binary is not implicitly skipped.

## 7. Tests

Unit tests:

- Manifest parsing classifies active and scaffold binaries.
- Manifest parsing rejects duplicate or ambiguous entries.
- Manifest parsing rejects `[[binaries]]` entries whose `status` is
  not `"active"` or whose `required` flag is not `true`.
- Active binary resolution maps `target = "echo-worker"` to
  `CARGO_BIN_EXE_echo-worker`.
- Missing active binaries fail resolution.
- Missing scaffold binaries are reported as skips.
- `SuiteResult` aggregation preserves named pass/fail details.
- Empty active suites are failures.

Integration tests:

- `echo-worker` passes the typed suite.
- `echo-worker` passes the raw-wire suite.
- `run_all` merges both suite results and fails if either layer fails.
- A scaffold binary listed in `[scaffold]` is reported as skipped.
- A nonexistent active binary is a launch failure, not a skip.
- Exact-byte idempotency replay returns the cached response.
- Same-key / different-body idempotency replay fails with
  `DuplicateIdempotencyKey`.
- The negative fixture emits a wrong-lease-id frame and the harness
  classifies it as rejection.
- The negative fixture emits a frame after terminal and the harness
  classifies it as rejection.
- The negative fixture truncates a response body and the harness
  classifies it according to the existing `NdjsonReader` behavior.
- Fixture results never count as active-worker passes.

Verification:

- Minimum: `cargo test -p voom-conformance`.
- Branch gate: `just ci`.

## 8. Implementation Slices

1. Add manifest parsing and result aggregation tests.
2. Add `typed_suite.rs` and wire `Harness::run_typed_suite`.
3. Add `raw_wire_suite.rs` and wire `Harness::run_raw_wire_suite`.
4. Add `negative_fixture.rs` and wire the protocol-negative fixture
   checks into the raw-wire suite.
5. Add integration tests that run `echo-worker` through `run_all` and
   the negative fixture through the protocol-negative checks.
6. Update `voom-fakes-manifest.toml` to use the active-entry schema
   for `echo-worker` and document the Phase 4/5 promotion rule next
   to the scaffold list.

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
