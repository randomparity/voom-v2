# Preserve channels during E-AC-3 transcoding (#832)

## Problem and authority

Issue #832 reports a selected audio channel mismatch and a separate title mismatch on
8-channel TrueHD sources. The operator chose actionable plan-time blocking for unsupported
E-AC-3 layouts, preserving the existing channel/title contract and forbidding implicit downmix.
The campaign approved exactly empty exclusions on 2026-11-01. The frozen scope is issue #832,
WORK:SCOPE token `q832-60aa5bec`; [ADR 0130](../../adr/0130-eac3-preserved-audio-contract.md)
records the decision. #339 retains flagship production-library acceptance ownership.

## Evidence and ownership

`transcode_audio_shape` currently checks selection, container and codec, then plans an encode.
`run_ffmpeg_transcode_audio` probes source facts but only AC-3 has a layout guard.
`verify_preserved_audio_metadata` checks language, title, dispositions and source channel count;
the title check precedes the channel check. Neither failure proves the cause of the other.
FFmpeg 8.1.3/Linux advertises 16 EAC3 layouts with at most six channels. Disposable generated
FLAC 7.1/8 becomes EAC3 5.1(side)/6; FLAC 6.0/6 becomes 5.0(side)/5. Generated 5.1 and 4.0
controls preserve channel counts. Those four native command replays preserve titles. A separate two-selected-stream replay
(TrueHD5.1 and AAC stereo, distinct Unicode/plain titles, English/untagged language) preserves
each title, language and assigned snapshot identity with the production argument ordering.
The installed TrueHD encoder only produces up to 5.1, so requesting 7.1 during fixture generation
already loses channels and cannot reproduce the original 8-channel TrueHD input.

Retain ownership: planner decides plannability from normalized snapshots; worker validates the
actual selected input before encoding and output before success. A clean extension suffices;
no ownership migration, dependency, schema, operation wire, or compatibility path is introduced.

## Behavior

For an actual `transcode audio to eac3` encode, require selected source channel/layout pairs:
mono/1; stereo/2 or FC+LFE/2; 2.1/3, 3.0/3 or 3.0(back)/3;
3.1/4, 4.0/4, quad/4, quad(side)/4 or FL+FR+LFE+BC/4;
4.1/5, 5.0/5 or 5.0(side)/5; 5.1/6 or 5.1(side)/6.
The FC+LFE and FL+FR+LFE+BC spellings are ffprobe's exact
`2 channels (FC+LFE)` and `4 channels (FL+FR+LFE+BC)` strings.

Parse an optional `channel_layout` into `SnapshotAudioStreamFact` beside `channels`.
Keep selected-stream filtering, missing codec/container checks and no-op detection in their
existing order. A true no-op requires no encode and therefore needs no encoder-layout admission.
After no-op detection, an EAC3 encode with missing channel/layout facts blocks as
`InsufficientSnapshotFacts`; a known unsupported or inconsistent pair blocks as
`UnsupportedMediaShape`, with a reason explaining that EAC3 transcode preserves channels and
asking the operator to choose a preserving codec or explicit synthesis for an intentional
companion downmix. Other target codecs and unselected tracks do not acquire this requirement.
No target_channels parameter is added to ordinary transcode payloads.

The worker independently rejects unsupported/missing actual selected source pairs after source
selection and existing AC-3 validation, before constructing/running the encode command. Keep
ordinary synthesis dispatch before this new guard: explicit synthesis is already a distinct
add-track path. Leave the existing transcode metadata/channel postchecks unchanged.
The two owners use the same small literal admission table locally; no cross-crate abstraction
or new dependency is warranted for two occurrences.

Existing executable syntax is `synthesize audio from channels > 6 { codec eac3 channels 6 }`
directly inside a phase; its compiler/payload/worker path adds a non-default companion
and preserves originals (ADR0026/ADR0110). Verify this exact snippet against the current compiler
before putting it in operator guidance, then exercise 8-to-6 synthesis with real FFmpeg.
It does not replace or make a later unconditional transcode of the original 7.1 stream valid.

Investigate title preservation with the real worker over generated 5.1 TrueHD and 7.1 FLAC,
with distinct stream titles and language/disposition metadata. Retain title mismatch rejection
with an intentionally mismatching output probe. Do not change title behavior unless a verified
reproduction establishes a cause. Original canary media and probe payloads are unavailable in
this worker's authorized inputs; if synthetic checks do not reproduce that failure, report the
exact evidence/limitation for the campaign operator's criterion decision.

## Success

- Selected 7.1/8 and 6.0/6 EAC3 encodes are blocked during planning with actionable diagnostics;
  each of the 16 named supported pairs plans. Missing/mismatched facts block.
- True no-ops, unselected unsupported tracks, filters, and other target codecs retain behavior.
- Direct worker EAC3 calls reject unsupported selected pairs before output creation.
- Generated supported EAC3 transcodes preserve channel count/title/language/dispositions;
  explicit 7.1-to-5.1 synthesis preserves the original and creates the requested companion.
- Existing title/channel mismatches remain errors; original title causality is reported honestly.

## Failure model

1. Actors and deployments: local operators, authenticated owner-node workers and Linux/macOS CI;
   media authors control stream facts/tags, and stale snapshots may disagree with actual bytes.
2. Invariants/assets: selected source channel/title contract, source immutability, and promotion
   only after a verified successful result; unsupported encodes must not silently downmix.
3. Accepted failure classes: unsupported/missing EAC3 layouts are explicit blocks; optional
   descriptive fields remain optional for other codecs and true no-ops. New encoder layouts
   outside the 16-pair table require deliberate support. Original title causality may remain
   unestablished only with the operator's explicit criterion disposition after bounded probes.
4. Covered elsewhere: #339 owns complete real-library acceptance; existing identity hashes,
   staging containment, subprocess deadlines and output verification own byte identity/lifecycle.

## Threat model

- Boundaries: existing persisted snapshot to planner and external ffprobe to worker parsing;
  neither boundary is added or widened. Channel/layout values are untrusted data.
- Actors: untrusted media authors can supply arbitrary layouts/tags; executables and authenticated
  operator configuration retain existing trust. No new network or permission surface is added.
- Controls: checked existing channel conversion plus exact paired layout admission; actual worker
  re-probe before encoding, argv arrays and existing output validation. Diagnostics use fixed text.
- Outside scope: compromised executable/host and production library execution retain their existing
  owners; this change does not relax source identity, metadata checks or staging controls.

## Validation and constraints

Rust 1.95.0; no new dependencies, wire/payload fields, migrations, capabilities or toolchain floor.
Use sibling unit tests. Focused planner/worker tests prove red before the guard, then green.
Native conformance proves blocked 7.1/6.0, supported/title controls and explicit synthesis using
disposable fixtures, independent ffprobe and unchanged source hashes. Compiler proof uses the
existing SYNTHESIS fixture/test harness. Run relevant source guards, formatting, lint and
mandatory hooks; run final `just ci` only after #833 actually merges and main is integrated.
The campaign grants exclusive cache/build ownership only after reviewed design and scope audit.
