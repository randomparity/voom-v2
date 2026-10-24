# ADR 0110: Synthesized audio companions are non-default

## Status

Accepted (2026-10-24)

## Context

Issue #737 and the operator's approved resolution preserve existing audio default
selection while adding companions as non-default. Copying a default surround
source's flag onto its AAC companion instead creates a second default track.
ADR 0043's complete-companion validation clause and its historical design require
disposition facts, including default, to match the selected source. That default
requirement conflicts with the approved companion behavior.

## Decision

For synthesis (`add_track = true`), preserve existing tracks' default flags and
set each appended companion's default flag to false, including when its selected
source is default. Preserve language, title, identity and other applicable
metadata through the existing synthesis path. Ordinary in-place transcoding
retains its existing metadata-preservation behavior.

This partially supersedes only the default-equality requirement in
[ADR 0043, Validate the complete companion set before publication](0043-recoverable-audio-synthesis-lineage.md#validate-the-complete-companion-set-before-publication)
and item 6 of the historical design's
[Complete result validation](../superpowers/specs/2026-07-27-issue-333-audio-synthesis-lineage-design.md#complete-result-validation).
Their remaining metadata, lineage, fencing and validation decisions continue to govern.

The FFmpeg synthesis worker explicitly sets dispositions and requires the actual
companion probe to report non-default before returning success. Returned facts
continue to describe observed bytes. Existing control-plane admission,
publication and storage of observed result flags remain unchanged.

## Consequences

New synthesis preserves the prior default selection without introducing another
default. Existing artifacts and recorded observations retain their actual flags.
The existing boolean facts accept false without a schema or protocol change.
Track placement and broader final-policy verification remain #339's questions.

## Considered & rejected

- Keep copying the source default. verified: #737 and its generated-media worker
  regression reproduce EAC3 default plus AAC default on base 2610bab6 with FFmpeg 8.0.1.
- Clear defaults on existing source tracks. judgment: violates the approved selection-preservation requirement.
- Clear defaults in the shared metadata helper. verified: that helper is also used by
  ordinary transcoding in `crates/voom-ffmpeg-worker/src/ffmpeg.rs` at base 2610bab6;
  changing its semantics would alter the protected ordinary-transcode path.
