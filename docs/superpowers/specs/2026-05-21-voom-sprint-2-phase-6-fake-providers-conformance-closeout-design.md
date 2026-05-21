---
name: voom-sprint-2-phase-6-fake-providers-conformance-foundation-design
description: Sprint 2 Phase 6 design — implement the deferred eleven fake providers and promote them into manifest-driven conformance as the foundation for Phase 7's simulated scheduler workflow.
status: proposed
date: 2026-05-21
sprint: 2
phase: 6
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 3 fake provider suite; §2 Phase 6 conformance expansion; §4.7 conformance; §5 test discipline
predecessor_specs:
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phases-4-5-6-conformance-fill-in-design.md
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-4-chaos-worker-design.md
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-5-benchmark-worker-design.md
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-5-control-plane-benchmark-harness-design.md
scope: implement and promote the eleven fake-provider workers plus active-worker conformance; real media tooling, supervisor DAG orchestration, and the Phase 7 simulated scheduler workflow remain deferred
---

# Sprint 2 Phase 6 — Fake Providers And Conformance Foundation

## 1. Goal

Sprint 2 has active conformance coverage for `echo-worker`,
`chaos-worker`, and `benchmark-worker`, but the eleven promised
Phase 3 fake providers still exist only as placeholder binaries. Phase
6 turns those placeholders into real synthetic workers and makes the
conformance harness validate every Sprint 2 provider binary as an
active manifest entry.

The foundation goal is practical, not production-like media behavior:
each fake provider must be a deterministic, process-backed worker that
speaks the public worker protocol, validates its own provider payload,
emits stable progress/result frames, and rejects invalid or
unsupported requests predictably. The suite remains synthetic. No real
media tools, external services, policy DAG compiler, production
supervisor orchestration, or fully simulated scheduler workflow lands
in this phase.

Phase 6 does not by itself satisfy the three Sprint 2 scheduler exit
criteria: synthetic end-to-end plan through the real scheduler,
supervisor-side chaos recovery, and benchmark scheduler throughput.
Those are Phase 7's responsibility. Phase 6 provides the active fake
workers and conformance guarantees Phase 7 consumes.

## 2. Scope

In scope:

- Replace all eleven `fake-*` placeholder binaries with real worker
  processes.
- Build a shared `voom-fake-support` HTTP worker harness for the
  eleven fake providers only.
- Promote all eleven fake providers from `[scaffold]` to active
  entries in `crates/voom-conformance/voom-fakes-manifest.toml`.
- Extend `voom-conformance` so every active worker is tested against
  both generic protocol conformance and its advertised provider
  operation.
- Add process-backed tests for each fake provider's happy path,
  invalid-payload rejection, unsupported-operation rejection, and
  idempotency behavior.
- Preserve the current rule that `chaos-worker` and
  `benchmark-worker` do not use `voom-fake-support`.

Out of scope:

- Real media tooling such as `ffmpeg`, `ffprobe`, or `mkvmerge`.
- Real external system integrations.
- Supervisor DAG or policy-compiler orchestration.
- Durable multi-step workflow execution through the supervisor.
- Performance thresholds beyond the existing benchmark harness.
- Phase 7's scanner → prober → orchestrator → remux/transcode →
  downstream validation workflow through the real scheduler.

Exit criteria:

- `cargo test -p voom-conformance --all-features` runs against
  `echo-worker`, `chaos-worker`, `benchmark-worker`, and all eleven
  fake providers.
- `cargo test -p voom-fakes --all-features` covers every fake
  provider.
- The conformance manifest has no Sprint 2 fake provider remaining
  under `[scaffold]`.
- Every fixed `OperationKind` has at least one active manifest-backed
  conformance case.
- Every `FailureClass` variant has at least one named conformance
  fixture/assertion, and `cargo test -p voom-conformance
  --all-features` fails when the registry is missing a variant,
  contains a duplicate class, or contains an unknown class.
- Phase 7 has enough active fake workers and conformance guarantees to
  build the scanner → prober → orchestrator → remux/transcode →
  downstream validation workflow.
- `just ci` passes.

## 3. Phase 6 To Phase 7 Handoff

The Sprint 2 overview originally placed conformance expansion and
integration validation in Phase 6. This spec narrows Phase 6 to the
worker suite and active-worker conformance foundation. Phase 7 will
own the fully simulated workflow and the direct Sprint 2 scheduler
exit validation.

| Requirement or dependency | Phase 6 / Phase 7 ownership |
|---|---|
| Fake-provider implementation | Phase 6 implements and process-tests all eleven fake providers. |
| Active-worker conformance | Phase 6 promotes all fake providers, `chaos-worker`, and `benchmark-worker` to active conformance manifest entries. |
| Every operation kind from the fixed vocabulary | Phase 6 covers these with manifest-backed primary and secondary fake-provider operations in §4. |
| Every error category from the failure taxonomy | Phase 6 covers these with a mechanically checked `voom-conformance` fixture registry keyed by `FailureClass`. The registry must contain one named fixture/assertion for every variant in the authoritative `voom_core::FailureClass` list, and the conformance test suite must fail on missing, duplicate, or unknown classes. Durable retry and terminal-issue classification remains owned by control-plane/store tests. |
| Synthetic end-to-end plan through the real scheduler | Phase 7 owns the scanner → prober → orchestrator → remux/transcode → downstream validation workflow through the real scheduler. Phase 6 only provides active workers and conformance guarantees. |
| Supervisor-side recovery from each chaos scenario | Phase 7 owns durable control-plane assertions for worker crash, timeout, malformed result, and missed heartbeat. Phase 6 keeps worker-side chaos modes active and conformant where applicable. |
| Benchmark worker reports scheduler throughput | Phase 7 owns scheduler-throughput reporting through the real scheduler path. Phase 6 keeps `benchmark-worker` active and conformant. |
| Registration replay and worker re-registration after crash | Phase 6 covers process-boundary relaunch behavior. Phase 7 owns durable worker-incarnation behavior when exercising the real scheduler/supervisor. |
| Capability mismatch | Phase 6 covers manifest-declared operation mismatch with `UnknownOperation`. Full scheduler capability scoring remains deferred to the scheduler/control-plane phase that introduces multi-worker scoring. |
| Cancellation | Deferred unless Phase 7 explicitly chooses to add cancellation transport. The current worker protocol has no cancel route; lease/job cancellation exists in store/control-plane APIs, not the worker transport. |

## 4. Architecture

`voom-fake-support` becomes the common runtime for the eleven provider
fakes. It wraps `voom_worker_protocol::HttpServer` for canonical
protocol behavior instead of reimplementing the transport. The
protocol crate remains responsible for:

- handshake negotiation;
- protocol version, bearer token, worker id, worker epoch, content
  length, and idempotency validation;
- NDJSON transport behavior.

`voom-fake-support` owns the reusable fake-provider behavior around
that canonical transport:

- process bootstrap from `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`,
  `VOOM_WORKER_EPOCH`, and `VOOM_WORKER_BIND`;
- loopback HTTP server startup and `BOUND addr=...` readiness output;
- stdin EOF shutdown;
- provider dispatch;
- provider payload validation;
- helper builders for progress frames and terminal result frames.

Each fake binary stays small. It declares:

- provider name;
- supported primary and secondary `OperationKind`s;
- payload parser;
- deterministic response builder.

The provider-to-operation mapping is fixed for this phase:

| Binary | Primary operation | Secondary conformance operations |
|---|---|---|
| `fake-scanner` | `ScanLibrary` | none |
| `fake-prober` | `ProbeFile` | `HashFile` |
| `fake-transcoder` | `TranscodeVideo` | `ExtractAudio`, `TranscribeAudio` |
| `fake-remuxer` | `Remux` | none |
| `fake-backup-store` | `BackUpFile` | `DeleteArtifact` |
| `fake-health-checker` | `VerifyArtifact` | none |
| `fake-identity-provider` | `IdentifyMedia` | none |
| `fake-external-system` | `SyncExternalSystem` | none |
| `fake-quality-scorer` | `ScoreQuality` | none |
| `fake-issue-provider` | `CommitArtifact` | none |
| `fake-use-lease-provider` | `EditTracks` | none |

This mapping covers all fifteen fixed `OperationKind` variants:
`ScanLibrary`, `ProbeFile`, `HashFile`, `IdentifyMedia`,
`ScoreQuality`, `SyncExternalSystem`, `BackUpFile`, `Remux`,
`TranscodeVideo`, `EditTracks`, `ExtractAudio`, `TranscribeAudio`,
`VerifyArtifact`, `CommitArtifact`, and `DeleteArtifact`.

`voom-conformance` remains black-box. It may read operation and
payload expectations from the manifest, but it must not depend on
`voom-fake-support` or any fake-provider internals.

`chaos-worker` and `benchmark-worker` remain independent binaries
using their existing worker-specific runtime code. This keeps their
fault and measurement behavior independent from the shared helper used
by the eleven positive-path provider fakes.

## 5. Provider Behavior

Each fake supports one primary operation. The three fakes listed with
secondary conformance operations in §4 also support those secondary
operations so the full fixed operation vocabulary is exercised without
adding more binaries. Every other `OperationKind` is rejected with
`ProtocolError::UnknownOperation`.

Payloads are small JSON objects. Common fields are reused where they
fit:

- `path`: required for file-oriented and artifact-oriented providers.
- `scenario`: optional string, defaulting to `"default"`.
- Provider-specific fields such as `target_codec`, `container`,
  `profile`, `system`, `action`, or `reason` are accepted only by the
  workers that need them.

Each successful operation emits exactly two frames:

1. A progress frame at `seq = 0` with provider name, operation name,
   scenario, and a stable stage.
2. A terminal response frame at `seq = 1` with provider-specific
   result JSON.

Provider-specific terminal payloads are stable:

| Binary | Result payload |
|---|---|
| `fake-scanner` | discovered file list and scan duration |
| `fake-prober` | duration, codecs, dimensions, and container |
| `fake-transcoder` | output path, target codec, and bytes written |
| `fake-remuxer` | output container and retained track count |
| `fake-backup-store` | local and object-store backup ids |
| `fake-health-checker` | `pass`, `degraded`, or `fail` health status |
| `fake-identity-provider` | canonical media id, match confidence, and duplicate evidence |
| `fake-external-system` | simulated system action and refresh status |
| `fake-quality-scorer` | profile name and quality score |
| `fake-issue-provider` | issue key, severity, and priority |
| `fake-use-lease-provider` | lease decision, holder, and reason |

Invalid behavior is deterministic:

- Missing required `path` or provider-required fields return
  `ProtocolError::InvalidPayload`.
- Unsupported provider-specific enum values return
  `ProtocolError::InvalidPayload`.
- Valid payloads sent to unsupported operations return
  `ProtocolError::UnknownOperation`.
- Repeating an idempotency key with the identical request body returns
  the cached response bytes.
- Repeating an idempotency key with a different request body returns
  `ProtocolError::DuplicateIdempotencyKey`.

## 6. Manifest-Driven Conformance

The current conformance suite uses `ProbeFile` as its generic positive
operation. Phase 6 changes that model because most fake providers
support a different operation. Active manifest entries gain operation
case metadata:

```toml
[[binaries]]
name = "fake-scanner"
target = "fake-scanner"
purpose = "phase 3 scanner fake - deterministic library discovery"
status = "active"
required = true

[[binaries.operations]]
operation = "scan_library"
valid_payload = { path = "/library", scenario = "default" }
invalid_payload = { scenario = "missing_path" }
```

The manifest parser validates that every active entry has:

- `name`;
- `target`;
- `status = "active"`;
- `required = true`;
- at least one `operations` case;
- each operation case has `operation`, object-shaped `valid_payload`,
  and object-shaped `invalid_payload`.

The conformance integration test validates global operation coverage:
the union of active manifest operation cases must include every
variant in `OperationKind`. Missing coverage is a manifest validation
failure, not a skipped test.

Scaffold entries remain supported for future non-Sprint-2 binaries,
but the Phase 6 integration test must fail if any of the eleven
Sprint 2 fake providers remains scaffolded.

The typed suite builds positive and negative operation requests from
the manifest operation cases. Generic assertions still cover
handshake, auth, identity, progress ordering, terminal-last,
idempotency, invalid payload handling, and unsupported operation
handling. For `unknown_operation_rejected`, the suite chooses a fixed
operation kind that is not declared in that worker's operation cases.

The raw-wire suite uses the same manifest operation and payload so
workers are tested without being forced to implement `ProbeFile`.

## 7. Failure Taxonomy Conformance

`voom-conformance` must make failure taxonomy coverage mechanically
enforceable. The implementation must add an authoritative iterable
variant list at `voom_core::FailureClass::ALL`, so the conformance
crate can compare its registry against the enum instead of copying the
enum shape into a test.

The conformance fixture registry must contain one entry per
`FailureClass`. Each entry has:

- a stable assertion name;
- the `FailureClass` under test;
- the expected `ErrorCode` from `FailureClass::into_error_code`;
- the expected retry class from `FailureClass::retry_class`;
- a fixture source: fake-provider error frame, chaos-worker scenario,
  or conformance-owned synthetic frame.

The registry test fails when:

- any `FailureClass` variant lacks a fixture;
- any registry entry names a class not present in the authoritative
  variant list;
- any class appears more than once;
- a fixture's error code or retry class disagrees with
  `FailureClass`'s canonical mapping.

This phase owns wire-level classification coverage. Durable lease
retry, issue creation, and terminal-failure persistence remain covered
by control-plane and store tests.

## 8. Tests

`voom-fakes` adds a process-backed integration test for the eleven
provider fakes. For each binary, the test launches the worker with
ordinary Sprint 2 credentials and asserts:

- startup and `BOUND addr=...`;
- clean stdin EOF shutdown;
- supported operation succeeds;
- progress frame includes provider, operation, scenario, and stage;
- terminal result includes the provider-specific stable fields;
- invalid payload fails with `InvalidPayload`;
- unsupported operation fails with `UnknownOperation`;
- idempotent replay with the same body returns the same response;
- idempotent replay with a different body returns
  `DuplicateIdempotencyKey`.
- secondary operation cases for `fake-prober`, `fake-transcoder`, and
  `fake-backup-store` succeed with their declared valid payloads.

`voom-fake-support` keeps sibling unit tests for reusable runtime
pieces that do not need a child process:

- credential loading from environment values;
- request parsing and auth/header validation;
- idempotency cache behavior;
- frame construction;
- provider dispatch helpers.

`voom-conformance` updates its integration test to:

- require `echo-worker`, `chaos-worker`, `benchmark-worker`, and all
  eleven fake providers as active manifest entries;
- reject any Sprint 2 fake provider under `[scaffold]`;
- fail when any fixed `OperationKind` lacks an active manifest-backed
  operation case;
- fail when any `FailureClass` variant lacks a named taxonomy
  fixture/assertion;
- fail when the failure taxonomy registry contains duplicate or
  unknown classes;
- assert each registered failure fixture's `FailureClass`,
  `ErrorCode`, and retry-class mapping;
- run typed and raw-wire suites against every active worker using the
  manifest-declared operation cases and payloads;
- keep the existing conformance-owned protocol-negative fixture checks.

Phase 7 tests should consume the Phase 6 active manifest instead of
hard-coding worker paths. Phase 7 should fail fast if scanner, prober,
remuxer, transcoder, quality, issue, external-system, use-lease,
chaos, or benchmark workers are absent from the active manifest.

Primary verification:

```bash
cargo test -p voom-fake-support --all-features
cargo test -p voom-fakes --all-features
cargo test -p voom-conformance --all-features
just ci
```

## 9. Failure Handling

Worker launch failure, bind timeout, malformed bind line, early exit,
cleanup timeout, protocol error, missing provider fields, unexpected
frame shape, and manifest resolution failure are test failures with
the binary name in the assertion label.

Shared fake-support runtime errors are returned as structured
`ProtocolError` responses whenever the request reaches protocol
handling. Process bootstrap errors before the server binds may fail
the process; tests treat those as launch failures.

The process-backed tests must close stdin and wait for child exit with
a bounded timeout. On timeout, tests kill the child and report cleanup
failure. This mirrors the current chaos, benchmark, and conformance
launch patterns so repeated local test runs do not leave worker
processes behind.

## 10. Implementation Notes

The implementation should proceed provider-support first:

1. Expand `voom-fake-support` into the shared runtime around
   `voom_worker_protocol::HttpServer` and prove it with a small
   test-only provider.
2. Convert a few simple fakes (`fake-prober`, `fake-quality-scorer`,
   `fake-health-checker`) to shake out the runtime API.
3. Convert the remaining fake providers in small batches.
4. Extend the conformance manifest schema and suite request builders.
5. Add secondary operation cases for `HashFile`, `ExtractAudio`,
   `TranscribeAudio`, and `DeleteArtifact`.
6. Add the failure taxonomy fixture registry and coverage test before
   declaring Phase 6 conformance complete.
7. Promote all eleven fake providers to active and enforce the Phase 6
   no-Sprint-2-scaffold and all-operation-coverage rules.

Each commit should keep `cargo test -p voom-fake-support
--all-features`, `cargo test -p voom-fakes --all-features`, or
`cargo test -p voom-conformance --all-features` green for the files it
touches before moving to the next batch.
