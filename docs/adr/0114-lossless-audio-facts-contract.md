# 0114 — Lossless audio settings resolve integer facts per stream

## Status

Accepted

## Context

[#760](https://github.com/randomparity/voom-v2/issues/760), under epic #758,
needs a settings/facts prerequisite before codec children #761–#763 can ship.
Existing audio requests use lossy profile bitrates; outputs lack rate and
integer precision evidence. One request can select heterogeneous streams.
The operator approved additive lossless settings and bounded integer contracts
on 2026-10-28. ADRs 0013 and 0016 govern durable JSON and exact wire versions.

## Decision

Add optional lossless settings containing only an optional 16/24-bit target
precision preference. Resolve rate, channels, exact layout and precision per
selected source; absence preserves source precision. Ordinary archival settings
preserve rate/layout and reject precision reduction, missing facts, floating
samples and unsupported integer/layout combinations. Keep 5.1 and 5.1(side)
distinct. Shared protocol helpers own this validation and output verification.

Collect optional observed rate/sample format/precision alongside existing
layout. New durable fields default absent and concrete serde structs reject
unknown fields. Advance exact worker protocol 4→5 and deploy coordinated
binaries before new durable writes; rollback requires the pre-upgrade database.
Existing lossy requests retain their bitrate path and omit the new settings.

This prerequisite does not enable FLAC, ALAC or PCM targets. Present lossless
settings on currently supported lossy targets fail before mutation. Codec
children own codec-specific bounds, encoder args/readiness, and real execution.
Canonical decoded-s32 equality is test support for known integer fixtures;
a downmixed companion is a transform, not a bit-exact archival copy.

## Consequences

Mixed selected streams retain independent resolved facts. No bitrate or
compression knob is required to represent archival intent. Missing layout stays
a failure: FFmpeg 8.1.3 PCM-in-MKV probes can omit it, a constraint for #763.
Strict optional additions preserve old JSON reads but old binaries reject new
fields, as intended by ADR 0013. Persisted media dispatch envelopes also carry
an exact protocol schema: quiesce ticket creation and complete or explicitly
operator-disposition unfinished schema-4 media work before swapping binaries.
The release drain procedure governs that operator action; this change performs
no drain or migration. Old additive settings/facts JSON readability is distinct
from deliberate old-envelope execution rejection. No byte ownership moves.

## Considered & rejected

- **Replace settings with a tagged lossy/lossless enum.** verified: at base
  `5ba4d649`, `rg -n 'deny_unknown_fields|struct TranscodeAudioSettings'
  crates/voom-worker-protocol/src/operations/audio.rs` shows a strict request struct;
  replacing its fields would reject existing request shapes. Judgment: additive
  fields deliver this prerequisite with less coordinated persisted-data change.
- **One resolved request-level rate/layout.** judgment: unnecessarily constrains
  requests selecting heterogeneous streams; resolution belongs per stream.
- **Infer precision/layout from storage width/channel count.** verified: FFmpeg
  8.1.3, `ffprobe -v error -show_streams -of json` on generated PCM24 MKV
  reports `sample_fmt=s32,bits_per_raw_sample=24` and no `channel_layout`. Judgment: neither
  proxy proves the required significant precision or channel arrangement.
- **Wait for codec children.** judgment: duplicates the shared contract across
  independently scoped codec work and leaves #760's requested prerequisite unmet.
