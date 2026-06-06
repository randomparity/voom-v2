# ADR 0011 — Audio-transcode plannability does not gate on per-stream preservation facts

- Status: Accepted
- Date: 2026-06-05
- Issue: #184 (follow-up to #167 / PR #183)
- Related: ADR-0007 (phase-barrier coordinator), the Sprint 16 closeout
  "Residual strictness" note (`docs/superpowers/specs/2026-06-05-voom-sprint-16-closeout.md`)

## Context

The audio-transcode planner refuses to **plan** a `transcode audio` operation
unless every selected audio stream carries `language` **and** `title` **and**
`channels` **and** a `commentary` disposition fact. The gate is
`has_transcode_preservation_facts`
(`crates/voom-plan/src/planner/audio/selection.rs`), consulted by
`transcode_audio_shape` (planner) and
`transcode_selection_from_payload_and_snapshot` (control-plane runtime
selection). A stream missing any of the four makes the phase
`Blocked(AudioPlanningBlock::InsufficientSnapshotFacts)`.

Real-world media frequently has audio streams with **no `title` tag** — muxers
do not synthesize one. PR #183 taught the ffprobe normalizer to lift `tags.title`
and `disposition.comment` *when the source carries them*, but deliberately left
the gate itself untouched (the closeout's "Residual strictness" note tracked the
gate as a follow-up, this issue). So an otherwise-valid audio transcode on
title-less media still blocks with `snapshot stream facts are insufficient for
audio planning`, and the Sprint 16 combined-flow fixture has to bake a synthetic
audio `title` into every track purely so the audio phase can commit.

The natural fix is "drop `title` from the gate." Investigating the gate's
purpose showed the defect is broader: **none of the four facts is needed to plan
or build a transcode.** The worker request the planner produces,
`TranscodeAudioSelection` (`crates/voom-worker-protocol/src/operations/audio.rs`),
carries only `selected_streams: Vec<AudioStreamRef>`, and `AudioStreamRef` is just
`snapshot_stream_id` + `provider_stream_index`. No per-stream descriptive fact —
not `language`, not `channels`, not `title`, not `commentary` — is sent to the
worker. The issue's framing that "language and channels are needed to build the
operation" does not hold against the worker contract.

The genuine plannability floor already exists, enforced **independently** of this
gate inside `transcode_audio_shape`: a known source `codec` (`selection.rs`,
blocks when `codec.is_none()`) and a known `container` (blocks when absent).
Everything `has_transcode_preservation_facts` checks is *preservation
completeness*, and the downstream already treats every one of those facts as
preservation-only, tolerant of absence:

- `worker_contract.rs` validates the transcode output's `language` / `title` /
  `channels` / `commentary` by *equality against the source* — a `None` source
  requires a `None` output (nothing to preserve, nothing to invent).
- `commit.rs` writes each fact onto the committed snapshot only when the source
  fact is `Some`.
- `extract` plannability is gated separately by `extraction_role` (which
  legitimately needs a known commentary disposition); it is unaffected.

So the gate conflates *preservation completeness* with *plannability*. Only the
gate over-couples them; nothing downstream requires the facts to be present.

## Decision

**Remove the `has_transcode_preservation_facts` gate entirely.** Audio-transcode
plannability is exactly the floor `transcode_audio_shape` already enforces — a
known source codec and a known container. All per-stream descriptive facts
(`language`, `title`, `channels`, `commentary`) become pure preservation
passthrough: carried to the output and the committed snapshot only when the
source supplies them, validated by equality at the worker boundary (absent ⇒
absent, never invented), never blocking planning.

- `transcode_audio_shape` (`voom-plan`) drops the
  `!has_transcode_preservation_facts` block; its existing `codec` and `container`
  checks remain and become the sole, explicit plannability floor.
- `transcode_selection_from_payload_and_snapshot` (`voom-control-plane`) drops its
  symmetric `selected.iter().all(has_transcode_preservation_facts)` assertion. That
  path never required a source codec (the planner owns that floor) and builds the
  worker request purely from stream references, so nothing it does unwraps a
  now-ungated fact.
- The function and its re-exports (`voom-plan` `planner/audio/mod.rs`, `voom-plan`
  `audio.rs`) are deleted, not renamed — there is no remaining gate for a renamed
  predicate to express.

The behaviour is locked at two levels:

- **Deterministic unit lock** (`voom-plan`): `transcode_audio_shape` returns
  `Planned` (not `Blocked`) for a stream missing `title` / `commentary` /
  `language` / `channels`, and still `Blocked(InsufficientSnapshotFacts)` when the
  source `codec` is absent — pinning that codec, not preservation completeness, is
  the floor.
- **Real-media proof** (`voom-control-plane`, ffmpeg-gated): the Sprint 16
  combined-flow fixture stops baking audio `title` metadata, and the
  `remux → transcode → audio` chain still commits all three phases against
  title-less audio.

## Consequences

- A `transcode audio` phase plans and commits against real media whose audio
  streams have no `title` tag — the issue's acceptance criterion — and, more
  generally, against any stream with a known codec, regardless of which
  descriptive tags the muxer happened to write. The combined-flow fixture no
  longer needs its synthetic-title workaround.
- Every per-stream descriptive fact is now genuinely optional end to end: absent
  on input ⇒ absent on output ⇒ absent on the committed snapshot, with no
  validation error. Present on input ⇒ still preserved and still validated by
  `worker_contract.rs`. No preservation guarantee is weakened for media that
  *does* carry the metadata; the change only stops *blocking* on its absence.
- Planning is more permissive than before: a stream missing `language` or
  `channels` now plans where it previously blocked. This is safe — the worker is
  driven by stream references, output validation is `None`-tolerant, and a later
  phase that genuinely needs `language` (e.g. a `lang in [...]` filter) still
  blocks *at that filter's own* `InsufficientSnapshotFacts` check, not here.
- The named pinning test
  (`transcode_preservation_facts_require_language_title_channels_and_commentary`)
  is removed along with the function, as the issue anticipated. The contract
  change is recorded here and in the closeout spec's resolved note.

## Considered & rejected

- **Drop only `title` (and `commentary`), keep `language` + `channels` in the
  gate** — the issue's literal Option 1. Rejected: the worker request carries no
  per-stream fact, so `language` and `channels` are not build inputs either.
  Keeping them would be an arbitrary preservation-completeness gate dressed up as
  a build requirement — the exact false rationale the worker contract disproves —
  and would still block real media whose audio lacks a `language` tag (`und` /
  untagged tracks are common) for no plannability reason. Drawing the line at the
  facts the planner *actually* needs (codec + container) is the principled floor.
- **Option 2 — keep the facts as gate inputs but default a missing one to absent
  instead of blocking.** Rejected: the fields are already `Option` and every
  consumer already tolerates `None`, so "default to absent" is just "do not gate
  on them" with an extra no-op transform. Removing the gate states that intent
  directly.
- **Option 3 — confirm the strictness is intentional and document at the DSL
  surface that audio transcode needs titled streams.** Rejected by the issue's
  acceptance criteria (title-less media must plan and commit) and by the
  substance: no per-stream tag is a transcode input, so documenting a requirement
  with no technical basis would institutionalize the bug.
- **Keep a reduced `has_transcode_planning_facts(codec, container)` predicate**
  rather than deleting the gate. Rejected: `codec` and `container` are not
  per-stream descriptive facts and are already checked, explicitly and in place,
  inside `transcode_audio_shape`. A separate predicate would duplicate those
  checks and re-introduce the same conflation under a new name. Deleting the gate
  and leaving the floor where it already lives is the smaller, clearer surface.
