# Issue #333 — deterministic audio synthesis and stream lineage

Base: `main` at `c508fd05aef39dffac848213133369f6aba2c611`.

Status: design for implementation.

Governing decisions:

- ADR 0013: strict additive durable payload evolution.
- ADR 0026: published synthesis grammar and add-track mode on
  `transcode_audio`.
- ADR 0043: stable descriptors, operation ledger, atomic lineage finalization.

## Outcome

For every audio stream selected by a published `synthesize audio` plan, execute
one add-track worker operation that preserves the source container streams and
appends one lower-channel companion. Publish the one result file, its media
snapshot, and every source-stream-to-companion relationship as one recoverable
operation.

Success means:

- plan order defines companion order;
- operation and companion identities survive retry and resume;
- the worker receives `add_track = true` and the planned target channels;
- a missing, extra, reordered, or malformed companion prevents publication;
- the committed snapshot contains each source and exactly one expected
  companion with verified codec, channels, language, title, and disposition;
- all lineage rows appear atomically with the result snapshot;
- retry and recovery return the same file, artifact, snapshot, companion, and
  lineage identities; and
- execution/compliance reporting exposes the complete ordered relationships.

## Dependencies and exclusions

Dependencies already merged:

- #276 / ADR 0026: compiler, planner shape, worker wire mode, ffmpeg add-track
  implementation, and worker verifier.
- #99 / ADR 0041: stable descriptor patterns and source-stream ordering.
- #337 / ADR 0042: claim/generation, add-only promotion, atomic relational
  finalization, recovery evidence, and strict ordered reporting patterns.

Explicit exclusions:

- no new parser or DSL production;
- no new `OperationKind` or worker capability;
- no execution-safety campaign work from #334, #343-#344, #346, #351-#353,
  #358-#359, #364, or #338-#339;
- no generalized worker-session reuse (#367), expected-aware generic copy
  primitive (#368), or claim-query optimization (#369);
- no generic media-operation framework; extraction stays unchanged; and
- no automatic deletion of quarantined old-generation staging evidence.

An excluded concern becomes blocking only if synthesis correctness depends on
it or this change worsens it. Independent discoveries become native #325
children and are not enqueued in this campaign.

## Current gaps

The published planner emits:

```json
{
  "type": "synthesize_audio",
  "target_codec": "aac",
  "container": "mkv",
  "target_channels": 2,
  "source_media_snapshot_id": 42,
  "filter": {"type": "channels", "op": "gte", "value": 6}
}
```

It does not identify the operation or companions. Workflow binding currently
requires `AudioOperationType::TranscodeAudio`, runtime selection rejects
`SynthesizeAudio`, and the request builder hardcodes replacement mode.

The generic replacement-transcode path records its result snapshot after the
artifact commit. It has no synthesis operation state, stable companion set, or
recovery path for the committed-file-before-lineage boundary.

## Stable planning contract

### Operation identity

Before planning synthesis, the planner computes the stable node ID using the
same `node_id(phase, ordinal, operation_kind, target)` mechanism used by
extraction. Although synthesis rides `PlanOperationKind::TranscodeAudio`, its
ordinal makes it distinct from any other operation in the phase.

The plan payload adds:

```json
{
  "operation_id": "node_...",
  "companions": [
    {
      "companion_id": "synth_companion_0123456789abcdef",
      "source_snapshot_stream_id": "stream-1",
      "source_provider_stream_index": 1,
      "result_snapshot_stream_id": "synth_companion_0123456789abcdef"
    }
  ]
}
```

`companion_id` and `result_snapshot_stream_id` intentionally match. The ID is:

```text
blake3(
  "voom.synthesize_audio.companion.v1"
  + NUL + operation_id
  + NUL + source_snapshot_stream_id
)
```

rendered as `synth_companion_` plus the first 16 lowercase hex digits.

The descriptor list follows `selected_audio_streams` order. Source provider
index is pinned as correlation evidence but never defines semantic ordering.
Zero selections remain a planning block.

### Compatibility

`operation_id` and `companions` are required only for
`synthesize_audio`. Existing `transcode_audio` and `extract_audio` payloads keep
their current shapes. No historical synthesis execution payload exists.
Compiled policy versions retain the published ADR 0026 operation shape and
remain readable; descriptor generation occurs when that policy is planned.

The typed payload rejects:

- synthesis without operation ID or companions;
- empty, duplicate, or reordered companion descriptors;
- companion fields on replacement transcode/extraction;
- duplicate source or result stream identities; and
- nonpositive target channels.

## Runtime selection and worker contract

Runtime re-reads the pinned source media snapshot, reevaluates the published
filter, and recomputes the expected ordered descriptors. Any mismatch with the
payload is `CONFIG_INVALID` before staging.

The selection plan carries:

```text
source SnapshotAudioStreamFact
source AudioStreamRef
companion_id/result_snapshot_stream_id
target codec/container/channels
operation_id
```

The worker request contains one selection per descriptor:

```text
snapshot_stream_id = descriptor.result_snapshot_stream_id
provider_stream_index = descriptor.source_provider_stream_index
add_track = true
target_channels = payload.target_channels
```

The provider index chooses the source bytes. The result stream ID identifies
the new companion and is written into its metadata by the worker.

### Complete result validation

Before any artifact row is bound:

1. input pre/post facts equal the pinned source version;
2. result selected IDs exactly equal ordered companion IDs;
3. selected output count and order equal descriptors;
4. every output codec equals `target_codec`;
5. every output channel count equals `target_channels`;
6. language, title, default, forced, and commentary facts equal its source;
7. the output container is `mkv`; and
8. the exact staging file matches reported size/hash.

The precommit ffprobe snapshot is normalized and then checked independently:

- every worker-reported result provider index identifies exactly one probed
  audio stream with the validated companion facts;
- every selected source remains present with its source codec, channels,
  language, title, and disposition facts; and
- the total selected-source/companion mapping is complete.

Normalized ffprobe payloads derive `id` as `stream-{provider_index}` and do not
retain the worker's private `snapshot_stream_id` tag. Once the result's planned
IDs, unique provider indexes, and all companion facts have been independently
matched to the exact normalized probe, the control plane replaces each
companion stream's derived ID with its planned companion ID. It rejects an
occupied ID, a duplicate index, a non-audio stream, or any ambiguous match.
Source and unrelated stream IDs remain the normalized provider-index IDs.

The worker result remains untrusted. No missing fact is synthesized merely to
make validation pass. Validated worker metadata may enrich only the exact
companion provider indexes already proven by the probe.

## Durable model

### `audio_synthesis_operations`

One row per semantic execution:

- unique `operation_key`;
- planned operation ID;
- source file version and source media snapshot;
- codec/container/target channels;
- canonical staging and target paths;
- state: `planned`, `staged`, `prepared`, `recovery_required`, `committed`;
- dispatch generation;
- live claim lease/token/expiry;
- staged artifact handle/location, verification, commit, result
  file/version/location/snapshot references;
- precommit probe worker/payload;
- observed file facts and recovery diagnostic; and
- timestamps.

The operation key is a SHA-256 domain hash over planned operation ID, source
version/snapshot, ordered descriptors, codec/container/channels, and canonical
target path. Exact stored fields are compared on replay; the hash alone is
never trusted.

### `audio_synthesis_companions`

One ordered row per descriptor:

- operation ID and unique ordinal;
- companion/result snapshot stream ID;
- source snapshot stream ID/provider index;
- validated result provider index;
- codec/channels/language/title/default/forced/commentary;
- lineage row ID after finalization; and
- unique result stream identity within the operation.

### Dispatch attempts

One attempt row per operation generation records worker ID/epoch, idempotency
key, exact attempt directory/path, and terminal/quarantined status. The key is:

```text
audio-synthesize:<operation-key>:<generation>
```

The attempt and path are committed before send.

### `audio_synthesis_stream_lineage`

One row per companion, unique by companion row and by result
snapshot/result-stream pair:

- source file version;
- source media snapshot;
- source snapshot stream ID/provider index;
- result file version;
- result media snapshot;
- result snapshot stream ID/provider index;
- codec/channels/language/title/default/forced/commentary; and
- recorded timestamp.

The finalize repository constructs lineage from the operation and validated
companion rows. Callers cannot insert arbitrary relationships.

## State machine and fencing

### Resolve or create

1. Resolve source, pinned snapshot, ordered descriptors, and canonical target.
2. Compute the operation key.
3. Load or insert the planned operation and companions.
4. Compare every stored semantic field on replay.
5. Return a committed report, resume a staged/prepared/recovery row, or acquire
   the planned writer claim.

Every noncommitted transition predicates on operation ID, expected state,
generation, claim token, and live expiry. Workflow lease heartbeats renew the
operation claim. Claim loss stops the former actor before further mutation.

### Dispatch and stage

The first generation writes beneath a private operation/generation directory.
The attempt row and exact path are durable before send.

- Same live worker epoch after restart: replay the exact key/path.
- Terminal response: validate and bind only if generation/claim still match.
- Worker epoch unavailable: quarantine the attempt, increment generation, and
  dispatch to a new path.
- Late old result: generation/claim mismatch; it cannot bind or publish.

No target path is given to the worker.

After complete validation, one transaction creates the staged artifact
handle/location, records result/companion facts, marks the attempt terminal,
and transitions the operation to staged.

### Verify, prepare, promote, finalize

Verification and the result probe run against the exact staged bytes.
Prepare uses the commit-safety gate, creates one pending artifact commit,
persists temp/probe evidence, and transitions the operation to prepared in one
transaction.

Promotion uses add-only no-replace semantics. Before mutation, the claim and
generation are renewed/rechecked. An occupied target is accepted only when
size and hash match. After mutation, claim loss leaves the exact target as
successor recovery evidence.

Finalize is one transaction:

- `record_verified_sidecar_commit_rows_in_tx` creates the committed result
  file identities and completes the artifact commit;
- `record_with_event_in_tx` records the preprobed snapshot;
- every descriptor is resolved to exactly one result snapshot stream;
- companion lineage rows are inserted;
- the operation becomes committed; and
- the artifact commit-completed event is appended.

Despite the helper's historical sidecar name, its contract is generic:
finalize one verified add-only artifact into file asset/version/location rows.
No bundle member is created for the synthesized container result.

### Recovery

Owned errors after prepare atomically mark the operation and artifact commit
recovery-required. A successor claim:

1. loads the persisted staging/target/temp facts and probe payload;
2. rechecks the source commit-safety gate;
3. accepts exact target bytes or resumes promotion;
4. runs the same finalize transaction; and
5. returns the committed report.

Crashes before staged binding redispatch through generation fencing. Crashes
after committed finalization load the committed report. No path creates a
second handle, commit record, result snapshot, companion, or lineage row.

## Reporting and observability

`ExecuteTranscodeAudioReport` adds optional:

- `synthesis_operation_id`;
- `synthesis_operation_key`; and
- ordered `synthesized_companions`.

Each companion report contains both source and result identities, result media
facts, and `lineage_id`. Replacement transcode reports omit these fields.

Started/succeeded/failed audio-transcode events add optional synthesis mode,
operation identity, and ordered companion mappings. New durable payload fields
are additive defaults and are added to the payload contract inventory.

Compliance execute and `report --job-id` collect ordered synthesis companions
from successful transcode-audio ticket results in a separate
`audio_synthesis_companions` list. Strict decoding rejects malformed published
results. CLI JSON envelopes expose the control-plane structures unchanged.

## Failure behavior

- Descriptor drift: config invalid before staging.
- Existing target with unknown/mismatched ownership or bytes: conflict, no
  overwrite.
- Partial/malformed worker result: malformed worker result, no staged binding.
- Verification/probe failure: no prepare or target bytes.
- Claim/generation loss: stale actor stops; successor resumes.
- Promotion/finalize failure: recovery-required with exact path and commit
  evidence.
- Malformed durable operation/probe/result payload: fail loud; never dispatch
  or manufacture lineage.

## Security and trust boundaries

No auth or capability vocabulary changes. Worker JSON, worker-reported file
facts, filesystem leaves, and probe output are untrusted. Typed strict payloads,
path containment, regular-file/no-symlink checks, exact checksum comparison,
commit-safety gates, and claim/generation predicates remain mandatory.

This change does not claim protection from a malicious same-account process or
strong inode/ownership fencing; those are owned by the later execution-safety
campaign.

## Test strategy

Planner/unit:

- one/many source streams produce stable ordered descriptors;
- plan regeneration preserves operation/companion IDs;
- descriptor drift, duplicates, zero match, and non-downmix block or reject.

Worker contract/control-plane:

- request sets add-track and target channels;
- result validation rejects missing/extra/reordered IDs and wrong codec,
  channels, language, title, or disposition;
- probe validation proves source preservation and exact companion set.

Store/recovery:

- unique operation/member/result-stream/lineage constraints;
- exact replay returns the same rows;
- incomplete finalize rolls back;
- claim/generation rejects stale actors;
- crashes before send, after response, after stage, after prepare, after target
  install, and during finalize resume without duplicates.

Generated media:

- real surround source produces a stereo companion while retaining surround;
- two selected surround sources produce two ordered companions;
- inspect committed ffprobe facts and durable lineage, not just request/exit;
- retry and recovery preserve row/file counts and identities.

Verification:

- focused crate tests and generated-media integration tests;
- strict Clippy on touched crates; and
- complete `just ci`.
