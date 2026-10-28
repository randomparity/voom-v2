# AC-3 encoding (#759)

## Problem and authority

Issue #759 and parent #758 require AC-3 independently of lossless targets.
Frozen authority: issue #759 WORK:SCOPE token `q759-4fccefe2`.
Existing audio policy, planning, protocol and FFmpeg seams retain ownership;
no ownership transition, migration, new container or scheduling behavior is needed.

## Design

Extend canonical target strings with `ac3` in ordinary transcode and strict-downmix
synthesis. Preserve the current request/result shape and `default` profile.
Use the existing deterministic bitrate contract: AC-3 gets 96 kbps/channel,
therefore mono=96, stereo=192, 5.1=576 kbps. Restrict AC-3 ordinary inputs to
known mono/1, stereo/2, and 5.1 or 5.1(side)/6 facts. Reject absent/inconsistent
facts, other layouts/counts, and 7.1 before output creation. AC-3 canonicalizes
5.1 to 5.1(side); this is documented, with channel count preserved.
Synthesis remains strictly downmixing: AC-3 targets only 1, 2, or 6 channels;
explicit 7.1→5.1 produces a six-channel non-default companion and copies originals.
Do not extend the separate compatibility-track policy from #765/PR #766.

Reuse bounded existing preflight encoder discovery. Audio encoder detection is
optional at startup, including existing AAC/libopus, so AC-3 use does not require
unrelated audio encoders. Record availability for aac/libopus/eac3/ac3; production
configuration receives the observed set. Before file observation/execution, the
handler checks only the requested encoder and returns CONFIG_INVALID with its
name and a remedy to choose a worker/build providing it. No fallback/install.
Extraction still requires libopus when requested; check it before output work.
Video encoder and muxer startup prerequisites remain unchanged.

FFmpeg command generation maps `ac3` directly, retains stream-copy defaults,
metadata, staging validation, input observation, post-run source immutability,
result identity/lineage and existing transaction/commit paths. Verify AC-3 output
codec, supported layout and expected channel count from physical probe facts.
No new durable fields are added; upgrade worker/control-plane binaries before
loading policies naming ac3; old binaries reject the new token.
See [ADR 0113](../../adr/0113-ac3-audio-constraints.md).

## Evidence and alternatives

FFmpeg 8.1.3 `ffmpeg -hide_banner -h encoder=ac3` advertises mono/stereo/5.1
and 5.1(side). Generated 48kHz MKV encodes at 96/192/576 kbps exit zero;
ffprobe returns ac3, 1/2/6 channels and exact requested bitrates, with surround
canonicalized to 5.1(side). Keep the established per-channel API rather than
adding settings fields for one bounded codec. Requiring ac3 at startup would
prevent workers serving unrelated operations; deferring discovery to execution
would lose the existing bounded readiness observation.

## Success

Policy fixtures compile and plan ac3 transcode and synthesis using existing
payloads. Generated mono/stereo/5.1 media traverses the authenticated worker
HTTP boundary and yields verified MKV AC-3 at the chosen bitrates. A generated
7.1 source synthesizes a 5.1 companion while preserving source bytes, original
streams/defaults, metadata and stream identities. Negative tests prove missing
encoder, unsupported/missing/mismatched layout/count facts fail before output
creation. Existing AAC/Opus/EAC3 and compatibility regressions remain green.

## Failure model

- Actors and deployments: local operators and authenticated worker requests;
  Linux/macOS CI with generated media and installed FFmpeg/FFprobe.
- Invariants/assets: source bytes, unaffected streams, metadata/dispositions,
  channel preservation, non-default companions, typed identity and commit boundaries.
- Accepted classes: malformed/non-media inputs may fail during probing without
  output; process/mux failures may leave staging artifacts under existing cleanup.
  No claim of lossless fidelity for lossy AC-3.
- Covered elsewhere: lossless facts/settings #760; codecs #761–#763; aggregate
  matrix/CLI acceptance #764; library acceptance #339; byte ownership ADR0050.

## Threat model

- Added boundaries: none. Widened boundaries: policy target tokens, authenticated
  worker payloads, and untrusted media probe facts used to construct FFmpeg argv.
- Actors: operators control policies/media; authenticated dispatch controls codec
  strings. Executable paths and installed FFmpeg build remain operator-trusted.
- Controls: existing closed policy/protocol validation; literal encoder mapping;
  checked AC-3 channel/layout pairs; argv rather than shell; existing staging
  confinement/no-overwrite/source checks; bounded draining subprocess helpers.
- Outside scope: decoder sandboxing and hostile local executable replacement
  remain existing deployment concerns; authentication/transport are unchanged.

## Validation

Focused protocol/policy tests pin acceptance, bitrate and generated fixture plans.
Worker unit tests pin readiness, missing encoder ordering, layout/channel rejection,
output facts and unchanged encoder settings. Real generated HTTP worker tests
assert physical codec/layout/bitrate, original stream copying, source hash,
metadata/dispositions and companion identities. No optional skips for AC-3.
Run focused suites first, then `just source-checks`, `just fmt-check`, `just lint`;
assembled integration and final candidate `just ci` are recorded separately.
