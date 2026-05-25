# Issue 70 Stream Metadata Design

## Context

Issue #70 is still open. The current ffprobe normalizer preserves stream order,
kind, codec, dimensions, duration, frame rate, sample rate, and channel count,
but it drops `tags.language` and `disposition`. Scan persistence writes the
normalized worker snapshot directly to `media_snapshots.payload`, so discarded
normalizer fields are not durable.

The Chaos observed-state exporter currently compensates by inferring `und` for
MP4/MOV streams. That inference belongs upstream in the durable snapshot only
when ffprobe actually reported the language.

## Goals

- Preserve stream language tags in `media_snapshots.payload`, including
  explicit `und` values from ffprobe.
- Preserve default and forced disposition flags when ffprobe provides them.
- Keep existing scan envelope shape stable.
- Remove the Chaos observed-state export fallback that synthesizes MP4/MOV
  `und` language values.
- Cover MP4 and MKV metadata behavior with focused tests.

## Non-Goals

- Do not add query-optimized stream metadata tables in this issue.
- Do not infer language from container type or file extension.
- Do not implement audio/subtitle policy selection.
- Do not ingest external subtitle sidecars; that remains #72.

## Design

Extend `voom-ffprobe-worker` normalization only. For each stream:

- Copy `tags.language` into normalized stream field `language` when present and
  not one of the existing unknown sentinels.
- Copy disposition flags into normalized stream field `disposition` as an object
  containing `default` and/or `forced` boolean fields when those keys are present.
- Accept ffprobe's usual `0`/`1` numeric disposition values and boolean values.
  String `"0"` and `"1"` are accepted defensively because other numeric ffprobe
  fields already accept numeric strings.
- Reject malformed present disposition values rather than silently producing a
  misleading snapshot.

The normalized snapshot remains the persistence contract. No store migration is
needed because `media_snapshots.payload` is JSON text and `persist_scanned_media_snapshot`
already records the worker result unchanged.

Update `crates/voom-cli/tests/support/observed_state.rs` so observed-state export
uses only the normalized `stream.language` field. If language is missing, the
export omits it instead of synthesizing `und` for MP4/MOV.

## Error Handling

Language remains optional. Missing `tags`, missing `language`, missing
`disposition`, or missing individual disposition flags are not errors. A present
disposition flag with a value other than boolean, `0`, `1`, `"0"`, or `"1"` is
reported as `MalformedWorkerResult`, consistent with existing numeric field
validation.

## Verification

- Add normalizer tests for MP4 `und` language/default disposition and MKV
  audio/subtitle language plus forced/default disposition.
- Add a normalizer test for malformed disposition values.
- Add or update observed-state support tests so MP4 language comes from the
  snapshot and is not inferred when absent.
- Run `cargo test -p voom-ffprobe-worker normalize`.
- Run the focused Chaos observed-state tests.
- Run `just fmt-check`, `just lint`, and `just test`.
