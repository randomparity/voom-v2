---
name: voom-sprint-13-design
description: Sprint 13 design for policy-driven MKV remux and basic track selection through durable tickets and staged artifact commit.
status: draft
date: 2026-05-25
sprint: 13
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-12-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-12-closeout.md
---

# VOOM Sprint 13 - Container Remux And Track Selection V1

## 1. Goal

Sprint 13 makes container and basic track policy operations executable for real
media. A policy containing `container mkv` plus V1 track-selection operations
compiles into typed policy intent, plans to a real `remux` workflow node, runs
through durable tickets, dispatches to an out-of-process MKVToolNix worker,
records a staged artifact, verifies the output, commits it through the Sprint 11
host-owned commit path, and reports stable IDs through CLI JSON envelopes.

This sprint deliberately treats container remux and track selection as one
same-file media mutation. When multiple V1 container or track operations apply
to the same file in the same phase, the planner groups them into one `remux`
node so MKVToolNix produces a single staged output from a single source
snapshot. Sprint 13 does not add audio transcoding, broad container editing,
backup, daemon scheduling, or UI media controls.

## 2. Scope

Sprint 13 delivers:

- Planner support for executable `remux` nodes that combine same-target,
  same-phase `container mkv`, `keep/remove audio|subtitle where ...`,
  `order tracks [...]`, and `defaults audio|subtitle ...` operations.
- Attachment-bearing sources and `keep/remove attachment ...` operations block
  visibly until worker attachment preservation/removal is supported.
- Preservation of typed policy intent in the plan payload; policy text is never
  lowered into command-line arguments.
- Track selector evaluation against durable `MediaSnapshot` stream facts.
- A typed `RemuxRequest` and `RemuxResult` in the worker protocol.
- A bundled out-of-process MKVToolNix worker for local files.
- Runtime discovery/preflight for the local MKVToolNix binaries required by the
  worker.
- Control-plane orchestration that selects and revalidates the source file,
  chooses deterministic per-ticket staging output, dispatches the worker,
  records staged artifact state, verifies the artifact, commits add-only, probes
  the committed result, and records lineage/events.
- CLI golden fixtures for policy compile, plan, execute, and report behavior.
- Closeout evidence tying DSL, planning, worker execution, staging,
  verification, commit, result snapshot, and reporting behavior to repeatable
  tests.

Sprint 13 explicitly does not deliver:

- Audio transcoding, audio extraction, subtitle extraction, OCR, or speech
  transcription.
- Video encode profiles, codec ladders, or hardware acceleration.
- Replace, delete, or archive semantics for original files.
- Backup policy or rollback UX.
- Daemon scheduling loops, remote media transfer, object storage, or UI
  controls.
- Free-form MKVToolNix arguments in policy text.
- Explicit video-track keep/remove policy. Sprint 13 preserves all source video
  streams in source order and blocks policy operations that target `video`.
- Arbitrary stream-ID reordering beyond V1 target-group ordering.
- A durable meaning for `defaults ... best`; that remains blocked until Sprint
  15 quality/profile work defines ranking inputs.

## 3. Architecture

The Sprint 13 real path is:

```text
voom scan --path <file>
  -> FileVersion + FileLocation + MediaSnapshot with stream facts

accepted policy with `container mkv` and V1 track operations
  -> compiled typed operations
  -> planner groups same-file/same-phase mutations
  -> ExecutionPlan node operation_kind = "remux"
  -> compliance execute submits durable workflow ticket
  -> scheduler leases ticket to builtin.mkvtoolnix
  -> MKVToolNix worker writes staged MKV artifact
  -> control plane records artifact_handle + staging artifact_location
  -> verify_artifact worker verifies staged bytes
  -> host commit creates add-only FileVersion + FileLocation
  -> scan/probe records MediaSnapshot for committed result
```

The MKVToolNix worker never writes SQLite and never commits output into managed
media locations. Its only filesystem mutation is writing the requested staging
path. The control plane owns artifact identity, verification, final commit,
lineage, events, and result snapshot persistence.

Sprint 13 reuses the Sprint 12 execution shape instead of introducing a
parallel mutation pipeline. A new focused remux execution module mirrors the
existing transcode boundaries: source selection, staging path selection, worker
dispatch, staged artifact recording, verification, commit, event payloads, and
result snapshot recording. Shared helpers are extracted only when they remove
real duplication between transcode and remux without changing behavior.

## 4. Policy And Planning

The policy compiler already produces typed operations for the V1 policy surface:

```text
container mkv
keep audio where lang in [eng, und]
remove subtitle where forced
remove attachments where not font
order tracks [video, audio, subtitle]
defaults audio: first
defaults subtitle: none
```

Sprint 13 makes those operations executable when they are supported by the V1
track model. The compiler continues to emit the existing typed operations
(`set_container`, `keep_tracks`, `remove_tracks`, `reorder_tracks`, and
`set_defaults`). The planner, not the compiler, groups those compiled operations
into an executable `remux` plan payload:

Attachment-target keep/remove operations are accepted by the policy compiler but
remain blocked at planning/execution binding in Sprint 13; they are not lowered
into executable remux payloads.

```json
{
  "type": "remux",
  "container": "mkv",
  "track_actions": [
    {
      "type": "keep_tracks",
      "target": "audio",
      "filter": {
        "type": "language_in",
        "values": ["eng", "und"]
      }
    }
  ],
  "track_order": ["video", "audio", "subtitle"],
  "defaults": [
    {
      "target": "audio",
      "strategy": "first"
    }
  ]
}
```

The planner builds one remux mutation group per target snapshot per phase. It
collects all supported same-phase container and track operations for that target,
even when non-remux operations such as tag edits appear between them in policy
text. The grouped operations become one `remux` node when at least one operation
would require a mutation. Unsupported same-phase remux operations produce a
blocked node for that operation and are not silently dropped into the executable
group. Phase dependency edges stay unchanged, and operations in different phases
remain separate plan nodes.

Planning behavior:

- `container mkv` alone is no-op when the current container is already MKV.
- `container mkv` plans a `remux` node when the current container is known and
  not MKV.
- Unknown container facts block with `insufficient_snapshot_facts`.
- Track operations plan only when the durable media snapshot has enough stream
  facts to evaluate selectors and defaults.
- Track operations no-op when the selected keep/remove/default/order result
  matches the snapshot.
- Sources with no video stream block with `unsupported_media_shape`.
- `keep/remove video ...` blocks with `unsupported_media_shape`; all source video
  streams are otherwise preserved in source order.
- `defaults audio: first`, `defaults subtitle: first`, `defaults subtitle:
  none`, and `defaults ... preserve` are V1-supported.
- `defaults ... best` blocks with `unsupported_media_shape` until Sprint 15.
- `order tracks [video, audio, subtitle]` orders target groups only; it does
  not expose arbitrary stream-ID ordering.
- Unknown or unsupported track target/filter/default shapes block visibly rather
  than disappearing.

When selector evaluation needs a fact that is absent from the snapshot, the plan
node is blocked instead of guessing. Examples include missing stream type,
language, codec, channel count, default flag, forced flag, title, attachment
MIME/type, or attachment filename facts required by a filter.

## 5. Worker Protocol

Sprint 13 adds typed protocol structs for `remux`:

```json
{
  "input": {
    "path": "/library/input.mp4",
    "expected": {
      "size_bytes": 1234,
      "content_hash": "blake3:...",
      "modified_at": "2026-05-25T00:00:00Z",
      "local_file_key": null
    }
  },
  "output": {
    "staging_root": "/tmp/voom-stage",
    "path": "/tmp/voom-stage/input.remux.mkv",
    "container": "mkv",
    "overwrite": false
  },
  "selection": {
    "keep_streams": [
      {
        "snapshot_stream_id": "stream-0",
        "provider_stream_index": 0
      },
      {
        "snapshot_stream_id": "stream-1",
        "provider_stream_index": 1
      },
      {
        "snapshot_stream_id": "stream-2",
        "provider_stream_index": 2
      }
    ],
    "default_streams": [
      {
        "snapshot_stream_id": "stream-1",
        "provider_stream_index": 1
      }
    ],
    "clear_default_streams": [
      {
        "snapshot_stream_id": "stream-2",
        "provider_stream_index": 2
      }
    ],
    "track_order": ["video", "audio", "subtitle"]
  }
}
```

The result reports provider facts and observed output:

```json
{
  "status": "remuxed",
  "provider": "mkvtoolnix",
  "provider_version": "mkvmerge v...",
  "input_pre": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "input_post": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "output": { "size_bytes": 1200, "content_hash": "blake3:..." },
  "output_container": "mkv",
  "kept_snapshot_stream_ids": ["stream-0", "stream-1", "stream-2"],
  "default_snapshot_stream_ids": ["stream-1"]
}
```

`snapshot_stream_id` is the durable stream identity from the source
`MediaSnapshot`; `provider_stream_index` is the worker-local selector used for
the input file format. The control plane resolves both values immediately before
dispatch from the same re-read snapshot used for execution preconditions. The
worker must echo snapshot IDs in the result, and the control plane validates
those IDs against the request before recording success. Provider-specific track
IDs may be observed for diagnostics, but they are not the durable contract
between planner, control plane, and worker.

The worker must:

- reject unknown operations through the protocol route policy;
- reject malformed payloads before invoking MKVToolNix;
- reject an existing output path because Sprint 13 has no overwrite semantics;
- reject missing or non-canonical `output.staging_root` values and reject output
  paths whose canonical parent is outside that root;
- observe and verify input bytes before and after MKVToolNix;
- invoke MKVToolNix out of process with a deterministic command shape derived
  from typed request fields;
- preserve all source video streams in source order because Sprint 13 has no
  explicit video-track keep/remove policy;
- fail if the request would produce no video stream;
- emit progress frames when the provider exposes useful progress;
- observe output bytes after MKVToolNix exits;
- validate that output facts satisfy MKV container and selected stream/default
  expectations;
- fail loudly for content drift, unavailable input/output, spawn/exit failures,
  timeout, malformed output facts, unsupported provider output, and path escape
  attempts.

The worker may use `mkvmerge --identify` or `ffprobe` internally to validate the
produced output file. That provider-local validation is not durable compliance
state. The committed result still needs a durable `MediaSnapshot` recorded
through the normal scan/probe path.

Worker startup or first use must fail loudly if required MKVToolNix binaries are
missing, not executable, or too old for the fixed V1 command shape. Required CI
tests are not silently skipped; missing binaries are setup failures with
explicit diagnostics.

## 6. Control-Plane Execution

Compliance execution currently bridges planned policy nodes into workflow
tickets. Sprint 13 extends that bridge so planned `remux` nodes with policy
targets use the real control-plane remux path instead of the synthetic provider
payload.

For each remux ticket, the control plane must:

1. Parse the workflow ticket payload and source identity.
2. Require an existing, unretired source `FileVersion`.
3. Require exactly one live local source `FileLocation`, unless the payload
   carries a specific source location ID.
4. Re-read the source media snapshot and require the same media-shape and stream
   facts used by the planner.
5. Re-evaluate track selectors from the typed operation payload at execution
   time.
6. Re-observe source bytes and compare them to the source version facts before
   dispatch.
7. Choose a canonical new staging path under the configured or command-scoped
   staging directory.
8. Dispatch `RemuxRequest` to the bundled MKVToolNix worker.
9. Reject worker success if input pre/post facts drift, output facts are
   missing, selected streams/defaults do not match the request, or output
   container is not MKV.
10. Record a staged artifact handle linked to the source `FileVersion`, with one
   live `artifact_locations.kind = 'staging'` row.
11. Verify the staged artifact through the Sprint 11 verification path.
12. Commit the verified staged artifact to an add-only target path.
13. Probe the committed result through the durable scan/probe path and record a
   `MediaSnapshot` for the result `FileVersion`.
14. Record lineage and events.

Staging path selection must be deterministic for a ticket and lease, not just
for the source file name. The path includes the workflow ticket ID and lease
identity under a canonical staging root so a retry cannot confuse a stale
partial output with the current attempt. If the selected staging path already
exists before dispatch, the control plane fails loudly and includes ticket/path
context for cleanup; it must not silently reuse, delete, truncate, or overwrite
the file in Sprint 13.

The control plane applies the same local path hardening as Sprint 12: canonicalize
the source path, staging parent, and target parent; reject symlink traversal for
source, staging, and target paths; and store canonical path values in durable
records and CLI output.

The first target path is add-only and deterministic, for example
`<source-stem>.remux.mkv` in a caller-provided output directory or policy
execution output root. If the target exists, the operation fails with
`CONFIG_INVALID`; replace semantics remain deferred.

## 7. Events And Reporting

Sprint 13 adds typed event payloads for:

- `artifact.remux_started`
- `artifact.remux_progress`
- `artifact.remux_succeeded`
- `artifact.remux_failed`

These events are audit facts only. Artifact handles, artifact locations,
verification rows, commit records, file versions, file locations, jobs, tickets,
and leases remain the source of truth.

Every remux event payload must include enough correlation data to reconstruct
the ticket attempt without reading provider logs: job ID, ticket ID, lease or
attempt identity, source file version/location IDs, selected snapshot stream
IDs, provider stream indexes used for dispatch, default snapshot stream IDs,
staging path or staged artifact IDs when known, provider name/version when
known, and the failure class and public error code on failure.

CLI reports must expose stable IDs for:

- policy version and input set;
- plan and report;
- job and ticket;
- source file version/location;
- staged artifact handle/location;
- verification row;
- commit record;
- result file version/location;
- committed-result media snapshot, when commit reached that phase.

The command output must continue to emit exactly one JSON envelope on stdout.

## 8. Error Handling

Stable Sprint 13 behavior:

- Unsupported remux or track policy shape: policy validation or planning
  diagnostic.
- Missing source file version or location: `NOT_FOUND`.
- Ambiguous source location: `CONFIG_INVALID`.
- Missing source bytes: `ARTIFACT_UNAVAILABLE`.
- Source drift before or during worker execution:
  `ARTIFACT_CHECKSUM_MISMATCH`.
- Existing staging or target path: `CONFIG_INVALID`.
- Missing stream facts needed by a selector: planning or execution diagnostic,
  reported as `CONFIG_INVALID` at execution time.
- Unsupported V1 track/default/order shape: planning or execution diagnostic,
  reported as `CONFIG_INVALID` at execution time.
- Request would produce no video stream: `CONFIG_INVALID`.
- MKVToolNix spawn/exit failure: `EXTERNAL_SYSTEM_UNAVAILABLE`.
- Worker crash, timeout, malformed result, and protocol errors use the existing
  worker failure taxonomy.
- Output fails verification or commit preconditions:
  `ARTIFACT_CHECKSUM_MISMATCH` or `CONFIG_INVALID` as appropriate.
- Commit failure after filesystem promotion begins must preserve Sprint 11
  `recovery_required` visibility.
- Result media snapshot probe failure after commit must not hide the committed
  result; the error envelope includes result `FileVersion`, `FileLocation`, and
  commit record IDs so an agent can inspect or re-probe.

Silent skips are not allowed. If the control plane records partial durable
state, the error envelope must include enough IDs for an agent to inspect it.

## 9. Testing

Required tests:

- Policy parser/compiler tests for accepted V1 container and track policy
  shapes, including combined same-phase examples.
- Planner tests for grouped `remux` nodes, container-only no-op/planned/blocked
  cases, track keep/remove/default/order no-op/planned/blocked cases, and
  unsupported `defaults ... best`.
- Compliance bridge tests proving planned `remux` policy nodes submit real
  workflow tickets with policy targets and typed remux payloads.
- Worker protocol serialization tests for remux request/result payloads.
- MKVToolNix worker conformance tests for success, missing input, input drift,
  existing output, bad payload, path escape, provider failure, timeout,
  no-video output, selected-stream mismatch, default-track mismatch, and
  non-MKV output facts.
- MKVToolNix preflight tests for missing binaries, non-executable binaries, and
  unsupported version output.
- Control-plane unit tests for source selection, selector re-evaluation,
  staging path selection, retry-safe staging path uniqueness, path
  canonicalization/symlink rejection, unsupported media shapes, artifact
  recording, verification integration, commit integration, result media snapshot
  recording, and event payload correlation.
- Integration tests for scan -> policy plan -> execute -> remux -> verify ->
  commit using small fixture media with multiple audio/subtitle/attachment
  cases.
- CLI insta snapshots for successful execution and representative failures.
- Documentation placeholder scan.
- `just ci`.

Tests must follow the repository layout convention: sibling `*_test.rs` files
for unit tests and integration tests under `crates/*/tests/`.

## 10. Acceptance Criteria

Sprint 13 is complete when:

- A policy containing `container mkv` and supported V1 track operations compiles
  and plans to one grouped `remux` node for each affected file/phase.
- Already-compliant fixture media produces no-op nodes with clear reasons.
- Missing stream/container facts and unsupported V1 shapes block visibly.
- Compliance execution runs planned remux work through durable tickets and an
  out-of-process MKVToolNix worker.
- The worker writes only a staged output and never commits managed media state.
- The control plane records the staged artifact, verifies it, commits it
  add-only, and records the resulting `FileVersion`, `FileLocation`, and
  committed-result `MediaSnapshot`.
- Source drift, output verification failure, selector mismatch, default-track
  mismatch, and commit failure are visible and do not report success.
- Missing MKVToolNix binaries or unsupported provider version fail during worker
  discovery/preflight with explicit diagnostics; required CI tests are not
  skipped.
- Existing staging output, retried leases, symlink/path escape attempts, unknown
  media facts, and no-video outputs fail before destructive or ambiguous
  mutation.
- CLI golden tests lock the agent-facing envelope shape.
- The Sprint 13 closeout matrix records repeatable evidence for DSL, planning,
  execution, progress, verification, commit, result snapshot, and reporting
  behavior.
- `just ci` passes.

## 11. Deferred Work

Deferred to later pre-daemon sprints:

- Sprint 14 audio transcode and extract.
- Sprint 15 named video profile settings and quality profile integration,
  including durable semantics for `defaults ... best`.
- Sprint 16 multi-phase real-media policy workflow completion.
- Sprint 17 backup, sidecar ingest, and real-media CLI closeout.
