# Isolate content-driven normalization failures (#818)

## Problem and authority

One invalid media fact currently invokes `normalize::malformed`, which creates
`MalformedWorkerResult`; handler translation preserves it and the strict scan pump fails the
session. A diagnostic replay of `rejects_malformed_disposition_values` at cbbdb185, changing
only the expected class to `MalformedMedia`, failed that assertion (cargo exit 101).
The frozen charter is #818 comment 6565306448, token q818-3a5fb481; exclusions are explicitly
empty, approved by the operator on 2026-11-01. Parent #787 informs reliability and evidence
authority; #339 owns complete-library acceptance. This design implements those two #818 criteria.

## Decision and ownership

Keep classification in the ffprobe normalizer, using existing `WorkerError` variants. Keep
`From<WorkerError> for FfprobeError`, the guarded probe caller, and `classify_single_file_error`
as their existing owners: neither the pump nor a caller guesses causes from error text.
ADR [0127](../../adr/0127-content-normalization-failures.md) records the distinction.
No protocol shape/version, durable schema, dependencies, or success snapshots change.

Three approaches: classify at the producing predicates (selected); translate every normalization
failure at the handler (would hide provider defects); or forgive `MalformedWorkerResult` in the
pump (would also hide unrelated worker contract failures). The selected approach changes only
classification of rejected values whose JSON representation is compatible with ffprobe fields.
First validate the existing checked provider structure/types across the complete document; only
then interpret content values. A provider defect wins when both kinds occur, independent of
field/stream ordering. Validate no previously ignored field, and preserve existing sentinels.

## Error-site inventory

| Site / rejected input | Classification after change |
|---|---|
| root not object; format not object; streams not array; stream not object | MalformedWorkerResult: provider structure |
| disposition not object; disposition flag neither boolean, number nor string | MalformedWorkerResult: provider field type |
| compatibility ID not string | MalformedWorkerResult: provider field type |
| empty/whitespace compatibility ID; conflicting case-folded IDs (both ID tag names) | MalformedMedia: file tag content |
| disposition numeric or string value other than 0/1, on default/forced/comment/attached_pic | MalformedMedia: file disposition content |
| float duration string fails parsing; parsed NaN/infinity (format and stream) | MalformedMedia: file numeric content |
| unsigned string fails parsing; numeric value negative/fractional/out of u64 (index/width/height/channels/sample_rate/bit_rate) | MalformedMedia: file numeric content |
| numeric field neither string nor number | MalformedWorkerResult: provider field type |
| audio numeric field neither string nor number; sample_fmt/channel_layout not string | MalformedWorkerResult: provider field type, checked before shared audio parser |
| audio sample_rate exceeds u32; raw/coded precision exceeds u8; raw precision exceeds coded; precision exceeds s16/s32 decoded capacity; malformed unsigned strings | MalformedMedia: file audio facts |
| snapshot still exceeds bound after raw removal | MalformedMedia, unchanged (ADR 0122) |
| snapshot JSON serialization; finite float cannot become JSON number | MalformedWorkerResult: internal result construction, unchanged |
| raw ffprobe bytes fail JSON parsing in run_ffprobe_json | MalformedWorkerResult, unchanged |

The finite-float representability guard is unreachable for serde_json's finite f64 values;
retain its fail-loud internal classification. Preserve unknown/sentinel omission and ADR 0125's
attached-picture flag behavior. Ignored fields and their permissive behavior are unchanged.

## Success and validation

C1: table-driven sibling normalizer tests cover the content and provider rows above, plus mixed
content/provider errors across container/stream and within an audio stream, including
both compatibility tags, every disposition key, both durations, unsigned field call sites, and
shared audio parser error branches. Existing bounded-snapshot tests remain proof of ADR 0122.
Handler tests send content and malformed provider JSON through a real worker HTTP stream and
assert terminal error class/code. C2: scan_session_test uses the existing production control
plane/router/SQLite fixture; its probe boundary supplies raw JSON through the real normalizer
and `FfprobeError::from` translation for one file and valid probe evidence for another.
Assert succeeded session, failed_content_count=1, rejected observation with no identity evidence,
and valid file evidence persisted. A provider-contract counterpart, including a document with
both invalid content and a provider defect, must fail the session.
These exercise production behavior without requiring external media-tool installation.
Run focused regressions red before implementation, relevant crate suites, formatting and lint,
then the mandatory final `just ci` before pushing. Root owns the shared build-cache lease;
#835 must land before the final suite because it fixes a known baseline warning.

## Global Constraints

Rust toolchain stays pinned at 1.95. No dependency or toolchain additions. Follow sibling unit
test layout, real-time SQLite tests, existing worker failure vocabulary, node-owned byte access,
strict frame/evidence bounds, and JSON protocol envelopes. No target architecture is declared;
this host is x86_64 GNU/Linux; CI supplies existing cross-platform coverage.

## Failure model

- Actors and deployments: local operators and CI using bundled ffprobe workers and owner-node
  scan sessions; media files may be damaged or malicious; provider output may be defective.
- Invariants and assets: rejected content publishes no authoritative identity; unrelated files
  can publish on completed scan; provider/internal defects remain fatal; source bytes untouched.
- Accepted failure classes: content may be recorded without identity, as #818 and ADR 0122
  require; missing/sentinel metadata keeps existing omission semantics.
- Covered elsewhere: #834 owns stderr bounds; ADR 0122 owns result-size bounds; #339 owns full
  flagship media acceptance; existing protocol validation owns frame/transport defects.

## Threat model

- Boundary inventory: media-controlled values enter ffprobe JSON, normalizer, error frames,
  then the existing scan pump and durable observation API. No new or widened entry point.
- Actor model: a media author controls bytes/tags; the installed provider is trusted to honor
  field representation. Provider defects are untrusted data at the normalizer boundary.
- Controls: retain structural/type validation and fatal provider errors; reject invalid content
  as MalformedMedia; emit no snapshot on failure; retain existing byte/frame/evidence bounds.
- Explicitly out of scope: source-file mutation defenses and credential/transport security stay
  with existing guards/protocol; provider stderr resource bounds belong to #834.
