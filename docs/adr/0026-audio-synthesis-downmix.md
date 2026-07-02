---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0026 — Audio track synthesis (downmix companion)

## Context

`transcode audio` re-encodes matched audio streams *in place*: the output has
the same stream set, selected streams re-encoded, and the ffmpeg worker's
verifier **enforces** channel preservation
(`crates/voom-ffmpeg-worker/src/ffmpeg.rs`, `verify_preserved_audio_metadata`).
There is no way to *add* a new audio track derived from an existing one, so the
very common "5.1 surround + stereo companion" library layout is unreachable
(#276). The `synthesize` keyword is already reserved-but-deferred in the parser
and hard-errors as `DeferredExecutionOperation`.

Synthesis needs three things the current audio path lacks: a DSL surface that
names a *source* track filter and a *target* channel layout; a downmix
(`-ac <n>`) in the worker with a verifier that allows the new channel count for
the synthesized stream while still preserving it for transcode; and a way to
register the added track as a **new** snapshot stream whose lineage parent is
the source stream (not a replacement).

## Decision

Add a `synthesize audio` operation that **adds** a downmixed companion track.

1. **Grammar (V1.1, block form).** `synthesize audio from <track-filter>
   { codec <aac|opus|eac3> channels <n> }`. The `from` clause selects the source
   audio stream(s); the block body sets the companion's target codec and channel
   count. One companion is added per selected source stream. Full production in
   `docs/specs/voom-control-plane-design.md`.

2. **Reuse the `transcode_audio` operation kind end-to-end (no new
   `OperationKind`).** Synthesis is modelled as an add-track *mode* on the
   existing audio path rather than a new `voom_core::OperationKind`:
   - `CompiledOperation::SynthesizeAudio { target_codec, container, target_channels, filter }`
     (voom-policy) lowers the DSL.
   - `AudioOperationType::SynthesizeAudio` + `target_channels` on the plan
     payload (voom-plan `planner/audio`), routed under
     `PlanOperationKind::TranscodeAudio`.
   - The worker `TranscodeAudioSettings` gains additive
     `#[serde(default)]` `target_channels: Option<u64>` and `add_track: bool`
     (ADR 0013 evolution contract); `add_track = true` selects the synthesize
     ffmpeg shape (`-map 0 -c copy` + an extra mapped, downmixed `-ac` stream)
     and the synthesized-stream-aware verifier.

   This keeps `voom_core`, the shared control-plane workflow/plan/expansion/
   binding files, and `voom-plan/src/compliance` untouched — avoiding collisions
   with the concurrently-landing backup work (#278), which edits those files.

3. **Lineage.** The synthesized stream is registered as a new snapshot audio
   stream whose parent is the source stream. Its output-fact channel count is
   the target (e.g. 2), so lineage records a derived — not preserved — track.

## Consequences

- 5.1 + stereo companion layouts are expressible and executable.
- The worker's channel-preservation verifier is now conditional: preserved for
  transcode (`add_track = false`), target-count-checked for synthesize.
- `synthesize` is no longer a deferred keyword; `verify` (implemented by #273)
  is untouched and still the only remaining deferred execution operation is
  none — the deferred-operation error is retained for genuinely unknown block
  operations.
- Because synthesis rides the `transcode_audio` kind, no worker capability
  vocabulary changes; existing ffmpeg workers advertise the same operation.

## Considered & rejected

- **New `OperationKind::SynthesizeAudio`.** Cleaner separation, but edits the
  fixed `voom_core` operation vocabulary and the shared control-plane
  workflow/plan/expansion/binding routing — the same files #278's backup work
  touches — for no functional gain over the mode flag. Rejected to avoid the
  cross-agent conflict and the vocabulary churn.
- **Flat `synthesize audio to <codec> channels <n> [where <filter>]`.** Matches
  the existing `transcode audio` shape and needs no block parsing, but the issue
  specifies the block form and the block cleanly separates *source selection*
  (`from`) from *target settings*. Adopted the block form.
- **Multi-output extraction (#99).** Out of scope; tracked separately.
