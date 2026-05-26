---
name: voom-sprint-14-design
description: Sprint 14 design for policy-driven audio transcode and exactly-one audio extraction through durable tickets, FFmpeg workers, staged artifacts, and bundle registration.
status: draft
date: 2026-05-26
sprint: 14
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-12-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-13-design.md
  - https://github.com/randomparity/voom-v2/issues/99
---

# VOOM Sprint 14 - Audio Transcode And Extract V1

## 1. Goal

Sprint 14 makes audio mutation policy operations executable through the real
media control-plane path. A policy containing `transcode audio to aac|opus where
...` compiles into a planned `transcode_audio` node that replaces selected audio
streams inside a new committed media file. A policy containing `extract audio
where ...` compiles into a planned `extract_audio` node that produces one
standalone audio sidecar file and registers that file as a bundle member.

The two operations share FFmpeg, selector evaluation, staged artifact recording,
verification, durable ticket execution, and agent-facing reports. They do not
share commit semantics. Audio transcode is a same-file media successor. Audio
extraction is a sidecar-producing bundle mutation.

Sprint 14 deliberately limits extraction to exactly one selected audio stream.
Multi-output extraction is deferred and tracked by GitHub issue #99.

## 2. Scope

Sprint 14 delivers:

- Compiler support for `transcode audio to aac where ...`, `transcode audio to
  opus where ...`, and `extract audio where ...`.
- Planner support for `transcode_audio` and `extract_audio` nodes using the
  existing track-filter vocabulary.
- Selector evaluation against durable `MediaSnapshot` stream facts, including
  language, codec, channel count, title, commentary, default flag, and stream
  identity when referenced by V1 filters.
- A typed `TranscodeAudioRequest` / `TranscodeAudioResult` worker protocol.
- A typed `ExtractAudioRequest` / `ExtractAudioResult` worker protocol.
- Worker operation vocabulary support for `transcode_audio`, while preserving
  the existing `extract_audio` vocabulary entry.
- Bundled out-of-process FFmpeg worker support for both audio operations.
- Runtime FFmpeg/ffprobe discovery and preflight that validates required
  encoders and muxers for AAC, Opus, MKV media output, and standalone audio
  sidecar output.
- Control-plane orchestration for same-file audio transcode: source selection,
  snapshot re-read, selector re-evaluation, deterministic staging, worker
  dispatch, staged artifact recording, verification, add-only commit, result
  snapshot recording, lineage, events, and CLI report IDs.
- Control-plane orchestration for audio extraction: source selection, snapshot
  re-read, exactly-one selector validation, deterministic staging, worker
  dispatch, staged artifact recording, verification, add-only sidecar commit,
  bundle-member registration, lineage, events, and CLI report IDs.
- A focused sidecar commit helper for verified staged artifacts. Unlike the
  same-file artifact commit path, extraction must create a new sidecar
  `FileAsset` with its own `FileVersion` and `FileLocation`, while preserving
  lineage back to the source `FileVersion` through artifact lineage, artifact
  `source_lineage`, the artifact commit record, and report fields. It must not
  create a `file_versions.produced_from_version_id` edge from the new sidecar
  asset to the source primary-media asset.
- A narrow identity migration and repository update allowing
  `produced_by = 'staged_commit'` with `produced_from_version_id = NULL` for
  sidecar commits. Because SQLite cannot express the needed cross-table
  condition in a `file_versions` CHECK, the sidecar commit repository method
  enforces that every null-parent staged sidecar version is created in the same
  transaction as an `artifact_commit_records.source_file_version_id` edge.
  Same-file staged commits continue to record `produced_from_version_id`.
- A narrow bundle-role migration adding `external_audio` for extracted
  non-commentary audio sidecars. Existing `commentary_audio` remains the role
  for extracted commentary tracks.
- Recovery visibility for extraction failures after filesystem promotion. A
  committed sidecar whose bundle registration fails must be inspectable and
  retryable; it must not disappear behind a generic worker failure.
- Fixture-media integration tests and CLI golden fixtures for successful audio
  transcode, successful audio extraction, and representative failures.
- Sprint 14 closeout evidence tying policy, planning, execution, artifacts,
  bundle registration, verification, and reporting to repeatable tests.

Sprint 14 explicitly does not deliver:

- Multi-output audio extraction. Broad selectors that match multiple streams
  fail visibly unless a future policy surface opts into multi-output behavior.
- Speech-to-text transcription, transcript bundle members, or OCR.
- Named audio profiles, bitrate ladders, loudness normalization, downmix policy,
  channel-layout policy, or quality scoring.
- Free-form FFmpeg arguments in policy text.
- Replace, delete, or archive semantics for original files.
- Backup policy, rollback UX, daemon scheduling, remote media transfer, object
  storage, or UI controls.
- Automatic source-bundle creation during extraction. Sprint 14 requires the
  source primary media asset to already belong to an asset bundle.
- A schema migration for a new `produced_by = 'extract_audio'` value. Sidecar
  lineage uses `staged_commit`, artifact lineage, and commit/report fields
  instead of extending the file-version producer vocabulary.
- Generalized bundle-role policy. Sprint 14 only distinguishes
  `commentary_audio` from `external_audio`.

## 3. Architecture

The audio transcode path is:

```text
voom scan --path <file>
  -> FileVersion + FileLocation + MediaSnapshot with stream facts

accepted policy with `transcode audio to aac|opus where ...`
  -> compiled TranscodeAudio operation
  -> planner evaluates matching audio streams
  -> ExecutionPlan node operation_kind = "transcode_audio"
  -> compliance execute submits durable workflow ticket
  -> scheduler leases ticket to builtin.ffmpeg
  -> FFmpeg worker writes staged media artifact
  -> control plane records artifact_handle + staging artifact_location
  -> verify_artifact worker verifies staged bytes
  -> host commit creates add-only FileVersion + FileLocation
  -> scan/probe records MediaSnapshot for committed result
```

The audio extraction path is:

```text
voom scan --path <file>
  -> primary FileAsset is a member of an AssetBundle
  -> MediaSnapshot includes audio stream facts

accepted policy with `extract audio where ...`
  -> compiled ExtractAudio operation
  -> planner requires exactly one matching audio stream
  -> ExecutionPlan node operation_kind = "extract_audio"
  -> compliance execute submits durable workflow ticket
  -> scheduler leases ticket to builtin.ffmpeg
  -> FFmpeg worker writes one staged standalone audio artifact
  -> control plane records artifact_handle + staging artifact_location
  -> verify_artifact worker verifies staged bytes
  -> host commit creates add-only sidecar FileAsset/FileVersion/FileLocation
  -> control plane adds the new FileAsset to the source AssetBundle
```

The FFmpeg worker never writes SQLite and never registers bundle membership. Its
only filesystem mutation is writing the requested staging path. The control
plane owns artifact identity, verification, final commit, result snapshot
persistence, bundle membership, lineage, events, and reports.

Sprint 14 should reuse the Sprint 12 transcode and Sprint 13 remux boundaries.
Shared helpers are extracted only when they remove real duplication around
source selection, staging path hardening, selected stream resolution, or worker
result validation.

The worker protocol operation-kind vocabulary must include `transcode_audio`.
`extract_audio` already exists in the architectural fixed vocabulary and should
be wired to real execution rather than treated as a synthetic or unsupported
operation. Any database tables that persist operation names must accept both
strings before the policy bridge can submit durable tickets for them.

Audio extraction creates a new sidecar `FileAsset`, so its `FileVersion` cannot
use `produced_from_version_id` to point at the source media version: the current
identity repository enforces same-asset parentage for non-ingest versions. The
sidecar commit helper therefore records the sidecar file version as
`produced_by = 'staged_commit'` with `produced_from_version_id = NULL`, and
stores the cross-asset relationship in artifact lineage, artifact
`source_lineage`, `artifact_commit_records.source_file_version_id`, and the
execution report. The migration/repo change is intentionally narrow:
same-file staged commits still require `produced_from_version_id` at the
repository boundary, and no other producer gains cross-asset parent semantics.
Tests must cover both enforcement layers: the schema admits the sidecar shape,
and the repository rejects null-parent `staged_commit` versions outside the
sidecar commit path.

## 4. Policy And Planning

The compiler adds typed operations:

```json
{
  "type": "transcode_audio",
  "target_codec": "aac",
  "container": "mkv",
  "filter": {
    "type": "language_in",
    "values": ["eng"]
  }
}
```

```json
{
  "type": "extract_audio",
  "target_codec": "opus",
  "container": "ogg",
  "filter": {
    "type": "commentary"
  }
}
```

The concrete V1 policy text is:

```text
transcode audio to aac where lang in [eng, und]
transcode audio to opus where lang in [jpn]
extract audio where commentary
```

Planning behavior for `transcode_audio`:

- Plans when one or more audio streams match the selector and at least one
  matched stream is not already in the requested codec.
- No-ops when every matched stream is already in the requested codec and the
  output container requirement is already satisfied.
- Blocks when zero audio streams match.
- Blocks when required stream facts are absent.
- Blocks when the source has no video stream; same-file mutation remains a
  primary media output in Sprint 14.
- Copies all video, subtitle, attachment, metadata, and non-selected audio
  streams according to the fixed worker command shape.
- Replaces only matched audio streams with newly encoded streams.
- Preserves matched audio stream language, title, disposition/default flag, and
  stream ordering unless the source lacks those facts. Missing preservation facts
  block planning or execution instead of silently dropping metadata.
- Preserves the selected stream count exactly. The worker result must prove one
  output audio stream for each selected input stream, and the control plane must
  reject missing, extra, or reordered selected outputs.

Planning behavior for `extract_audio`:

- Plans only when exactly one audio stream matches the selector.
- Blocks when zero audio streams match.
- Blocks when more than one audio stream matches. Multi-output extraction is
  deferred to issue #99.
- Blocks when required stream facts are absent.
- Uses a deterministic V1 sidecar format: Opus audio in an Ogg container.
- Preserves selected audio language and title in the sidecar when present in the
  source snapshot. Missing language/title facts do not block extraction unless
  the policy selector needs them, but present facts must not be discarded
  silently.
- Assigns the bundle role deterministically from selected stream facts:
  `commentary_audio` when the selected stream is known commentary and
  `external_audio` when it is known non-commentary. Unknown commentary state
  blocks planning as insufficient facts so Sprint 14 cannot mislabel an
  extracted sidecar.

Unsupported codec names, custom profiles, free-form FFmpeg options, subtitle
extraction, transcript creation, and multi-output sidecar policy are validation
or planning errors. Silent skips are not allowed.

## 5. Worker Protocol

Sprint 14 adds typed protocol structs for audio transcode.

Request shape:

```json
{
  "input": {
    "path": "/library/input.mkv",
    "expected": {
      "size_bytes": 1234,
      "content_hash": "blake3:...",
      "modified_at": "2026-05-26T00:00:00Z",
      "local_file_key": null
    }
  },
  "output": {
    "staging_root": "/tmp/voom-stage",
    "path": "/tmp/voom-stage/ticket-1/lease-1/input.audio-opus.mkv",
    "container": "mkv",
    "overwrite": false
  },
  "selection": {
    "selected_streams": [
      {
        "snapshot_stream_id": "stream-1",
        "provider_stream_index": 1
      }
    ]
  },
  "audio": {
    "target_codec": "opus",
    "profile": "default-opus"
  }
}
```

Result shape:

```json
{
  "status": "transcoded",
  "provider": "ffmpeg",
  "provider_version": "ffmpeg version ...",
  "input_pre": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "input_post": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "output": { "size_bytes": 1100, "content_hash": "blake3:..." },
  "output_container": "mkv",
  "selected_snapshot_stream_ids": ["stream-1"],
  "output_audio_codecs": ["opus"]
}
```

The audio transcode result must also report the selected output stream facts in
request order: snapshot stream ID, output provider stream index, codec,
language, title, default/disposition state, and channel count when ffprobe
reports it. The control plane compares those facts to the request and source
snapshot before committing.

Sprint 14 also adds typed protocol structs for exactly-one audio extraction.

Request shape:

```json
{
  "input": {
    "path": "/library/input.mkv",
    "expected": {
      "size_bytes": 1234,
      "content_hash": "blake3:...",
      "modified_at": "2026-05-26T00:00:00Z",
      "local_file_key": null
    }
  },
  "output": {
    "staging_root": "/tmp/voom-stage",
    "path": "/tmp/voom-stage/ticket-2/lease-1/input.commentary.opus.ogg",
    "container": "ogg",
    "audio_codec": "opus",
    "overwrite": false
  },
  "selection": {
    "snapshot_stream_id": "stream-3",
    "provider_stream_index": 3
  }
}
```

Result shape:

```json
{
  "status": "extracted",
  "provider": "ffmpeg",
  "provider_version": "ffmpeg version ...",
  "input_pre": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "input_post": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "output": { "size_bytes": 321, "content_hash": "blake3:..." },
  "output_container": "ogg",
  "output_audio_codec": "opus",
  "selected_snapshot_stream_id": "stream-3",
  "output_language": "eng",
  "output_title": "Commentary"
}
```

For both operations, `snapshot_stream_id` is the durable stream identity from the
source `MediaSnapshot`, and `provider_stream_index` is the worker-local selector
used for FFmpeg mapping. The control plane resolves both values immediately
before dispatch from the same re-read snapshot used for execution preconditions.
The worker echoes snapshot IDs in the result, and the control plane validates
them against the request before recording success.

The worker must:

- reject unknown operations through the protocol route policy;
- reject malformed payloads before invoking FFmpeg;
- reject existing output paths because Sprint 14 has no overwrite semantics;
- reject missing or non-canonical `output.staging_root` values and reject output
  paths whose canonical parent is outside that root;
- observe and verify input bytes before and after FFmpeg;
- invoke FFmpeg out of process with deterministic command shapes derived from
  typed request fields;
- copy non-selected streams explicitly in the audio transcode command shape;
- map exactly one stream in the extraction command shape;
- emit progress frames when FFmpeg exposes useful progress;
- observe output bytes after FFmpeg exits;
- validate output codec/container facts with ffprobe before returning success;
- validate requested language/title/disposition preservation where the operation
  requires it;
- fail loudly for content drift, unavailable input/output, spawn/exit failures,
  timeout, malformed output facts, unsupported provider output, and path escape
  attempts.

Worker startup or first use must fail loudly if FFmpeg/ffprobe are missing, not
executable, or unable to satisfy the fixed V1 command shapes. Required tests are
not silently skipped; missing media tools are setup failures with explicit
diagnostics.

## 6. Control-Plane Execution

Compliance execution extends the policy bridge so planned `transcode_audio` and
`extract_audio` nodes with policy targets submit real workflow tickets.

For each audio transcode ticket, the control plane must:

1. Parse the workflow ticket payload and source identity.
2. Require an existing, unretired source `FileVersion`.
3. Require exactly one live local source `FileLocation`, unless the payload
   carries a specific source location ID.
4. Re-read the source media snapshot.
5. Re-evaluate the audio selector and require one or more selected streams.
6. Re-observe source bytes and compare them to the source version facts before
   dispatch.
7. Choose a canonical new staging path under the configured or command-scoped
   staging directory.
8. Dispatch `TranscodeAudioRequest` to the bundled FFmpeg worker.
9. Reject worker success if input pre/post facts drift, output facts are
   missing, selected stream IDs do not match the request, or output codec facts
   do not satisfy the requested audio codec.
10. Record a staged artifact handle linked to the source `FileVersion`, with one
    live `artifact_locations.kind = 'staging'` row.
11. Verify the staged artifact through the Sprint 11 verification path.
12. Commit the verified staged artifact to an add-only target path.
13. Probe the committed result through the durable scan/probe path and record a
    `MediaSnapshot` for the result `FileVersion`.
14. Reconcile the committed-result snapshot with the requested preservation
    rules: selected audio stream count, codec, language/title/disposition, and
    ordering must match the request.
15. Record lineage and events.

For each audio extraction ticket, the control plane must:

1. Parse the workflow ticket payload and source identity.
2. Require an existing, unretired source `FileVersion`.
3. Require exactly one live local source `FileLocation`, unless the payload
   carries a specific source location ID.
4. Resolve the source `FileAsset` bundle membership and require
   `BundleMemberRole::PrimaryVideo`.
5. Re-read the source media snapshot.
6. Re-evaluate the audio selector and require exactly one selected stream.
7. Re-observe source bytes and compare them to the source version facts before
   dispatch.
8. Choose a canonical new staging path under the configured or command-scoped
   staging directory.
9. Dispatch `ExtractAudioRequest` to the bundled FFmpeg worker.
10. Reject worker success if input pre/post facts drift, output facts are
    missing, selected stream ID does not match the request, or output
    codec/container facts do not satisfy the request.
11. Record a staged artifact handle linked to the source `FileVersion`, with one
    live `artifact_locations.kind = 'staging'` row.
12. Verify the staged artifact through the Sprint 11 verification path.
13. Commit the verified staged artifact to an add-only sidecar target path,
    creating a new sidecar `FileAsset`, `FileVersion`, and `FileLocation`.
14. Add the new sidecar `FileAsset` to the source bundle as
    `BundleMemberRole::CommentaryAudio` or `BundleMemberRole::ExternalAudio`
    according to the selected stream facts.
15. If commit succeeds but bundle-member insertion fails, return a recovery
    report containing the commit record ID, sidecar file asset/version/location
    IDs, source bundle ID, failed role, and public error code. The committed
    sidecar must remain discoverable so a later repair command or retry can add
    the bundle membership without re-running FFmpeg.
16. Record lineage and events.

The extraction bundle role is intentionally limited to `commentary_audio` and
`external_audio` in V1. The role is derived from durable selected-stream facts,
not filenames or track titles. If a future policy needs richer extracted audio
roles, it should add a typed role field and validation.

Staging path selection must be deterministic for a ticket and lease. The path
includes the workflow ticket ID and lease identity under a canonical staging
root. If the selected staging path already exists before dispatch, the control
plane fails loudly and includes ticket/path context for cleanup; it must not
silently reuse, delete, truncate, or overwrite the file.

Target paths are add-only and deterministic:

- audio transcode: `<source-stem>.audio-<codec>.mkv`
- audio extraction: `<source-stem>.<snapshot-stream-id>.<codec>.<extension>`

If the target exists, the operation fails with `CONFIG_INVALID`; replace
semantics remain deferred.

The extraction target name must use a sanitized durable `snapshot_stream_id`.
It must not use track title, language, or provider-specific stream index as the
unique filename component. Human-readable stream metadata belongs in result
payloads and CLI reports, not in path identity.

The control plane applies the same local path hardening as Sprint 13:
canonicalize source paths, staging parents, and target parents; reject symlink
traversal; and store canonical path values in durable records and CLI output.

## 7. Events And Reporting

Sprint 14 adds typed event payloads for:

- `artifact.audio_transcode_started`
- `artifact.audio_transcode_progress`
- `artifact.audio_transcode_succeeded`
- `artifact.audio_transcode_failed`
- `artifact.audio_extract_started`
- `artifact.audio_extract_progress`
- `artifact.audio_extract_succeeded`
- `artifact.audio_extract_failed`

These events are audit facts only. Artifact handles, artifact locations,
verification rows, commit records, file assets, file versions, file locations,
bundle memberships, jobs, tickets, and leases remain the source of truth.

Every event payload must include enough correlation data to reconstruct the
ticket attempt without reading provider logs: job ID, ticket ID, lease or
attempt identity, source file version/location IDs, selected snapshot stream
IDs, provider stream indexes, staging path or staged artifact IDs when known,
provider name/version when known, and failure class/public error code on
failure.

Success and failure payloads for audio transcode must include requested and
observed selected-output stream facts so operators can audit language, title,
disposition, channel count, codec, and ordering preservation without opening
provider logs.

Audio extraction success reports must also expose:

- source bundle ID;
- source primary file asset ID;
- extracted sidecar file asset ID;
- extracted sidecar file version/location IDs;
- bundle member row ID and role.

CLI reports for both operations must expose stable IDs for policy version,
input set, plan/report, job/ticket, source file version/location, staged
artifact handle/location, verification row, commit record, and produced
file/version/location state. The command output must continue to emit exactly
one JSON envelope on stdout.

## 8. Error Handling

Stable Sprint 14 behavior:

- Unsupported audio policy shape: policy validation error.
- Missing source file version or location: `NOT_FOUND`.
- Ambiguous source location: `CONFIG_INVALID`.
- Missing source bytes: `ARTIFACT_UNAVAILABLE`.
- Source drift before or during worker execution:
  `ARTIFACT_CHECKSUM_MISMATCH`.
- Existing staging or target path: `CONFIG_INVALID`.
- Missing stream facts needed by a selector: planning or execution diagnostic,
  reported as `CONFIG_INVALID` at execution time.
- Audio transcode selector matches zero streams: planning or execution
  diagnostic, reported as `CONFIG_INVALID` at execution time.
- Audio extraction selector matches zero or multiple streams: planning or
  execution diagnostic, reported as `CONFIG_INVALID` at execution time.
- Extraction source asset is not a primary bundle member: `CONFIG_INVALID`.
- FFmpeg spawn/exit failure: `EXTERNAL_SYSTEM_UNAVAILABLE`.
- Worker crash, timeout, malformed result, and protocol errors use the existing
  worker failure taxonomy.
- Output fails verification or commit preconditions:
  `ARTIFACT_CHECKSUM_MISMATCH` or `CONFIG_INVALID` as appropriate.
- Bundle member registration conflicts: `CONFLICT`.
- Missing commentary/non-commentary facts needed to assign an extraction bundle
  role: planning or execution diagnostic, reported as `CONFIG_INVALID` at
  execution time.
- Commit failure after filesystem promotion begins must preserve Sprint 11
  `recovery_required` visibility.
- Extraction bundle registration failure after filesystem promotion must return
  a recovery report with enough sidecar and bundle IDs to complete registration
  without rerunning FFmpeg.
- Result media snapshot probe failure after audio transcode commit must not hide
  the committed result; the error envelope includes result `FileVersion`,
  `FileLocation`, and commit record IDs so an agent can inspect or re-probe.

Silent skips are not allowed. If the control plane records partial durable
state, the error envelope must include enough IDs for an agent to inspect it.

## 9. Testing

Required tests:

- Policy parser/validator/compiler tests for accepted audio transcode/extract
  shapes and rejected unsupported shapes.
- Planner tests for audio transcode planned/no-op/blocked cases, including zero
  selector matches and missing facts.
- Planner tests for audio extraction exactly-one planned cases and zero/multiple
  selector blocked cases, plus commentary and non-commentary bundle-role
  selection.
- Compliance bridge tests proving `transcode_audio` and `extract_audio` planned
  nodes submit real workflow tickets with policy targets and typed payloads.
- Worker protocol serialization tests for audio transcode and extract
  request/result payloads.
- Worker operation-kind tests proving `transcode_audio` and `extract_audio`
  serialize to stable snake_case names and can be routed by the durable workflow
  executor.
- FFmpeg worker conformance tests for success, missing input, input drift,
  existing output, bad payload, path escape, provider failure, timeout, selected
  stream mismatch, output codec mismatch, output container mismatch, language or
  title preservation mismatch, default/disposition mismatch, and selected output
  ordering mismatch.
- FFmpeg/ffprobe preflight tests for missing binaries, non-executable binaries,
  missing AAC/Opus encoder support, and missing muxer support.
- Control-plane unit tests for source selection, selector re-evaluation,
  staging path selection, retry-safe staging path uniqueness, path
  canonicalization/symlink rejection, artifact recording, verification
  integration, same-file commit integration, extraction sidecar commit
  integration, bundle membership registration, post-commit bundle-registration
  recovery reporting, `external_audio` role migration coverage, and event
  payload correlation.
- Stage/target-path tests proving extraction target names use sanitized durable
  stream IDs and ignore unsafe title/language/provider-index metadata.
- Identity repository tests proving null-parent `staged_commit` file versions
  are accepted only through the sidecar commit path and same-file staged commits
  still require `produced_from_version_id`.
- Integration tests for scan -> policy plan -> execute -> audio transcode ->
  verify -> commit -> result snapshot using small fixture media.
- Integration tests for scan -> policy plan -> execute -> audio extract ->
  verify -> sidecar commit -> bundle member registration using small fixture
  media with a commentary or otherwise selectable audio stream.
- CLI insta snapshots for successful execution and representative failures.
- Documentation placeholder scan.
- `just ci`.

Tests must follow the repository layout convention: sibling `*_test.rs` files
for unit tests and integration tests under `crates/*/tests/`.

## 10. Acceptance Criteria

Sprint 14 is complete when:

- A policy containing supported audio transcode text compiles and plans to
  `transcode_audio` for non-compliant selected audio streams.
- Already-compliant selected audio streams produce no-op nodes with clear
  reasons.
- Audio transcode selectors that match zero streams block visibly.
- A policy containing supported audio extraction text compiles and plans to
  `extract_audio` only when exactly one audio stream matches.
- Audio extraction selectors that match zero or multiple streams block visibly.
- Compliance execution runs planned audio transcode through durable tickets and
  an out-of-process FFmpeg worker.
- Compliance execution runs planned audio extraction through durable tickets and
  an out-of-process FFmpeg worker.
- Workers write only staged outputs and never commit managed media state.
- Audio transcode records a staged artifact, verifies it, commits it add-only,
  and records the resulting `FileVersion`, `FileLocation`, and committed-result
  `MediaSnapshot`.
- Audio transcode preserves selected stream language, title, disposition/default
  state, channel count when known, and request-order mapping, and rejects worker
  or probe results that lose those facts.
- Audio extraction records a staged artifact, verifies it, commits it add-only
  as a sidecar file asset/version/location, and adds that sidecar asset to the
  source bundle as `commentary_audio` or `external_audio`.
- Audio extraction commit is recovery-visible if the sidecar file commits but
  bundle membership insertion fails.
- Source drift, output verification failure, selected-stream mismatch, output
  codec/container mismatch, commit failure, and bundle membership conflicts are
  visible and do not report success.
- Missing FFmpeg/ffprobe binaries or unsupported provider capabilities fail
  during worker discovery/preflight with explicit diagnostics; required CI tests
  are not skipped.
- Existing staging output, retried leases, symlink/path escape attempts,
  unknown media facts, and extraction multi-match selectors fail before
  destructive or ambiguous mutation.
- CLI golden tests lock the agent-facing envelope shape.
- The Sprint 14 closeout matrix records repeatable evidence for DSL, planning,
  execution, artifact, bundle, verification, and reporting behavior.
- `just ci` passes.

## 11. Deferred Work

Deferred to later pre-daemon sprints:

- Multi-output audio extraction policy and execution, tracked by issue #99.
- Named audio profiles, loudness normalization, downmix/channel-layout policy,
  and audio quality scoring.
- Speech-to-text transcription and transcript bundle members.
- Generalized sidecar bundle-role policy beyond `commentary_audio` and
  `external_audio`.
- Sprint 15 named video profile settings and quality profile integration.
- Sprint 16 multi-phase real-media policy workflow completion.
- Sprint 17 backup, sidecar ingest, and real-media CLI closeout.
