# ADR 0011 — Audio-transcode plannability does not gate on preservation-only facts

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

The gate conflates two distinct contracts:

1. **Plannability** — what the planner needs to *build* the transcode operation.
2. **Preservation** — what is *carried through* to the output if the source has
   it.

`language` and `channels` are plannability inputs; `title` and `commentary` are
preservation-only. A title-less stream can transcode fine — it simply has no
title to preserve on the output. The downstream code already treats title and
commentary as preservation-only and tolerates their absence:

- `worker_contract.rs` validates the transcode output's title/commentary by
  *equality against the source* (`actual.title != expected.source.title`), so a
  `None` source requires a `None` output — "nothing to preserve," not "must
  invent a title."
- `commit.rs` writes `title`/`commentary` onto the committed snapshot only when
  the source fact is `Some`.
- `extract` plannability is gated separately by `extraction_role`, which is
  unaffected.

Only the planner gate over-couples the two contracts.

## Decision

**Narrow the gate to the plannability facts only: a selected audio stream is
plannable for transcode when it has `language` and `channels`. `title` and
`commentary` are no longer required to plan; they remain pure preservation
passthrough, carried to the output only when the source supplies them.**

The function is renamed `has_transcode_preservation_facts` →
`has_transcode_planning_facts` to keep the name truthful: after the change it
gates plannability, and the two facts named "preservation" are exactly the ones
it no longer checks. A function still called `…preservation_facts` that returns
`true` for a stream with no title and no commentary would actively mislead.

The behaviour is locked at two levels:

- **Deterministic unit lock** (`voom-plan`): the renamed
  `transcode_planning_facts_require_language_and_channels` test asserts the gate
  passes with `title: None` / `commentary: None` and fails only when `language`
  or `channels` is absent; `transcode_audio_shape` returns `Planned` (not
  `Blocked`) for a title-less, commentary-less stream.
- **Real-media proof** (`voom-control-plane`, ffmpeg-gated): the Sprint 16
  combined-flow fixture stops baking audio `title` metadata, and the
  `remux → transcode → audio` chain still commits all three phases against
  title-less audio.

## Consequences

- A `transcode audio` phase plans and commits against real media whose audio
  streams have no `title` tag — the issue's acceptance criterion. The
  combined-flow fixture no longer needs its synthetic-title workaround, removing
  a piece of test scaffolding that existed only to satisfy the over-strict gate.
- `title` and `commentary` become genuinely optional end to end: absent on input
  ⇒ absent on output ⇒ absent on the committed snapshot, with no validation
  error. Present on input ⇒ still preserved and still validated by
  `worker_contract.rs`. No preservation guarantee is weakened for media that
  *does* carry the metadata.
- The named pinning test changes, as the issue anticipated. That is the intended
  contract change, recorded here and in the closeout spec's resolved note.
- The rename touches the function's two call sites and its re-exports
  (`voom-plan` `planner/audio/mod.rs`, `voom-plan` `audio.rs`, `voom-control-plane`
  `audio/selection.rs`); a mechanical, compiler-checked rename with no behavioural
  effect beyond the gate narrowing.

## Considered & rejected

- **Option 2 — keep `title`/`commentary` as gate *inputs* but default a missing
  one to absent instead of blocking.** Rejected as functionally identical to the
  chosen option but more indirect: the fields are already `Option`, and every
  consumer already tolerates `None`, so "default to absent" is just "do not gate
  on them." Narrowing the gate states that intent directly; a defaulting layer
  would add a no-op transform the downstream does not need (and AGENTS Rule 6 —
  code answers, do not add indirection).
- **Option 3 — confirm the strictness is intentional and document at the DSL
  surface that audio transcode needs titled streams.** Rejected by the issue's
  acceptance criteria, which require title-less media to plan and commit, and by
  the substance: title is cosmetic metadata, not a transcode input. Documenting a
  requirement that has no technical basis would institutionalize the bug.
- **Drop only `title`, keep `commentary` in the gate.** Rejected: `commentary` is
  preservation-only for *transcode* by the same argument as `title` — it is a
  disposition flag carried through, not an input the planner needs to build the
  operation. (Extraction is different — `extraction_role` legitimately needs a
  known commentary disposition to classify the bundle role — but that path is
  gated separately and is untouched.) Dropping `title` while keeping `commentary`
  would leave the same class of real media — title-less *and*
  commentary-disposition-less streams — blocked for no plannability reason.
- **Keep the name `has_transcode_preservation_facts` and only change the body.**
  Rejected: the name would then describe the opposite of what the function does
  (it would pass streams with no preservation facts). Honest naming is cheap here
  — the rename is mechanical and compiler-checked — and the misnomer would mislead
  every future reader of the gate.
