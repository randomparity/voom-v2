# Issue #272 — Language-filter semantics for untagged tracks and zero matches

Closes #272. Closes #158. Decision record: ADR 0021.

## Problem

A language-filtered policy (the flagship "keep only my language" shape) misbehaves
on real libraries:

1. A single audio stream with **no** language tag makes `LanguageIn` return
   `Err(InsufficientSnapshotFacts)`, which hard-blocks planning for the whole
   target (`crates/voom-plan/src/planner/audio/selection.rs:113-118`,
   `crates/voom-plan/src/planner/remux/selection.rs:85-90`). Real libraries are
   full of untagged tracks, so the policy blocks on a large fraction of files.
2. A remux `keep audio` that matches zero tracks strips all audio. The `KeepTracks`
   arm in `crates/voom-control-plane/src/remux/selection.rs:57-61` removes every
   audio stream then extends with the (possibly empty) match set, with no guard —
   producing an audio-less artifact. This is issue #158: "no matching track ⇒ does
   voom delete the file?" The answer must be "never empty audio."

The transcode/extract audio path already guards zero matches
(`AudioPlanningBlock::ZeroMatches`). The gap is the remux `keep` path.

## Decision (see ADR 0021)

1. **Untagged ⇒ `und`.** In both filter evaluators, `LanguageIn` treats a `None`
   language as `und` and matches normally. No block; `language in ["und"]` opts in
   to untagged tracks. `LanguageIn` stops producing `InsufficientSnapshotFacts`.
2. **Per-file `Warning` diagnostic.** When a language filter runs against a
   snapshot with an untagged stream of the filtered kind, the planners attach a
   `Warning` `PlanningDiagnostic` with new code `UntaggedTrackLanguageDefaulted`.
   Node status is unchanged (a `Planned` node stays `Planned`).
3. **Zero-match `keep` never empties audio.** The remux execution selector enforces
   a result invariant: a source with audio must retain ≥1 kept audio stream, else
   `VoomError::Config` (per-file failure → `terminal_failure` issue + skip via
   ADR 0018). Scoped to audio; subtitles may go to zero.

## Plan (tightly coupled; implemented directly with TDD in one session)

All work is on branch `feat/language-filter-semantics-272`. Guardrails before every
commit: `just fmt-check`, `just lint`, `just check-test-layout`, `just test`; full
`just ci` before first push. Commit trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

### Task A — `und` fallback in the two evaluators
Files: `crates/voom-plan/src/planner/audio/selection.rs`,
`crates/voom-plan/src/planner/remux/selection.rs` (+ sibling `_test.rs`).
- **First, enumerate every assertion of the old behavior** so no case is missed:
  `rg -n "InsufficientSnapshotFacts|language" crates --glob '*_test.rs'` and scan
  golden fixtures (`crates/voom-plan/fixtures/`) for any language filter paired
  with an untagged (no `language` key) stream. Update each to the `und` semantics.
- Replace `stream.language.as_ref().ok_or(InsufficientSnapshotFacts)?` in the
  `LanguageIn` arm with `stream.language.as_deref().unwrap_or("und")`.
- Update the two remux tests that assert the old block
  (`remux_language_filter_missing_fact_blocks_planning`,
  `remux_and_evaluates_later_missing_facts_after_false_child`) to the new
  semantics; the second keeps its "And surfaces a later insufficient after a false
  child" intent using a still-insufficient selector (missing `codec`).
- New tests (fixtures): untagged ⇒ excluded by `["eng"]`, matched by `["und"]`;
  `und`-tagged behaves identically to untagged; a no-match language yields
  `Ok(false)` not an error. Cover both evaluators.
- Acceptance: `evaluate_*_filter(LanguageIn, untagged)` returns `Ok(false)` for a
  non-`und` value set and `Ok(true)` when `und` is in the set; never `Err`.

### Task B — per-file untagged warning diagnostic
Files: `crates/voom-plan/src/diagnostic.rs` (+ `_test.rs`),
`crates/voom-plan/src/planner/audio/mod.rs`,
`crates/voom-plan/src/planner/remux/mod.rs`.
- Add `PlanningDiagnosticCode::UntaggedTrackLanguageDefaulted`
  (`"untagged_track_language_defaulted"`); extend `diagnostic_test` exhaustive
  as_str coverage.
- "References a language selector" is a **recursive** walk over `And`/`Or`/`Not`
  (mirror the existing `*_has_unsupported_*` walkers), so `not (language in [...])`
  and nested boolean filters count.
- The untagged predicate is over the **filtered kind's streams present in the
  snapshot**, regardless of whether they survive selection — the untagged stream
  the filter *excludes* is exactly the one we defaulted to `und`, so it must still
  trigger the warning.
- Audio: in `plan_transcode` / `plan_extract`, when the filter references a
  language selector and any audio stream in the snapshot is untagged, attach a
  `Warning` diagnostic (only on non-`Blocked` nodes).
- Remux: in `plan_group`, when a `KeepTracks`/`RemoveTracks` op's filter references
  a language selector and any snapshot stream of the op's target kind is untagged,
  attach one `Warning` diagnostic.
- Acceptance: a plan over an untagged-audio file with a language filter carries the
  warning; node status stays `Planned`/`NoOp`. No warning when all streams tagged.

### Task C — zero-match keep guard (never empty audio)
Files: `crates/voom-control-plane/src/remux/selection.rs` (+ `_test.rs`).
- **First, enumerate every affected assertion**: `rg -n "keep|remove|audio"` across
  `crates/voom-control-plane/src/remux/*_test.rs` and
  `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`, and the
  golden fixtures, to find any test that keeps/removes audio down to zero and
  expects success. The full `just test` run is the acceptance gate, not focused
  tests.
- After the track-actions loop and video re-add, if the source has ≥1 audio stream
  but the keep set has 0 audio streams, return
  `VoomError::Config("remux would leave the file with no audio; no audio track survived the track filters")`.
- New tests (fixtures): `keep audio where language in ["fra"]` on an eng/spa file →
  error (`ConfigInvalid`, "no audio"); a matching keep still succeeds; a
  video-only source (no audio) is unaffected; a zero-match subtitle keep is allowed.
- Acceptance: no selection is ever returned with zero kept audio when the source
  had audio.

### Task D — control-plane execution selectors inherit `und`
Files: `crates/voom-control-plane/src/{audio,remux}/selection.rs` (`_test.rs` only).
- These call the shared evaluators, so `und` fallback is inherited; add tests
  proving untagged audio flows through (transcode admits it under `["und"]`;
  remux keep under `["und"]` keeps it; under `["eng"]` excludes it → guard fires).

### Docs
- ADR 0021 + README row (done).
- DSL reference "Track-filter language semantics" subsection in
  `docs/specs/voom-control-plane-design.md` (done).

## Rollback / cleanup

Pure behavior change in four selection modules plus a new (non-durable) diagnostic
code and docs. No migration, no durable-payload inventory change (ADR 0013
untouched). Reverting the branch fully restores prior behavior.
