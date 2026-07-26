# Issue #331: Attachment and commentary remux design

## Status

Proposed for implementation on `feat/remux-attachment-commentary` from base
`694ced7c5a3b520676c924f4da9170542293516f`.

## Goal

Execute the published attachment and commentary track selectors from authoritative structured
probe facts, preserve every unselected source item, retain at least one source audio track, inspect
the produced inventory, and replan a compliant MKV as `NoOp`.

## Acceptance criteria

1. `keep attachment` and `remove attachment` are supported remux actions.
2. `font` uses only the exact official and legacy Matroska font MIME vocabulary in ADR 0040.
3. `commentary` uses the normalized boolean disposition fact. Missing or malformed facts block the
   affected file.
4. The resolved control-plane keep set starts with every source stream, changes only the action's
   target kind, preserves every video, and rejects a result with no source audio.
5. The mkvtoolnix worker maps ffprobe provider indexes to ordinary tracks and attachments without
   filename, title, codec, or MIME guessing.
6. The worker emits `--attachments <ids>` for selected attachments and `--no-attachments` for none.
7. Output inspection verifies selected ordinary tracks and attachment filename/size/MIME identity
   fingerprints.
8. A generated MKV proves:
   - the main audio, video, subtitle, and font attachment remain;
   - the commentary audio and non-font attachment are absent;
   - default/forced dispositions on unselected retained tracks are preserved;
   - the committed authoritative snapshot contains the expected inventory;
   - the same policy replans the produced artifact as compliant/`NoOp`.
9. A commentary-only single-audio input is rejected by the final-audio guard.
10. Existing compiled policy JSON remains readable and no parser-only DSL form is introduced.

## Scope and exclusions

### In scope

- ffprobe normalization of attachment filename/MIME facts;
- remux snapshot projection and exact filter evaluation;
- planner support and deterministic change detection for attachment/commentary actions;
- control-plane keep-set resolution;
- mkvmerge attachment selection and output inspection;
- focused and generated-media tests;
- ADR 0040 and directly affected remux documentation.

### Excluded

- Filter-addressed defaults and head ordering are owned by #332.
- Language-ranked `defaults ... best` is owned by #336.
- Audio execution and lineage are owned by #99, #337, and #333.
- The deferred forced-operation DSL in ADR 0023 is not needed for #331.
- Filename-extension inference for `font` is deliberately excluded by ADR 0040.
- Unrelated campaign exclusions #346, #343-#344, #351-#353, #334, and #337-#339 remain deferred.

An excluded concern becomes blocking if attachment/commentary correctness depends on it. Otherwise
it must remain out of this PR.

## Current behavior

- `voom-plan::planner::remux::candidate_support` rejects attachment targets and commentary filter
  shapes.
- `stream_facts` carries attachment MIME/filename fields, but the ffprobe normalizer does not
  populate them. It does not carry commentary.
- `evaluate_filter` treats commentary as unsupported and checks `font` with a MIME substring.
- remux planning and control-plane selection reject any source attachment before resolving actions.
- the typed execution-payload reader rejects attachment targets.
- the mkvtoolnix worker represents only `tracks`, rejects attachment references, and always emits
  `--no-attachments`.
- output validation inspects only ordinary mkvmerge tracks.

## Provider evidence

Local conformance against ffprobe 8.1.2 and MKVToolNix 100.0 established:

- ffprobe reports a Matroska attachment as a stream after video/audio with
  `codec_type = "attachment"`, `tags.filename`, `tags.mimetype`, and `extradata_size`;
- mkvmerge reports that file under top-level `attachments` with `id`, `file_name`,
  `content_type`, and `size`;
- `mkvmerge --attachments <id>` preserves the selected attachment;
- `mkvmerge --no-attachments` removes all attachments;
- MKVToolNix canonicalizes the tested legacy TrueType MIME value to `font/ttf`.

The provider model is also documented by:

- FFmpeg's attachment option documentation:
  <https://ffmpeg.org/ffmpeg.html#Main-options>
- mkvmerge's per-input attachment options:
  <https://mkvtoolnix.download/doc/mkvmerge.html>
- the Matroska attachment and font-media-type specification:
  <https://www.matroska.org/technical/attachments.html>

## Design

ADR 0040 is the governing decision. The implementation follows one fact path:

```text
ffprobe JSON
  -> normalized durable stream facts
  -> planner/control-plane filter evaluation
  -> RemuxSelection.keep_streams
  -> mkvmerge source-item mapping
  -> --attachments / --no-attachments
  -> mkvmerge output-item inspection
  -> ffprobe authoritative result snapshot
  -> deterministic NoOp replanning
```

### Snapshot normalization

Extend `insert_stream_tags` with explicit input/output key pairs:

- `filename -> filename`
- `mimetype -> mime_type`

The existing folded-key lookup and sentinel rejection apply. No extension or codec fallback is
added. Raw provider JSON remains available unchanged.

### Planning facts and filters

Add `commentary: Option<bool>` to `SnapshotStreamFact`.

`stream_facts` reads only `disposition.commentary` booleans. A missing or malformed value becomes
`None`; it does not affect filters that do not reference commentary. `evaluate_filter` requires a
known value only when evaluating `TrackFilter::Commentary`.

`TrackFilter::Font`:

1. returns `false` for non-attachments;
2. requires a MIME value for attachments;
3. checks exact membership in ADR 0040's closed allowlist.

Boolean filter composition retains its current behavior: `and` evaluates every child so missing
facts cannot be hidden behind a false earlier child; `or` may return a proven true before an
unknown later child.

### Planner and typed payload

Remove the attachment-target and commentary-shape rejection gates. Keep `TitleMatches` unsupported.
Allow attachment targets in `RemuxOperationPayload::try_from_execution_value`.

Remove the blanket source-attachment block from remux change detection. Attachment actions use the
existing keep/remove functions. Track-order attachment semantics remain blocked and owned by #332.

### Control-plane resolution

Remove the blanket attachment-source and attachment-action blocks. The existing `BTreeSet` keep
algorithm already provides the required semantics:

- begin with every stream ID;
- `keep <target>` replaces only that target kind with its matches;
- `remove <target>` removes only matching IDs;
- re-add all video IDs;
- reject an empty final audio set.

The worker selection contains ordinary tracks and attachments in one ordered `keep_streams` list,
using source provider order.

### Worker mapping and arguments

Replace the track-only identify mapping with an internal source-item mapping. Ordinary tracks keep
their current enumerate-derived provider indexes. Attachments follow them in top-level attachment
array order. Each item holds its mkvmerge ID, kind, default flag where applicable, and a
kind-specific fingerprint.

Attachment fingerprints contain exact `file_name` and `size`. Recognized registered and legacy
font MIME values share a `font` identity; every non-font MIME value remains exact. Ordinary-track
fingerprints remain unchanged.

Argument construction partitions the selected items:

- track item IDs feed the existing video/audio/subtitle and flag options;
- attachment item IDs feed `--attachments`;
- zero selected attachments emits `--no-attachments`;
- attachment IDs never enter `--track-order` or track flags.

Unknown provider indexes and wrong-kind references fail before provider execution.

### Output inspection

The output mapping includes ordinary tracks followed by attachments. Expected output order applies
group ordering to ordinary tracks, appends any remaining ordinary tracks, and then appends selected
attachments in source order.

Validation compares item count, kind, ordinary-track identity, attachment
filename/size/MIME identity, video presence, and default flags. A mismatch is a malformed worker
result and prevents commit.

## Compatibility and migration

- No database migration.
- No dependency addition.
- No compiled-policy schema change.
- No worker-protocol schema change.
- Existing compiled policy versions and remux payloads deserialize unchanged.
- Existing durable snapshots remain readable. A selector requiring a fact absent from an old
  snapshot blocks that file; a new probe supplies the additive normalized fact.
- Rollback is code-only. A reverted binary still reads all stored snapshot JSON because the payload
  is an untyped passthrough surface; it will again reject attachment execution.

## Failure behavior

- Missing/malformed stream ID, index, or kind: insufficient snapshot facts.
- Missing/malformed commentary when referenced: insufficient snapshot facts.
- Missing or non-string attachment MIME when `font` is referenced: insufficient snapshot facts.
  A present string outside the closed font allowlist is deterministically non-font.
- Zero audio after all actions: actionable `CONFIG_INVALID` terminal failure for that file.
- Missing mkvmerge item for a provider index: request/config failure before execution.
- Output item count, kind, fingerprint, order, default, or video mismatch: malformed worker result;
  no commit.
- Provider command failure or timeout: existing external-system failure path.

## Security and observability

No new command text comes from policy or snapshot strings. Attachment IDs are parsed unsigned
integers from mkvmerge identify JSON and rendered as arguments without shell execution. Filenames
and MIME values participate only in comparisons and diagnostics.

Existing remux started/succeeded/failed events and terminal-failure issue creation remain the
observability surface. The durable result's `kept_snapshot_stream_ids` includes kept attachments
because it is derived from `keep_streams`.

## Test strategy

### Unit and contract tests

- ffprobe normalization: filename/MIME lifting, case-folded tag keys, malformed tag rejection.
- remux facts: known commentary true/false, missing/malformed commentary, exact official/legacy
  font MIME matches, non-font false, missing MIME block.
- planner: attachment/commentary actions plan or no-op correctly; unknown facts block; container
  remux with attachments is supported.
- typed payload: attachment action round-trip and malformed-target rejection.
- control plane: attachment/commentary keep sets, unselected preservation, commentary-only final
  audio rejection.
- mkvmerge mapping/arguments: top-level attachments, selected IDs, no-attachment case, missing
  mapping, output fingerprint mismatch, attachment exclusion from track order.

### Generated media

Extend the existing real remux flow fixture with:

- video;
- main English audio;
- English commentary audio with commentary disposition;
- a retained subtitle;
- a MIME-tagged font attachment;
- a non-font attachment.

Execute a policy that removes commentary audio, keeps font attachments, and removes non-font
attachments. Inspect the source snapshot, request/result lineage, produced file via authoritative
result snapshot and mkvmerge identify facts, then regenerate the compliance report and require
`NoOp`.

### Guardrails

- focused package tests for every changed crate;
- the real generated-media remux integration test;
- mutation checks that make commentary unknown evaluate false and font use substring matching;
- `just fmt-check`, `just lint`, and full `just ci`.
