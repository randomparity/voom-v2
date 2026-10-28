# Primary audio compatibility (#765)

## Authority and scope

Frozen external charter: issue #765, `WORK:SCOPE` token `q765-dd13d5e5`,
comment 6457264164. Campaign identity
`a67063986e67-5237a867-97a9-4316-ab73-4fea16379237`.
The operator approved the exact exclusions and then the concrete two-stage
design in the interactive campaign on 2026-10-28. [ADR 0111](../../adr/0111-primary-audio-compatibility.md)
records the bounded extension. Classification non-trivial, full-spec,
complexity L, fixed denominator 1000 changed lines, iterating branch review.

The exclusions and owners remain: encoder expansion/readiness — #758 and
#759–#764; real-library execution/reports — #339; ordinary synthesis defaults —
#737/ADR 0110; general media framework or unrelated extraction/transcode
refactoring — future scoped issue owned for reconsideration by campaign
operator; control-plane byte ownership expansion — ADR 0050 and separate
approved architecture work. No unresolved product ambiguity.

## Problem and success

Existing synthesis matches multiple sources and requires fewer target channels;
existing explicit remux selectors require one unambiguous match. They cannot
express the requested primary compatibility workflow without a deliberate
contract extension.

For generated MKV fixtures and owner-node workflow execution:

1. Existing language retention keeps English and unknown/untagged audio,
   including commentary, and removes other audio languages (C1).
2. Ensure chooses one original non-commentary source: English first, then
   unknown/untagged, with provider index as deterministic tie-breaker (C1).
3. Retain originals and create only the required target: AAC mono/stereo with
   source channels; supported EAC3 3–6-channel layouts with source channels;
   7.1 becomes EAC3 5.1. Reuse an already-compatible selected original (C2).
4. Placement puts the resolved compatibility track first within audio and
   makes it the sole audio default. Retained commentary, video, subtitles,
   attachments and applicable metadata survive; existing non-audio relative
   order remains intact (C3).
5. Missing eligible sources, invalid required facts, unsupported layouts and
   invalid companion associations block with explicit diagnostics (C4).
6. Dispatch retries, interruption between ensure/place and repeat application
   do not accumulate companions. Tests inspect final bytes and actual probe
   order/defaults, not only plan/request JSON (C4–C5).

## Policy and deterministic selection

Add two parameter-free, strict operations, represented by annotated newtype
content structs in `CompiledOperation`: `EnsureAudioCompatibility` and
`PlaceAudioCompatibility`. Reject extra arguments. Preserve existing DSL forms.
The executable sample uses dependent phases:

```text
phase retain {
  keep audio where language in ["eng", "und"]
}
phase compatibility {
  depends_on: [retain]
  ensure audio compatibility
}
phase placement {
  depends_on: [compatibility]
  place audio compatibility
}
phase verify {
  depends_on: [placement]
  verify artifact
}
```

The planner owns ranking and target resolution in a dedicated audio
compatibility module. Structural stream IDs and provider indexes must be
unique. A source needs a nonempty codec, a boolean commentary fact, positive
checked channel count and recognized layout. Missing language means `und`;
malformed present language is invalid, not silently unknown. Preserve a narrow
invalid-fact marker during probe normalization so ordinary filters retain their
existing semantics while compatibility resolution can diagnose malformed tags.
Commentary and
foreign-language streams cannot be primary sources. Derived compatibility
companions are excluded from original-source ranking.

Supported `(channels, layout)` pairs: `(1, mono)`, `(2, stereo)`,
3-channel `2.1`, `3.0`, `3.0(back)`; 4-channel `3.1`, `4.0`, `quad`,
`quad(side)`, `4 channels (FL+FR+LFE+BC)`; 5-channel `4.1`, `5.0`,
`5.0(side)`; 6-channel `5.1`, `5.1(side)`; 8-channel `7.1`.
EAC3 encoder support was observed with `ffmpeg -hide_banner -h encoder=eac3`
on FFmpeg 8.0.1. Generated MKV encode/probe experiments on that version showed
EAC3 canonicalizes `quad`, `5.0` and `5.1` to their `(side)` forms. Resolve those
three targets to the observed `(side)` form, and 7.1 to `5.1(side)`; the other
listed pairs retain their layout name. Compare already-compatible sources
using these explicit equivalents. The requirement preserves channel count and
retained originals, not a noncanonical lossy bitstream layout label.
Other layouts/counts and inconsistent pairs
block rather than relying on FFmpeg's implicit layout guesses.
The layout experiment used `ffmpeg -f lavfi -i anullsrc=r=48000:cl=5.1
-t 0.1 -c:a eac3 -channel_layout 5.1 output.mkv`, then
`ffprobe -v error -show_entries stream=codec_name,channels,channel_layout
-of json output.mkv`; repeating with each listed layout verified the table.

Resolve the compatibility track to the selected source if codec/channels/layout
already match. Otherwise look for exactly one companion explicitly associated
with this source and target; validate its identity and facts. Missing companion
means ensure plans one encoding; placement instead blocks until ensure has
published. Multiple or incompatible associated companions block explicitly.
No arbitrary same-codec track substitutes for the selected source's companion.

## Contracts and ownership

Keep policy vocabulary in `voom-policy`, deterministic resolution in
`voom-plan`, SQL/lineage in `voom-store`, authority/publication in the control
plane, and byte transformations in owner-node workers. This is a clean
extension; no existing responsibility moves and no unrelated path is removed.

Ensure uses the existing `TranscodeAudio` plan/capability and synthesis ledger,
with a distinct payload type and optional strict compatibility settings. The
payload pins selected source ID/index, resolved target and one stable derived
ID. Runtime selection recomputes those values against the pinned snapshot and
rejects disagreement. Ordinary synthesis remains strictly downmix/non-default.
The worker independently checks actual source channels/layout and resolved
settings before encoding, copies originals, appends exactly one non-default
companion, and verifies actual output target facts before reporting success.

Preserve the source stream ID on copied source metadata and the derived ID on
the new companion. Associate the derived stream explicitly with its source;
normalize these observed tags into snapshots, with checked/unique structural
validation. Existing synthesis lineage records source version/snapshot/stream
and result version/snapshot/stream. Companion reuse requires a valid explicit
association and matching source/target facts; unknown or contradictory facts
do not authorize another encoding. Retained associations and identities survive
copy-remux and repeated probing. Media tags supply observed identity within a
pinned snapshot, not authorization or a replacement for durable publication.
Observed source-ID association plus unique identities and exact target facts
governs later companion selection; the durable ledger remains publication and
audit proof, rather than a requirement that imported tags prove DB origin.

Placement resolves against refreshed committed facts and uses remux payload
actions: sole selected audio default and an additive audio-group head ID. Add
the corresponding optional selection field to the worker protocol and
`voom-mkvtoolnix-worker` ordering helper; absent means existing remux behavior.
Existing global head behavior remains unchanged. The compatibility track moves
to the first audio position while non-audio ordering remains as requested by
the existing group order. Recompute actual order/default flags at the worker's
output-validation boundary and admit only matching results.

`voom-ffprobe-worker` normalization and `voom-mkvtoolnix-worker` are direct
consumers required by C3/C4; the campaign coordinator confirmed these paths
within the frozen direct-dependency surface. Domain IDs stay typed through
repositories and publication; persisted corruption remains a database error
before domain absence/conflict classification. Use existing named transaction
helpers and preserve mutation/event/claim ordering.

## Recovery and evolution

Reuse synthesis operation key, exact claim generation, immutable staging receipt,
atomic commit and lineage finalization. A retry of the same planned operation
reuses its committed result. A later policy application recognizes the
associated companion even if placement never ran. Once placed, original ranking
excludes the derived companion and still resolves that same identity. A
compatible original needs no new lineage or encode and placement may still run.

Optional serialized fields use `#[serde(default)]`; concrete deserialized
content structs deny unknown fields. Update durable contract inventory/scope
where a typed JSON unit is added. Deploy supporting binaries before admitting
policies using new variants/fields. Reverting application code requires ceasing
new-policy admission; do not pretend old binaries parse new durable variants.
The existing ledger needs no schema migration; reservation
`migrations/0045_*.sql=0045` remains consumed and unused.

## Failure model

- Actors/deployments: interactive policy authors, pure dry-run planners,
  authenticated owner-node execution with FFmpeg/MKVToolNix/ffprobe, persisted
  policy/snapshot/lineage data, and Linux/macOS repository CI with generated media.
- Invariants/assets: retained source content and metadata, one ranked original,
  one valid compatibility identity, ordinary synthesis defaults, explicit
  placement, typed identity, owner-owned bytes, exact claims and publication
  transaction/event order. Malformed input and corrupt persisted association
  must fail explicitly before mutation; retries must not multiply companions.
- Accepted classes: unsupported layouts/counts are diagnosed rather than
  converted; lossy compatibility encoding does not promise decoded equality to
  originals; encoder-tool absence fails through existing readiness/execution
  errors rather than installing or selecting a new encoder. These are bounds
  of C2/C4, not waived required behavior.
- Other owners: encoder expansion/readiness #758/#759–#764; real-library
  deployment/reports #339; broader operation framework and byte ownership are
  the approved exclusions above. Existing ordinary synthesis remains #737/ADR0110.

## Validation

The implementation plan assigns focused red/green evidence to compiler and
durable shape contracts; ranking/target/diagnostic/no-op selection; worker
source/target/preservation checks; remux audio-group ordering/defaults;
runtime binding and publication identity/retry; and generated-media final
acceptance. Documentation uses no prose snapshot tests. Existing default
synthesis and ordinary remux regressions remain mandatory. Run assembled
integration checks, then `just ci` before pushing, with skipped tests disclosed.
