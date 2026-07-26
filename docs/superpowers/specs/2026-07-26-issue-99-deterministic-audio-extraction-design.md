# Issue #99: Deterministic multi-output audio extraction design

## Goal

Make the published operation

```text
extract audio [where <track-filter>]
```

plan and execute one ordered worker output per matched source audio stream. The
planner fixes operation identity, output identity, source lineage, bundle role,
and collision-safe name suffixes before a durable ticket is created. Retries
and resume consume those fixed descriptors rather than reassigning identities.

Issue #337 owns atomic host staging, verification, commit, bundle registration,
durable lineage, and reporting for the plural result. Until #337 lands, the
current single-sidecar host fails before creating a staging directory when a
planned operation contains more than one descriptor.

## Existing contracts

- `snapshot_stream_id` is the durable stream identity within a source
  `MediaSnapshot`; `provider_stream_index` is the worker-local selector.
- Ascending unique provider stream index is canonical source order. Snapshot
  JSON array order is not semantic.
- Extracted sidecars are Opus in Ogg and use
  `<source-stem>.<stream-name>.opus.ogg`.
- A known commentary fact maps to `commentary_audio`; known non-commentary maps
  to `external_audio`. An absent or malformed fact cannot be guessed.
- `PlanNode.node_id` is deterministic from phase, operation ordinal, operation
  kind, and target.
- The worker protocol and durable JSON payloads evolve additively under ADR
  0013. Bundled workers are version-locked under ADR 0016.
- The published grammar already permits an omitted selector. No parser,
  compiler, or DSL change is needed.

## Planner resolution

The planner evaluates the compiled selector once against structured snapshot
facts, then:

1. rejects a missing stream inventory, duplicate snapshot stream ID, duplicate
   provider stream index, unsupported selector, missing required fact, or a
   source with no video;
2. sorts matches by ascending provider stream index;
3. rejects zero matches with the existing visible per-file planning diagnostic;
4. resolves the bundle role for every match and blocks the whole node if any
   role is unknown; and
5. emits one output descriptor per match in canonical source order.

One match stays planned and retains its existing target-name suffix. More than
one match is no longer a planning error.

## Stable identities

The operation identity is exactly the plan node's existing `node_id`. It is
written into the extract payload as `operation_id`, making the retry/resume key
explicit without creating a second operation identity.

Each output identity uses this fixed preimage:

```text
blake3(
  b"voom.extract_audio.output.v1\0"
  + operation_id.as_bytes()
  + b"\0"
  + snapshot_stream_id.as_bytes()
)
```

The public text form is:

```text
extract_output_<first 16 lowercase hexadecimal digest characters>
```

The domain separator, NUL boundaries, digest algorithm, prefix, and truncation
length are contract. Tests pin an exact known value so refactoring cannot drift
identity silently. Because snapshot stream IDs are unique inside the pinned
snapshot and operation IDs scope the source/phase/ordinal, output IDs are
unique within an operation and stable across replanning, retry, and resume.

## Descriptor shape

The plan payload carries:

```json
{
  "type": "extract_audio",
  "operation_id": "node_0123456789abcdef",
  "target_codec": "opus",
  "container": "ogg",
  "source_media_snapshot_id": 42,
  "outputs": [
    {
      "output_id": "extract_output_0123456789abcdef",
      "source_snapshot_stream_id": "audio-1",
      "source_provider_stream_index": 1,
      "name_suffix": "audio-1.opus.ogg",
      "bundle_role": "external_audio"
    }
  ]
}
```

The selector remains in the payload for compatibility and audit context, but
execution treats the ordered descriptors as authoritative. The control plane
re-reads the pinned snapshot and validates that each descriptor still names the
same selected source stream, index, order, and role. It does not create new
identities by reevaluating an unordered collection.

Legacy extract payloads without `operation_id` or `outputs` remain readable.
The current host admits that form only for exactly one selected stream, which
preserves already-materialized single-output work. Newly generated plans always
carry both fields.

## Collision-safe names

The base descriptor name is the current suffix:

```text
<sanitized-snapshot-stream-id>.opus.ogg
```

Sanitization preserves ASCII alphanumeric characters, `-`, and `_`, replaces
every other character with `-`, and uses `stream` when the result is empty.
Collision detection happens after this complete sanitization and after ASCII
case-folding, matching case-insensitive target filesystems. No later stage may
sanitize the descriptor suffix again.

For a singleton operation, the name is unchanged. This preserves the existing
one-output behavior. For every output in a plural operation, the planner
appends the output identity's 16 hexadecimal characters:

```text
<sanitized-id>-<output-hash>.opus.ogg
```

The hash has a fixed width and every plural member carries one. Two final names
can therefore be equal only if both their sanitized bases and truncated output
hashes are equal. The planner performs one final ASCII-case-folded uniqueness
check over the emitted names and blocks instead of emitting aliases if that
cryptographic collision ever occurs. Descriptors remain ordered by provider
stream index, not by their filenames.

## Worker request and result

The request keeps `input` operation-wide. Each plural request member has this
exact shape:

```json
{
  "output_id": "extract_output_0123456789abcdef",
  "selection": {
    "snapshot_stream_id": "audio-1",
    "provider_stream_index": 1
  },
  "output": {
    "staging_root": "/tmp/voom-stage",
    "path": "/tmp/voom-stage/ticket-2/lease-1/movie.audio-1.opus.ogg",
    "container": "ogg",
    "audio_codec": "opus",
    "overwrite": false
  }
}
```

The result keeps `status`, `provider`, `provider_version`, `input_pre`, and
`input_post` operation-wide. Each plural result member has this exact shape:

```json
{
  "output_id": "extract_output_0123456789abcdef",
  "selection": {
    "snapshot_stream_id": "audio-1",
    "provider_stream_index": 1
  },
  "path": "/tmp/voom-stage/ticket-2/lease-1/movie.audio-1.opus.ogg",
  "output": {"size_bytes": 321, "content_hash": "blake3:..."},
  "output_container": "ogg",
  "output_audio_codec": "opus",
  "output_language": "eng",
  "output_title": "Main"
}
```

For every ordinal, result validation requires exact equality with the request's
`output_id`, complete `selection`, and `output.path`, plus observed
container/codec equality with the requested output settings. Output byte facts,
language, and title are output observations validated against the staged file
and pinned source facts. Correlation never relies on list position alone.

`ExtractAudioRequest` and `ExtractAudioResult` gain additive, presence-preserving
`Option<Vec<_>>` ordered descriptor lists. Their existing singular fields remain
the required projection of the first output:

- `None` (the field is absent) means legacy singleton data;
- `outputs: null` is invalid rather than another spelling of `None`;
- `Some([])` (the field is explicitly present and empty) is invalid;
- `Some(non_empty)` is the authoritative ordered list and carries every output,
  including the first;
- the first list member must equal the singular projection;
- output IDs, source snapshot IDs, source provider indexes, and normalized paths
  must be unique and remain in request order.

The request's singular projection is exact:

| Singular request field | First request descriptor |
|---|---|
| `selection` | `outputs[0].selection` |
| `output` | `outputs[0].output` |

The result's singular projection is exact:

| Singular result field | First result descriptor |
|---|---|
| `output` | `outputs[0].output` |
| `output_container` | `outputs[0].output_container` |
| `output_audio_codec` | `outputs[0].output_audio_codec` |
| `selected_snapshot_stream_id` | `outputs[0].selection.snapshot_stream_id` |
| `output_language` | `outputs[0].output_language` |
| `output_title` | `outputs[0].output_title` |

The singular shapes have no output ID, provider index, or path echo; those
fields are validated through the plural request/result pair. Legacy singleton
data therefore retains its historical validation strength, while every newly
planned request carries the stronger correlation fields.

Observed output facts are validated per correlated descriptor but are not
identities and need not be unique. Distinct source streams may legitimately
produce the same content hash, size, language, title, or other media facts.

This redundant first projection is deliberate compatibility, not two
independent sources of truth. Validation rejects disagreement before invoking
FFmpeg or accepting a worker result.

The serde field uses `default`, `skip_serializing_if = "Option::is_none"`, and a
presence-aware deserializer. The deserializer is invoked only when the field is
present and requires a JSON array before returning `Some(Vec<_>)`; it therefore
rejects `null`. A legacy `None` serializes by omitting the field. Plain
`Option<Vec<_>>` deserialization is forbidden because it conflates missing and
explicit `null`.

The FFmpeg worker validates every descriptor and every output path before
starting provider work. It then extracts outputs in request order and returns
one result descriptor per request descriptor. A provider or probe failure
returns an operation error, never a shortened success list. Files already
written in the private staging area remain uncommitted evidence; issue #337
validates the complete result set and atomically commits all or none.

## Current host boundary

The current control-plane extraction path supports the legacy singular commit
unit. It:

- accepts a legacy payload or a new one-descriptor payload;
- verifies the descriptor against the pinned snapshot;
- sends one ordered descriptor to the worker and validates the singular/list
  projection; and
- rejects a payload with more than one descriptor before preparing staging or
  target paths, recording artifacts, or dispatching a worker.

This prevents the interval between #99 and #337 from silently committing only
the first sidecar.

## Failure behavior

- Zero matches: a blocked node with an actionable
  `extract_audio selector matched zero audio streams` diagnostic.
- Duplicate source identity/index or missing facts: blocked as insufficient
  snapshot facts.
- Unknown role on any selected stream: the whole node blocks.
- Duplicate output ID, a final normalized-name collision, reordered descriptor,
  or snapshot mismatch: configuration error before provider work.
- Invalid request projection, duplicate path, path outside staging, or existing
  path: worker rejects the complete operation before FFmpeg starts.
- Missing, extra, reordered, duplicated, or projection-inconsistent result:
  malformed worker result; the host commits nothing.
- Provider/probe failure after one output: operation error with no success
  result; any staged file remains outside the durable commit boundary.

## Compatibility and rollback

No grammar, compiled-policy type, migration, dependency, or database payload
changes are introduced. Historical compiled policies still deserialize and
generate new descriptors when planned.

The plan payload additions are optional on read. Historical single-output
workflow payloads still select and execute one stream. Worker request/result
lists are additive optional fields that preserve absent versus explicitly empty
input, and the singular fields retain their exact wire meaning. A rollback
rejects newly written unknown fields loudly under the project payload contract;
restoring a pre-upgrade binary therefore follows the existing binary-before-DB
release rule.

## Test strategy

- Planner behavior tests cover bare and broad selectors, canonical ordering
  despite shuffled JSON, zero/one/many matches, unknown roles, duplicate
  indexes, deterministic plural names after sanitization/case-folding,
  second-order collision attempts, final uniqueness, exact output-ID preimages,
  and identical identities across repeated plans.
- Payload tests read historical extract payloads without descriptors and
  round-trip new ordered descriptors.
- Workflow tests generate a real plural plan, bridge/render/persist/reload its
  ticket, and prove retry preserves the descriptor bytes while resume
  regeneration produces the same IDs, names, and order.
- Worker-protocol tests separately read historical JSON with omitted lists,
  prove legacy serialization omits the field, reject literal `outputs: null`
  and `outputs: []`, round-trip plural request/results, and reject duplicate,
  reordered, and inconsistent projections through validation helpers. Literal
  wire-shape assertions pin every descriptor field and every table mapping. A
  plural result with distinct identities/paths but identical observed facts is
  accepted.
- FFmpeg worker tests execute one and multiple descriptor requests, assert
  ordered source/output facts, reject all malformed paths before invocation,
  and prove a partial provider failure returns no success result.
- Control-plane tests prove one output preserves behavior and plural payloads
  fail before staging-directory or target-path creation. Fake-dispatcher tests
  return projection-inconsistent, extra, missing, and reordered result
  descriptors and prove the host rejects each before verifier or commit
  activity, with no target file, durable artifact/version, or success event.
- Focused strict Clippy and the repository-wide `just ci` gate remain clean.

## Out of scope

- Host multi-artifact staging, verification, atomic commit, bundle membership,
  durable lineage rows, recovery, CLI results, and compliance reporting (#337).
- Synthesized audio companions and their stream lineage (#333).
- Generated-media full grammar acceptance (#338), except focused worker tests
  required to prove this contract.
- Parser aliases, new selectors, unpublished DSL forms, or campaign-excluded
  issues.
