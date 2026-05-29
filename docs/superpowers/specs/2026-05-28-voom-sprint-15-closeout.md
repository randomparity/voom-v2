---
name: voom-sprint-15-closeout
description: Sprint 15 closeout evidence for named, validated, durable video encode profiles applied end-to-end through policy, planner, worker protocol, FFmpeg workers, and CLI inspection.
status: complete
date: 2026-05-28
sprint: 15
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-28-voom-sprint-15-design.md
  - docs/superpowers/plans/2026-05-28-voom-sprint-15-implementation.md
---

# VOOM Sprint 15 Closeout

Named video encode profiles (HEVC and AV1) are durable, validated, resolved
before planning, carried through dispatch to the FFmpeg workers, and inspectable
from the CLI. The matrix below maps each Section 11 acceptance criterion to the
test(s) and command that prove it.

## Acceptance Matrix

| Acceptance criterion (spec Section 11) | Command | Observed result |
|---|---|---|
| Profile model + migration: every seeded built-in present and valid; lookup-by-name; unknown name returns none; STRICT `CHECK`s reject bad rows | `cargo test -p voom-store video_profiles` | passed: 8 `video_profiles` repo/migration tests covering the six seeded built-ins, lookup, unknown name, and STRICT `CHECK` rejection |
| Per-encoder descriptor validation: CRF ranges, named vs numeric presets, tune/profile/level/pixel-format vocabularies, incompatible combinations | `cargo test -p voom-worker-protocol` | passed: 107 tests including `default_hevc_profile_serializes_minimal_superset` and per-encoder descriptor accept/reject cases |
| Named reference + valid inline both compile and plan for HEVC and AV1; each inline rejection class; mutual-exclusion error | `cargo test -p voom-policy` | passed: 122 tests across `video_profile_test`, `validate_test`, and `compiled_test` |
| Legacy bare-string `"profile": "default-hevc"` deserializes to `Named` and plans unchanged | `cargo test -p voom-policy legacy_bare_string_profile_round_trips_through_compiled_json` | passed: compiled-policy compatibility round-trip ok |
| Resolution: named resolves to typed profile; unknown name yields `CONFIG_INVALID` before planning; inline passes through | `cargo test -p voom-control-plane transcode::resolve` | passed: `resolve_test` named/unknown/inline cases ok |
| Planner is dimension-, pixel-format-, and container-aware (no-op vs planned); resource/quality estimate notes | `cargo test -p voom-plan` | passed: planner compliance and estimate-note tests ok |
| `transcode video to hevc` resolves to `default-hevc` with the unchanged FFmpeg command line | `cargo test -p voom-worker-protocol default_hevc_profile_serializes_minimal_superset` | passed: command-line invariance asserted (not byte-identical JSON) |
| Control plane resolves named profiles before planning and carries the resolved profile through dispatch (not `default_hevc()`); computes `copy_video`; rejects `copied_video` disagreement | `cargo test -p voom-control-plane transcode` | passed: `resolve_test::copy_video_*`, `mod_test::execute_rejects_copied_video_disagreement_before_commit`, `dispatch_test::validate_output_facts_rejects_copied_video_disagreement` |
| FFmpeg worker applies all fields per encoder, muxes MKV/MP4 (`hvc1`/`av01`), downscales, stream-copies on `copy_compatible`, reports `copied_video` + observed output facts; mismatches and drift fail loud; missing encoders fail at preflight | `cargo test -p voom-ffmpeg-worker` and `cargo test -p voom-ffmpeg-worker --test transcode_conformance` | passed: `ffmpeg_test` (hvc1/av01 tagging, downscale, copy), `handler_test::copy_video_*`, `preflight_test` (missing `libx265`/`libsvtav1`/`libaom-av1` rejected), and the per-encoder conformance suite against real ffmpeg |
| Worker writes only a staged output; control plane records the staged artifact, verifies, commits add-only, records result `FileVersion`/`FileLocation`/`MediaSnapshot` | `cargo test -p voom-control-plane transcode::tests::execute_records_verified_committed_transcode_result_and_events` | passed: staged → verified → committed → result snapshot recorded |
| Target-path discriminator: two profiles of the same codec+container produce distinct, coexisting targets; the same profile twice collides with `CONFIG_INVALID` | `cargo test -p voom-control-plane transcode::stage` | passed: `two_profiles_same_codec_and_container_produce_distinct_targets`, `target_path_rejects_existing_target_for_same_profile` |
| Transcode events + execution report carry resolved-profile facts (name, encoder, target codec, output container) and observed output facts (`copied_video`, width/height/pixel-format) | `cargo test -p voom-events` and `cargo test -p voom-control-plane transcode::tests::execute_records_verified_committed_transcode_result_and_events` | passed: `artifact_transcode_succeeded_payload_carries_profile_and_observed_output_facts`, `artifact_transcode_failed_payload_carries_profile_facts`, and report-field assertions |
| `voom profile list` / `voom profile show <name>` return stable JSON envelopes; unknown name returns `NOT_FOUND`; CLI goldens lock the envelope shape | `cargo test -p voom-cli --test profile_envelope` | passed: 3 insta snapshots (`profile_list`, `profile_show_hevc_archive`, `profile_show_unknown` with `NOT_FOUND` + exit 2) |
| End-to-end: scan → policy plan → execute → transcode → verify → commit → result snapshot for named and inline profiles, HEVC and AV1, MKV and MP4 | `cargo test -p voom-control-plane --test video_profile_flow` | passed: 3 cases — `named_default_hevc_mkv_flow_commits_and_replans_as_no_op`, `named_hevc_1080p_downscales_oversized_hevc_source_to_mp4`, `inline_av1_mp4_flow_commits_with_inline_discriminated_target` (real ffmpeg; preflight asserts the three encoders) |
| Documentation placeholder scan | `rg -n "default_hevc\|hevc\.mkv\|only .* hevc" docs crates --glob '!**/specs/**'` | reconciled: matches are live `default_hevc()` API calls, valid `.hevc.mkv` output names, and historical Sprint-12 plan text; no current doc asserts single-profile behavior (`docs/specs/voom-control-plane-design.md` already documents `transcode video to hevc [using profile <quoted-name>]` and named profiles) |
| `just ci` | `just ci` | passed: fmt-check, lint, check-test-layout, test, doc, deny, audit all green |

## Deferred Work

Per spec Section 12, Sprint 15 defers the `QualityScoringProfile` registry and
`QualityScore` records, user-defined profile create/update, mov_text subtitle
transcoding and attachment handling for MP4, hardware-accelerated encoders,
additional software encoders/codecs, fractional CRF, bitrate/codec ladders,
per-title automatic profile selection, and the Sprint 16 multi-phase real-media
policy workflow.
