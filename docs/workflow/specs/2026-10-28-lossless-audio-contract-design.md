# Lossless audio settings and verified facts — #760

Authority: [#760 frozen scope](https://github.com/randomparity/voom-v2/issues/760#issuecomment-6474621237), native epic #758, and operator approval on 2026-10-28. Decision: [ADR 0114](../../adr/0114-lossless-audio-facts-contract.md).

## Problem and boundary

Issue760 is the contract prerequisite for codec children761–763. Existing selected output facts carry codec/channels/layout and metadata; encoding requires a profile resolving to lossy bitrate. Layout already exists, making that portion of issue evidence stale. Source normalization omits integer precision/sample format. Do not enable unfinished target names in policy, planning, readiness, encoders, or worker handlers.

Existing AAC/Opus/EAC3/AC3 requests retain their shape when new optional fields are absent. Same-channel compatibility from765 and AC3 semantics from759 are protected. Source files, copied streams, metadata, defaults, lineage, and transaction/publication boundaries remain governed by existing paths.

## Architecture

Add optional `lossless: Option<LosslessAudioSettings>` to protocol `TranscodeAudioSettings`, omitted when absent. Define `LosslessAudioSettings { bits_per_sample: Option<u8> }`: absent precision preserves the selected source; present precision requests16/24 representation and cannot reduce known source precision. No bitrate, arbitrary format string, sample-rate knob, remix knob, compression knob, or profile CRUD.

Define non-durable `ResolvedLosslessAudioSettings { sample_rate: u32, channels: u32, channel_layout: String, bits_per_sample: u8 }`, resolved separately for each selected source. This avoids imposing uniform rate/layout across multiple selected streams. Define ephemeral `AudioSampleFacts` holding optional observed rate, format, precision, channels, and layout, used by source/output parsing and verification. Add optional `sample_rate`, `sample_format`, `bits_per_sample` to serialized `AudioOutputStreamFact`; existing `channel_layout` remains authoritative and optional for old/ordinary lossy data.

Protocol owns the bounded fact resolver and verifier because workers and test support consume the same contract. FFprobe normalization calls the shared raw probe parser, persists observed sample evidence, and retains original probe JSON as today. FFmpeg source and output probes call it; control-plane normalized result reconstruction preserves checked optional facts. No plan/schema migration or new scheduling behavior is needed while no codec child emits lossless settings. Existing envelope constructors explicitly leave lossless absent. Test support adds an internal normal dependency on protocol, a support-only edge.

## Precision and layout rules

Collection distinguishes absence from malformed data. Zero ffprobe bit counts mean unavailable. Prefer positive `bits_per_raw_sample`, otherwise positive `bits_per_sample`; never derive significant precision from storage width alone. Reject contradictory counts beyond the decoder sample-format capacity. Retain unknown absent facts for current lossy behavior; archival resolution requires them.

Initial archival contract accepts known16/24 significant bits in signed integer s16/s16p/s32/s32p, with significant width no larger than the representation. Reject float/double,8/32bit archival sources, unknown sample format, zero/missing rate, missing precision, and missing/incompatible channel/layout pairs. Known layout pairs: mono/1, stereo/2,5.1/6,5.1(side)/6,7.1/8. Distinguish side/back layouts even at equal channel counts. Rate is a positive u32; codec-specific ranges remain child-owned.

Ordinary archival resolution preserves source rate/channels/layout and source precision unless explicit widening16→24 was requested. A24→16 request fails. Resolved output verification requires exact resolved rate/layout/channels/precision and validates the observed integer representation again. Shared helpers return actionable errors; they do not write files or mutate durable state. Explicit strict-downmix synthesis remains a separate requested transform and creates a non-default companion; its codec implementation belongs to the children and cannot claim source/sample equivalence.

While this prerequisite ships, current lossy codec handlers reject a present lossless settings object before source/output mutation, and unfinished targets remain rejected. This proves no optional contract can silently alter existing behavior. The resolver/verifier execute in focused tests as the prerequisite surface.

## Canonical comparison helper

Add a dev-only test-support helper taking FFmpeg/FFprobe paths, a file, and a provider stream index. Probe the selected stream, validate known integer facts through the shared resolver, then decode exactly that stream to signed interleaved little-endian32bit PCM. Do not set rate/channel count or remix filters; disable dither explicitly. Compare complete decoded sample bytes and exact rate/channels/layout; validate each side's precision but allow representational padding16→24. Codec output precision itself remains the production verifier's separate contract.

Use tiny generated WAVEFORMATEXTENSIBLE fixtures with known stereo masks, significant16/24bit values including signed extrema,0,+/-1 and channel-distinct content. Compare an FFmpeg re-encoded/remuxed supported fixture with the original, and a second fixture differing in one least-significant source bit. Require exact equality for the first and a sample-mismatch error for the latter. Include unequal-rate/layout failures and float/unknown-fact rejection. Required FFmpeg/FFprobe absence fails loudly; no local skips.

Experimental evidence: FFmpeg8.1.3 encoded PCM16/24MKV decoded to s32le exactly matches expected left-aligned samples. Raw PCM MKV lacks probed layout despite explicit stereo input; this remains a concrete constraint for763. Explicit WAVEFORMATEXTENSIBLE fixtures report stereo with known16/24precision and avoid inventing layout by count. This proof is helper evidence, not advertised PCM product support.

## Versioning and deployment

Bump exact `PROTOCOL_VERSION`4→5; handshake and operations reject the old version through existing negotiation. Additive fields default absent and skip absent serialization; old additive settings/facts JSON remains readable. Concrete serde structs deny unknown fields. Update payload inventory/scope only for new durable nested structs/modules, not new columns. Before the protocol swap, quiesce workflow ticket creation and complete or explicitly operator-disposition unfinished protocol-4 media work using the [release drain procedure](../../release-process.md); persisted `media_dispatch.schema` must exactly match the binary, so old schema-4 envelopes deliberately reject. This is deployment guidance, not authority to perform a drain here. Deploy coordinated binaries before writing new facts/settings; an old binary rejects newly written unknown fields. Rollback after new writes requires restoring the pre-upgrade database. ADR 0114 records the additive request contract and codec boundaries.

## Success and validation

- Old request/result JSON round-trips unchanged; unknown new nested fields reject; exact protocol4 and persisted schema-4 media envelopes are rejected after the bump, while old additive settings/facts JSON stays readable.
- Known16/24facts resolve and verify; missing facts, float/unsupported precision, contradictory layout/precision, and precision reduction fail.
- FFprobe normalization and FFmpeg output collection retain observed rate/format/precision from generated real media.
- Current supported codec set stays AAC/Opus/EAC3/AC3; lossless-on-lossy rejection leaves source/output paths untouched.
- Canonical helper executes with real tools; equality and one-LSB negative proof pass.
- Focused crate tests, sourcechecks, formatting, lint, required precommit hooks and one final `just ci` pass on final candidate; CI separately green.

## Failure model

- Actors/deployments: bundled same-release workers/control plane, local operators, Linux/macOS CI, owned tiny generated test fixtures.
- Invariants/assets: decoded integer samples, known rate/layout/precision, old persisted request/result readability, source immutability, protected lossy/default/lineage semantics.
- Accepted classes: unfinished codec requests fail explicitly; incomplete historical sample evidence remains readable but cannot satisfy archival resolution. Test helper is for generated fixtures, not arbitrary long untrusted media processing.
- Covered elsewhere: codec mappings/readiness/real archive and synthesis execution761–763; aggregateCLI/matrix764; real library339; AC3/readiness759; owner-byte migration416–425/ADR0050; arbitrary decoder security is FFmpeg's existing boundary.

## Threat model

- Added boundary: optional request lossless object from authenticated dispatch; strict serde and explicit handler rejection govern it until supported codecs implement it.
- Widened boundary: FFprobe JSON observation and durable optional output facts; checked numeric conversion, explicit unknown handling, and integer/layout validation govern them. No shell interpolation; subprocess arguments remain separate.
- Actors/trust: media may be malformed; FFmpeg/FFprobe are operator-provisioned trusted tools; scheduling/authentication remain existing protocol middleware responsibilities.
- Out of scope: new routes/auth, worker install/fallback, arbitrary user flags, long-running canonical comparison service, codecchild support guarantees.
