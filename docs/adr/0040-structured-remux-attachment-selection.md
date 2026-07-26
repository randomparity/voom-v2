---
status: accepted
date: 2026-07-26
deciders: [VOOM core]
---

# 0040 — Resolve remux attachments from structured provider facts

## Context

The published policy language permits `keep attachment` and `remove attachment`, including the
`font` filter. The remux planner and control plane currently reject every attachment source, and
the mkvtoolnix worker always emits `--no-attachments`.

The two providers represent the same Matroska attachment differently:

- ffprobe exposes an attachment as a stream with `codec_type = "attachment"`. FFmpeg documents
  that attachment streams follow the ordinary mapped streams.
- `mkvmerge --identify --identification-format json` exposes ordinary tracks in `tracks` and
  attached files in a separate `attachments` array. `mkvmerge --attachments <ids>` selects
  attached files by the identifiers in that array.

The normalized ffprobe snapshot currently drops the attachment `filename` and `mimetype` tags.
The remux planner's `font` filter instead searches for the substring `"font"` in an optional MIME
value. That is neither an exact vocabulary nor fail-closed behavior.

The existing `RemuxSelection.keep_streams` contract already carries the stable snapshot stream ID
and ffprobe provider-stream index for every selected source stream. A second attachment-selection
wire contract would duplicate that identity and create version-skew work without adding a fact
the providers need.

## Decision

### Canonical snapshot facts

The ffprobe normalizer copies attachment tags into the same structured stream object used by all
planning:

- `tags.filename` becomes `filename`;
- `tags.mimetype` becomes `mime_type`;
- tag names remain case-insensitive, matching the existing Matroska tag normalization;
- values remain exact strings after the existing empty/sentinel rejection.

The raw ffprobe JSON remains unchanged under `raw.ffprobe_json`.

Commentary remains a structured disposition fact. The remux projection carries it as
`Option<bool>`. A `commentary` filter requires `Some(true)` or `Some(false)`; absence or a
non-boolean durable value blocks that file as insufficient snapshot facts.

### Exact font vocabulary

`font` evaluates only attachment MIME facts. It accepts the official and legacy font media types
listed by the Matroska attachment specification:

- `font/sfnt`
- `font/ttf`
- `font/otf`
- `font/collection`
- `font/woff`
- `font/woff2`
- `application/x-truetype-font`
- `application/x-font-ttf`
- `application/vnd.ms-opentype`
- `application/font-sfnt`
- `application/font-woff`

Matching is exact and case-sensitive against the normalized fact. Missing MIME data blocks the
file. `application/octet-stream` and filename extensions never imply a font: the Matroska
specification permits players to guess from extensions, but policy execution must not.

### Provider mapping and execution

The mkvtoolnix worker builds one source-item mapping:

1. ordinary mkvmerge tracks retain provider indexes `0..tracks.len()`;
2. mkvmerge attachments receive provider indexes starting at `tracks.len()`, in attachment-array
   order, matching ffprobe's documented appended-stream model;
3. the item kind distinguishes video, audio, subtitle, attachment, and unsupported values.

The worker resolves every requested `keep_streams` reference through that mapping. It emits:

- `--attachments <ids>` when one or more attachment items are kept;
- `--no-attachments` when none are kept;
- the existing track-selection arguments for video, audio, and subtitle items.

Attachment items do not enter mkvmerge's `--track-order`, default, or forced-flag arguments.
Issue #332 owns filter-addressed track ordering and defaults; this decision does not change those
semantics.

The output probe maps attachments the same way and validates:

- the exact number and kinds of selected output items;
- ordinary-track fingerprints and order as before;
- attachment filename, byte-size, and MIME-identity fingerprints.

Commentary disposition is compared explicitly for every selected ordinary track, including when
the source has only one track of that kind. A provider cannot bypass the check through an
otherwise-unambiguous single-track mapping.

MIME identity maps every recognized legacy or registered font MIME type to one `font` class and
keeps every non-font MIME value exact. MKVToolNix 99 and later may therefore canonicalize a legacy
font MIME type without causing a false identity mismatch, while a font-to-non-font change fails
output validation and cannot commit an artifact that immediately replans as noncompliant.

### Planning and safety

Attachment keep/remove actions use the same deterministic initial-keep-set algorithm as audio and
subtitle actions. Every source stream starts kept; an action changes only streams of its target
kind. Planning and execution share the same ordered reducer, so later actions may deliberately
restore streams removed by earlier actions. All video streams are re-added, and the existing
final-audio guard runs after all actions.

A container-only remux therefore preserves every attachment. A filtered remux preserves all
unselected video, audio, subtitle, and attachment items. Missing or malformed facts needed by a
selector block planning or runtime selection; they never become a match or non-match silently.

## Consequences

- Existing compiled policy versions remain readable. No compiled-policy, database, migration, or
  worker-protocol schema changes are required.
- Existing remux requests remain readable because `RemuxSelection` is unchanged.
- Historical media snapshots remain readable. A historical attachment stream lacking the MIME
  fact required by `font` blocks that selector until the file is probed again.
- A successfully remuxed MKV is probed through the normal authoritative snapshot path. The same
  attachment/commentary policy replans as `NoOp` when the output satisfies it.
- Generated-media coverage must include a commentary audio stream, a main audio stream, a font
  attachment, and a non-font attachment. It must inspect the output inventory and compliant
  replanning.

## Considered and rejected alternatives

### Add a separate attachment list to `RemuxSelection`

Rejected. The existing stream reference already names the ffprobe stream identity and provider
index. A second list would duplicate selection state, require additive wire-version handling, and
allow the two lists to disagree.

### Match attachments by filename or MIME alone

Rejected. Filenames and MIME types may repeat, and MKVToolNix may canonicalize legacy font MIME
types. The provider-index mapping selects the item; filename and size validate output
preservation.

### Infer fonts from filename extensions or titles

Rejected. This is a string heuristic outside the published policy facts. It would turn an unknown
MIME value into a match and violate fail-closed planning.

### Launch ffprobe from the mkvtoolnix worker

Rejected. Workers remain provider-specific, and the control plane already supplies the ffprobe
stream identity selected from the authoritative snapshot. Adding an ffprobe dependency to the
mkvtoolnix worker would expand the trust and deployment boundary.
