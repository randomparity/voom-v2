---
name: voom-sprint-12-design
description: Sprint 12 design for policy-driven FFmpeg video transcode to H.265 MKV through durable tickets and staged artifact commit.
status: draft
date: 2026-05-25
sprint: 12
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-11-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-11-closeout.md
---

# VOOM Sprint 12 - FFmpeg Video Transcode V1

## 1. Goal

Sprint 12 proves the first real policy-driven media mutation. A policy
containing `transcode video to hevc` compiles into a planned
`transcode_video` node, executes through durable workflow tickets, runs an
out-of-process FFmpeg worker, records a staged H.265 MKV artifact, verifies the
output, commits it through the Sprint 11 host-owned commit path, and reports
the result through CLI JSON envelopes.

This sprint deliberately starts with one video operation. It does not try to
complete audio transcode, remux/track selection, named profile settings,
backup, sidecar ingest, daemon scheduling, or UI controls. Those are now
explicitly allocated to later pre-daemon real-media sprints in the architecture
spec.

## 2. Scope

Sprint 12 delivers:

- DSL/compiler support for exactly `transcode video to hevc {}` and the
  equivalent no-body statement form if the existing parser normalizes it.
- A compiled policy operation for video transcode with target codec `hevc`,
  output container `mkv`, and default profile identity.
- Planner support that emits a planned `transcode_video` node when the source
  media snapshot is not already HEVC-in-MKV and emits no-op when it is already
  compliant.
- Compliance execution bridge support for real `transcode_video` workflow
  nodes.
- A typed `TranscodeVideoRequest` and `TranscodeVideoResult` in the worker
  protocol.
- A bundled out-of-process FFmpeg worker that reads a local input path and
  writes a new local staging path.
- Control-plane orchestration that chooses the source file version/location,
  creates a deterministic staging target, dispatches the worker through the
  worker protocol, records an artifact handle/location for the produced bytes,
  verifies the artifact with the Sprint 11 verification worker, and commits it
  to an add-only target path.
- Events and durable records that make each stage inspectable.
- CLI golden fixtures for policy compile/plan/execute/report behavior.
- Closeout evidence tying policy, planning, worker execution, staging,
  verification, commit, and reporting to repeatable tests.

Sprint 12 explicitly does not deliver:

- Audio transcode, audio extract, subtitle processing, or track selection.
- MKVToolNix remux or broad container editing.
- Named video profile settings beyond a fixed default HEVC profile.
- Hardware acceleration, codec ladders, bitrate ladders, or adaptive outputs.
- Replace/delete/archive semantics for original files.
- Backup policy or rollback UX.
- Daemon scheduling loops, remote media transfer, object storage, or UI
  controls.

## 3. Architecture

The Sprint 12 real path is:

```text
voom scan --path <file>
  -> FileVersion + FileLocation + MediaSnapshot

accepted policy with `transcode video to hevc {}`
  -> compiled TranscodeVideo operation
  -> ExecutionPlan node operation_kind = "transcode_video"
  -> compliance execute submits durable workflow ticket
  -> scheduler leases ticket to builtin.ffmpeg
  -> FFmpeg worker writes staged H.265 MKV
  -> control plane records artifact_handle + staging artifact_location
  -> verify_artifact worker verifies staged bytes
  -> host commit creates add-only FileVersion + FileLocation
```

The FFmpeg worker never writes SQLite and never commits output into managed
media locations. Its only filesystem mutation is writing the requested staging
path. The control plane owns artifact identity, verification, final commit,
lineage, and events.

Sprint 12 should reuse the Sprint 11 staged artifact tables and commit
semantics. If implementation discovers a missing repository method, add the
small method to the existing artifact repository instead of introducing a
parallel transcode-output table.

## 4. Policy And Planning

The policy compiler must stop treating `transcode` as a deferred execution
operation for the Sprint 12-supported shape:

```text
transcode video to hevc {}
```

The only accepted Sprint 12 target is HEVC/H.265 video in an MKV container.
Unsupported variants fail validation with stable diagnostics:

- `transcode audio ...` remains deferred to Sprint 14.
- `transcode video to h264|av1|vp9|...` is unsupported in Sprint 12.
- `using profile "<name>"` remains deferred to Sprint 15 except for an
  implementation-internal default profile.
- Free-form FFmpeg arguments are never accepted by policy text.

The compiled operation should preserve typed intent, not an FFmpeg command
line:

```json
{
  "type": "transcode_video",
  "target_codec": "hevc",
  "container": "mkv",
  "profile": "default-hevc"
}
```

The planner maps that compiled operation to `operation_kind =
"transcode_video"`. A snapshot is no-op only when the normalized container is
`mkv` and the normalized video codec is `hevc` or `h265`. Otherwise the node is
planned. Unknown codec or container facts should produce a blocked node with an
insufficient-facts diagnostic; Sprint 12 must not transcode bytes whose source
snapshot cannot identify the current video codec and container.

## 5. Worker Protocol

Sprint 12 adds typed protocol structs:

```json
{
  "input": {
    "path": "/library/input.mkv",
    "expected": {
      "size_bytes": 1234,
      "content_hash": "blake3:...",
      "modified_at": "2026-05-25T00:00:00Z",
      "local_file_key": null
    }
  },
  "output": {
    "path": "/tmp/voom-stage/input.hevc.mkv",
    "container": "mkv",
    "video_codec": "hevc",
    "overwrite": false
  },
  "profile": {
    "name": "default-hevc",
    "encoder": "libx265",
    "crf": 23,
    "preset": "medium"
  }
}
```

The result reports provider facts and observed output:

```json
{
  "status": "transcoded",
  "provider": "ffmpeg",
  "provider_version": "ffmpeg version ...",
  "input_pre": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "input_post": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "output": { "size_bytes": 987, "content_hash": "blake3:..." },
  "output_container": "mkv",
  "output_video_codec": "hevc"
}
```

The worker must:

- reject unknown operations through the protocol route policy;
- reject malformed payloads before invoking FFmpeg;
- reject an existing output path because Sprint 12 has no overwrite semantics;
- observe and verify input bytes before and after FFmpeg;
- invoke FFmpeg out of process with a deterministic command shape;
- emit progress frames derived from FFmpeg progress output when available;
- observe output bytes after FFmpeg exits;
- fail loudly for content drift, unavailable input/output, FFmpeg spawn/exit
  failures, timeout, malformed output facts, and unsupported codec/container
  outcomes.

The worker may use `ffprobe` internally only to validate the produced output
file's codec/container facts. That internal validation is worker-local
provider behavior; durable media snapshots are still owned by the control-plane
scan/probe path.

## 6. Control-Plane Execution

Compliance execution currently bridges planned policy nodes into workflow
tickets. Sprint 12 extends that bridge for `transcode_video` and ensures the
runtime registry can discover or bootstrap the bundled local FFmpeg worker.

For each transcode ticket, the control plane must:

1. Parse the workflow ticket payload and source identity.
2. Require an existing, unretired source `FileVersion`.
3. Require exactly one live local source `FileLocation`, unless the payload
   carries a specific source location ID.
4. Re-observe source bytes and compare them to the source version facts before
   dispatch.
5. Choose a canonical new staging path under the configured or command-scoped
   staging directory.
6. Dispatch `TranscodeVideoRequest` to the bundled FFmpeg worker.
7. Reject worker success if input pre/post facts drift or output facts are
   missing.
8. Record a staged artifact handle linked to the source `FileVersion`, with one
   live `artifact_locations.kind = 'staging'` row.
9. Verify the staged artifact through the Sprint 11 verification path.
10. Commit the verified staged artifact to an add-only target path.
11. Record lineage and events.

The first target path should be add-only and deterministic, for example
`<source-stem>.hevc.mkv` in a caller-provided output directory or policy
execution output root. If the target exists, the operation fails with
`CONFIG_INVALID`; replace semantics remain deferred.

## 7. Events And Reporting

Sprint 12 adds typed event payloads for:

- `artifact.transcode_started`
- `artifact.transcode_progress`
- `artifact.transcode_succeeded`
- `artifact.transcode_failed`

These events are audit facts only. Artifact handles, artifact locations,
verification rows, commit records, file versions, file locations, jobs, tickets,
and leases remain the source of truth.

CLI reports must expose stable IDs for:

- policy version and input set;
- plan and report;
- job and ticket;
- source file version/location;
- staged artifact handle/location;
- verification row;
- commit record;
- result file version/location.

The command output must continue to emit exactly one JSON envelope on stdout.

## 8. Error Handling

Stable Sprint 12 behavior:

- Unsupported transcode policy shape: policy validation error.
- Missing source file version or location: `NOT_FOUND`.
- Ambiguous source location: `CONFIG_INVALID`.
- Missing source bytes: `ARTIFACT_UNAVAILABLE`.
- Source drift before or during worker execution:
  `ARTIFACT_CHECKSUM_MISMATCH`.
- Existing staging or target path: `CONFIG_INVALID`.
- FFmpeg spawn/exit failure: `EXTERNAL_SYSTEM_UNAVAILABLE`.
- Worker crash, timeout, malformed result, and protocol errors use the existing
  worker failure taxonomy.
- Output fails verification or commit preconditions:
  `ARTIFACT_CHECKSUM_MISMATCH` or `CONFIG_INVALID` as appropriate.
- Commit failure after filesystem promotion begins must preserve Sprint 11
  `recovery_required` visibility.

Silent skips are not allowed. If the control plane records partial durable
state, the error envelope must include enough IDs for an agent to inspect it.

## 9. Testing

Required tests:

- Policy parser/validator/compiler tests for accepted
  `transcode video to hevc {}` and rejected unsupported transcode shapes.
- Planner tests for planned, no-op, and blocked transcode nodes.
- Compliance bridge tests proving `transcode_video` planned nodes submit real
  workflow tickets instead of unsupported-execution errors.
- Worker protocol serialization tests for transcode request/result payloads.
- FFmpeg worker conformance tests for success, missing input, input drift,
  existing output, bad payload, FFmpeg failure, and timeout.
- Control-plane unit tests for source selection, staging path selection,
  artifact recording, verification integration, commit integration, and event
  emission.
- Integration tests for scan -> policy plan -> execute -> transcode -> verify
  -> commit using small fixture media.
- CLI insta snapshots for successful execution and representative failures.
- Documentation placeholder scan.
- `just ci`.

Tests must follow the repository layout convention: sibling `*_test.rs` files
for unit tests and integration tests under `crates/*/tests/`.

## 10. Acceptance Criteria

Sprint 12 is complete when:

- A policy containing `transcode video to hevc {}` compiles and plans to a
  `transcode_video` node for non-HEVC or non-MKV fixture media.
- The same policy produces no-op for fixture media already normalized to
  HEVC-in-MKV.
- Compliance execution runs the planned transcode through durable tickets and
  an out-of-process FFmpeg worker.
- The worker writes only a staged output and never commits managed media state.
- The control plane records the staged artifact, verifies it, commits it
  add-only, and records the resulting `FileVersion` and `FileLocation`.
- Source drift, output verification failure, and commit failure are visible and
  do not report success.
- CLI golden tests lock the agent-facing envelope shape.
- The Sprint 12 closeout matrix records repeatable evidence for DSL, planning,
  execution, progress, verification, commit, and reporting behavior.
- `just ci` passes.

## 11. Deferred Work

Deferred to later pre-daemon sprints:

- Sprint 13 container remux and track selection.
- Sprint 14 audio transcode and extract.
- Sprint 15 named video profile settings and quality profile integration.
- Sprint 16 multi-phase real-media policy workflow completion.
- Sprint 17 backup, sidecar ingest, and real-media CLI closeout.
