---
status: accepted
date: 2026-07-27
deciders: [VOOM core]
---

# 0043 — Publish synthesized audio companions with recoverable stream lineage

## Context

ADR 0026 publishes `synthesize audio` as add-track mode on the existing
`transcode_audio` operation kind. The compiler, planner shape, worker request
fields, ffmpeg execution, and worker verifier exist, but the control plane
rejects synthesis. The current audio-transcode path also has no stable
companion descriptors or operation ledger: it records a staged artifact, uses
the generic single-artifact commit, and records the result snapshot afterward.
A retry cannot identify a prior synthesis operation, and a crash between file
commit and snapshot recording has no domain state from which to finish
companion lineage.

A synthesized output is one container file with all source streams preserved
and one new companion per selected source stream. The file commit is singular,
but its companion set and source-to-result relationships are plural and must
be exact. A worker result with a missing, extra, reordered, or malformed
companion must not become published state.

Design:
[`docs/superpowers/specs/2026-07-27-issue-333-audio-synthesis-lineage-design.md`](../superpowers/specs/2026-07-27-issue-333-audio-synthesis-lineage-design.md).

## Decision

### Publish stable companion descriptors in the plan

The planner gives every planned synthesis node an `operation_id` using the
same stable node identity mechanism as extraction. It emits one ordered
companion descriptor per selected source stream. A domain-separated hash of
the operation ID and source snapshot stream ID produces the stable
`companion_id`; that value is also the result snapshot stream ID supplied to
the worker.

Each descriptor pins:

- companion/result snapshot stream ID;
- source snapshot stream ID;
- source provider stream index; and
- ordinal in stable source-stream order.

Execution recomputes the descriptors from the pinned source snapshot and
rejects drift. No new DSL form or `OperationKind` is introduced.

### Persist one synthesis operation and its ordered companions

Add synthesis-specific operation, companion, dispatch-attempt, and lineage
tables. The operation key covers the planned operation ID, source file
version, pinned source media snapshot, ordered descriptors, target codec,
channel count, container, and canonical target path. It is the idempotency
boundary for retries and resume.

The operation owns one staged artifact and commit record. Companion rows own
the stable source-to-result mappings and validated output facts. A committed
operation returns its recorded report. A staged operation resumes the generic
artifact commit ledger without creating another artifact, commit, snapshot,
or lineage row.

There is no legacy adoption path. Synthesis execution did not exist before
this decision, so no historical committed synthesis artifact can be proven.

### Fence dispatch generations

A live, expiring claim tied to the workflow lease fences every noncommitted
operation mutation. Each worker generation uses a distinct private staging
path and a stable idempotency key derived from the operation key and
generation. The dispatch attempt, worker identity/epoch, key, and exact path
are durable before send.

A host restart first replays the same key to the same live worker epoch. A
persisted terminal attempt advances to a new generation because its response
was not durably bound. If the worker epoch changed, the attempt is quarantined
and a new generation writes a different path.
Old-generation writers can never reach the current staging or target path;
their late completions fail the generation/claim predicate. Quarantined paths
remain diagnostic evidence and are not reused.

### Validate the complete companion set before publication

The request uses `add_track = true`, the published target channel count, and
one `AudioStreamRef` per descriptor. Its snapshot stream ID is the derived
companion ID while its provider stream index selects the source stream.

The control plane accepts a worker result only when:

- result IDs exactly equal the ordered planned companion IDs;
- every result provider index is unique and identifies one audio stream in the
  precommit probe;
- every companion has the target codec and channel count;
- language, title, and disposition facts match its selected source;
- the reported file facts match the exact staging file; and
- a precommit probe of those bytes contains one exact stream for every planned
  companion and preserves every selected source stream's media facts.

The normalized ffprobe payload does not preserve the worker's private
`snapshot_stream_id` tag: it initially assigns stream IDs from provider
indexes. After independently validating the worker result against the planned
IDs and the exact probe facts, the control plane replaces the ID of each
validated companion stream at its unique result provider index with the
planned companion ID. It rejects an occupied or ambiguous ID/index mapping.
This bound payload, not the unbound normalized probe, is the result snapshot
prepared for publication.

Malformed or partial worker results leave no staged artifact binding. Once the
complete worker result and exact staging-file facts pass, artifact creation and
synthesis-operation binding commit in one transaction. Verification or probe
failure then leaves one resumable staged operation bound to that same artifact;
retry never redispatches or creates another handle.

### Finalize the result snapshot and lineage atomically

After all validation and verification, the synthesis operation is staged with
one generic artifact handle. Generic artifact commit creates its pending
record and performs add-only promotion, accepting an occupied target only
when its exact size and hash match the staged artifact.

After the target is exact, one SQLite transaction:

1. finalizes the generic artifact commit and result file asset/version/location,
   including its commit-completed event;
2. records the preprobed and companion-bound result media snapshot;
3. records one synthesis lineage row per ordered companion, including the
   source and result stream identities plus codec, channels, language, title,
   and disposition facts;
4. marks every companion finalized; and
5. marks the synthesis operation committed and records the synthesis-success
   event.

Relational publication therefore exposes the result snapshot and complete
lineage set together or neither. The generic artifact ledger owns pending and
recovery-required commit evidence; the synthesis operation remains staged.
Recovery resumes that exact commit, then reuses the same synthesis
finalization transaction. A committed replay returns the same identities.

### Report synthesis distinctly within transcode execution

The existing transcode execution report evolves additively with an optional
ordered `synthesized_companions` list and optional synthesis operation
identity. Ordinary replacement transcodes omit both fields and retain their
wire shape. Each synthesis companion report includes planned/result IDs,
source file/snapshot/stream identity, result file/snapshot/stream identity,
codec, channels, language, title, disposition, and lineage row ID.

Compliance execute and job-report views collect these ordered results from
succeeded `transcode_audio` tickets when the synthesis fields are present.
Audio transcode events add the synthesis mode and ordered relationships
additively.

## Consequences

- Planning, execution, worker correlation, durable lineage, and reporting use
  the same stable ordered companion identities.
- One and many selected source streams produce one output file with the
  sources preserved and exactly one companion per source.
- Retry and recovery cannot duplicate target files, artifacts, snapshots,
  companion rows, or lineage rows.
- Generation-specific stale staging paths can remain after an unavailable
  worker epoch. They are never reused or published; generalized staging
  garbage collection remains independent of synthesis correctness.
- The schema and recovery code are synthesis-specific. This avoids a premature
  generic media-operation abstraction while preserving the existing
  extraction and replacement-transcode paths.
- Rollback follows ADR 0013 binary-before-database ordering. The migration is
  additive and needs no historical backfill.

## Considered and rejected alternatives

### Reuse the replacement-transcode path without an operation ledger

Rejected. It cannot recover the gap between file commit and snapshot/lineage
recording, nor return stable identities on replay.

### Store parent lineage only in the media snapshot JSON

Rejected. Retry uniqueness and relationship queries require indexed typed
rows with foreign keys. Snapshot facts remain the media observation; lineage
is a durable relationship.

### Reuse `audio_extract_output_lineage`

Rejected. Extraction relates one source stream to one result file. Synthesis
relates multiple source streams to multiple result streams within one result
file and snapshot; forcing that shape into extraction tables would erase
their invariants.

### Introduce a generic media-operation framework

Rejected. Extraction and synthesis have different file cardinality, bundle,
dispatch, and lineage rules. A second concrete use does not yet justify a
shared framework, and the issue requires a surgical vertical slice.

### Add a new synthesis operation kind or DSL form

Rejected by ADR 0026 and the published grammar. Synthesis remains add-track
mode on `transcode_audio`.
