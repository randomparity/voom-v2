---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0020 — E-AC-3 audio transcode target and deterministic audio bitrate

## Context

The V1 DSL grammar advertises `transcode audio to aac|opus [where …]`
(`docs/specs/voom-control-plane-design.md`). The reference real-media library
(#269) requires an **E-AC-3 5.1** audio outcome, which the current pipeline
cannot produce: `eac3` is rejected at three independent layers and appears
nowhere in the workspace.

- **DSL validator** (`voom-policy` `compile/validate/operations.rs`) gates the
  transcode-audio header on `matches!(prefix, ["transcode","audio","to",
  "aac"|"opus"])`.
- **Worker request guard** (`voom-ffmpeg-worker` `handler.rs`
  `validate_transcode_audio_contract`) errors unless the target codec is
  `aac`/`opus`.
- **Encoder-arg map** (`voom-ffmpeg-worker` `ffmpeg.rs` `audio_encoder`) maps
  only `aac` → `aac` and `opus` → `libopus`.

The compiler already extracts the codec token generically
(`token_string(&tokens, 3, "opus")`), and the planner and control plane carry
`target_codec` as an opaque string. So `transcode audio to eac3` already
compiles into a plan payload and dispatches — it is only the three closed token
sets above that reject it.

A second, pre-existing defect compounds this: `TranscodeAudioSettings.profile`
is defined on the worker request but **never read**. No audio bitrate or quality
argument is ever emitted, so audio output quality is left entirely to ffmpeg's
version-dependent encoder defaults. This is invisible for a lossy re-encode of
stereo, but it is unacceptable for a 5.1 E-AC-3 target, whose fidelity is
bitrate-dominated and where a stereo-oriented default would starve the surround
channels.

## Decision

Open E-AC-3 as a first-class transcode-audio target and make `profile`
load-bearing by resolving it to a **deterministic, channel-scaled bitrate**
emitted for every transcode-audio target codec.

### E-AC-3 token

- Add `eac3` to the DSL validator's transcode-audio codec set, alongside `aac`
  and `opus`. The grammar delta is recorded as a **V1.1 amendment** in
  `docs/specs/voom-control-plane-design.md`.
- Add `eac3` → `eac3` (ffmpeg's native E-AC-3 encoder) to `audio_encoder`. The
  native encoder ships in every standard ffmpeg build (it is not an external
  library like libx265/libsvtav1), so — matching how `aac` is treated — no
  encoder-availability preflight guard is added.

### Deterministic audio bitrate wired through `profile`

- `profile` names an audio quality profile. Exactly one profile exists today:
  `"default"` — the only value the control plane emits
  (`audio/worker_contract.rs`). A new shared helper
  `audio_target_bitrate_kbps_per_channel(codec, profile)` in `voom-worker-protocol`
  resolves `(codec, profile)` to a **per-channel** target bitrate, returning
  `None` for any unsupported codec or profile.
- Per-channel defaults (kbps/channel): `aac` 64, `opus` 48, `eac3` 96. These are
  conservative, broadly-transparent targets; the codec-specific values reflect
  relative coding efficiency (Opus < AAC < E-AC-3 for equal quality).
- The ffmpeg worker emits `-b:a:<ordinal> <N>k` per selected stream, where
  `N = per_channel × source_channels`. The source channel count comes from the
  input ffprobe (`SourceAudioFact.channels`). This makes the emitted bitrate a
  function of the probed channel layout, so a 5.1 (6-channel) source receives 6×
  the per-channel budget (e.g. E-AC-3 5.1 → 576 kbps) while stereo receives 2×.
- When ffprobe does not report a channel count for a selected stream (rare for a
  decodable audio stream), the worker assumes **stereo (2 channels)** for the
  bitrate computation. This is a bounded, documented fallback that always yields
  a valid deterministic bitrate; it does not affect channel *preservation*,
  which is verified independently (below).
- The worker guard (`validate_transcode_audio_contract`) rejects a request whose
  `(target_codec, profile)` does not resolve, before any ffmpeg process starts.
  `run_ffmpeg_transcode_audio` re-resolves and fails loud if the combination is
  unsupported, so the public function is safe when called directly.

This wiring applies **uniformly** to aac, opus, and eac3. The value of
`profile` now changes ffmpeg arguments for every transcode-audio target, which
is the point: it replaces reliance on ffmpeg's version-dependent audio defaults
with a reproducible, channel-appropriate bitrate.

### 5.1 (6-channel) preservation

Channel-count preservation is a **codec-agnostic** invariant already enforced in
the output-probe path: `verify_preserved_audio_metadata` requires
`source.channels == output.channels` for every selected stream (when the source
reports a channel count), so a 6-channel input that emerged as anything other
than 6 channels is an `OutputFactsMismatch`. ffmpeg's native eac3 encoder
preserves the source channel layout by default, so no eac3-specific channel
handling is added; instead the invariant is locked for E-AC-3 by an explicit
5.1 unit test (stub ffprobe reporting `channels: 6` in and out) and an
env-gated real-ffmpeg conformance test that generates a 5.1 source and asserts a
6-channel eac3 output.

## Consequences

- `transcode audio to eac3 [where …]` compiles, plans, and executes end-to-end.
  A golden validator test pins acceptance; `transcode audio to flac` stays
  rejected.
- Audio transcodes now emit a deterministic `-b:a`. This is a **behavior change
  for existing aac/opus** targets (previously no bitrate arg). The project is
  pre-release, so there is no compatibility window; the change is a strict
  improvement in reproducibility. Existing arg-capture unit tests are updated to
  assert the new bitrate arg and to use the canonical `"default"` profile.
- `audio_target_bitrate_kbps_per_channel` is the single source of truth for the
  codec/profile set on the worker side; the DSL validator keeps its own token
  set because `voom-policy` does not depend on `voom-worker-protocol` (they are
  peers over `voom-core`). The duplication is pre-existing (aac/opus already
  lived in both) and each side has a test.
- No new durable payload fields: `profile` already exists on
  `TranscodeAudioSettings`, so ADR 0013's deny-unknown-fields contract is
  untouched.

## Considered & rejected

- **A flat per-codec bitrate (not channel-scaled).** Rejected: the entire point
  of this issue is a 5.1 E-AC-3 outcome, and a stereo-oriented flat bitrate
  starves the surround channels while a surround-oriented one bloats stereo.
  Per-channel scaling is the smallest rule that is correct for both.
- **A full audio `EncoderDescriptor` mirroring the video encoder-capability
  system (crf/preset/tune/…).** Rejected as premature abstraction: the audio
  transcode operation has no inline-profile grammar and only one profile exists.
  A one-line `(codec, profile) → kbps` table is the honest amount of mechanism.
- **An eac3 encoder-availability preflight guard + `FfmpegConfig` field.**
  Rejected: E-AC-3 is a native ffmpeg encoder present in every standard build;
  `aac` has no such guard either. Adding an unused detection field would be dead
  code, and a guard with no `has_audio_encoder` call site is scope creep.
- **Add eac3-specific channel-preservation code.** Rejected: preservation is
  codec-agnostic and already enforced; duplicating it for eac3 would be
  redundant. It is instead exercised by an eac3-specific test.
- **Fail loud when ffprobe omits a selected stream's channel count.** Rejected
  as too strict for a robustness edge that does not affect correctness: the
  stereo fallback yields a valid bitrate and channel preservation is checked
  separately. Revisit if a real input ever surfaces this.
