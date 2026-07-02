# Issue #271 — DSL V1 grammar conformance

## Problem

Four forms in the spec's V1 grammar (`docs/specs/voom-control-plane-design.md`
lines 640–692) fail today. The policy compiler's validator rejects them, so the
spec's own examples do not compile:

1. `language == <quoted-token>` (track-filter) — only `language in [...]` is
   accepted (`crates/voom-policy/src/compile/validate/conditions.rs`,
   `is_valid_track_filter`). The spec example
   `keep audio where language == "eng" and not commentary` fails validation.
2. `media.container == <token>` and `media.duration_millis <op> <number>`
   (condition) — the `media.` field-path root is unrecognized
   (`is_core_field_root` / `is_valid_core_field_path`). Only `container.*` and
   `video.duration_millis` are accepted.
3. `where` is mandatory on `transcode audio to aac|opus` and `extract audio`
   (`crates/voom-policy/src/compile/validate/operations.rs`,
   `validate_required_track_filter`), where the spec brackets it optional.

The root cause is that the fixture suite tracks the implementation, not the spec,
so these gaps went unnoticed.

## Key finding — the lower and evaluate layers already conform

Investigation of the pipeline shows the gap is almost entirely in the **validator**:

- **`media.*` paths** are already resolved by the planner's field-path evaluator
  (`crates/voom-plan/src/planner.rs`, `canonical_field_path`): `media.container`
  → container name, `media.duration_millis` → duration. No evaluator change.
- **Omitted `where`** already lowers to `filter: None` (`track_filter` returns
  `None` when no ` where ` is present), and `TranscodeAudio` / `ExtractAudio`
  already carry `filter: Option<TrackFilter>`. The planner's
  `selected_audio_streams(snapshot, None)` already selects **all** audio streams.
  No lowering or planner change.
- **`language == <token>`** is the one form needing a lowering change: the
  existing `filter_from_text` has no `language ==` arm, so it would silently lower
  to `None`.

## Decision

Make the validator accept the three forms, and add the single missing lowering
arm. Reuse existing compiled representations — introduce no new `TrackFilter`,
`CompiledCondition`, or `CompiledOperation` variants.

### Form 1 — `language == <token>`

- **Validate** (`is_valid_track_filter`): accept
  `["lang" | "language", "==", value]` where `value` is a single token (quoted
  or bare).
- **Validate the language code.** The existing `validate_language_tokens`
  (`conditions.rs`) only iterates bracketed `list_values`, so it does **not**
  cover the `==` single-value form: without a fix, `language == "english"` or
  `language == "zz"` would validate and lower to `LanguageIn { values:
  ["english"] }`, silently matching zero streams (a fail-quiet footgun that
  violates AGENTS.md Rule 12). `validate_language_tokens` must be extended to
  also read the `language == <token>` RHS and apply the same
  eng/und/3-letter-lowercase-ASCII rule, emitting `InvalidLanguageCode`
  otherwise. A negative conformance case (`language == "english"` ⇒
  `InvalidLanguageCode`) pins this.
- **Lower** (`filter_from_text`): map `language == <token>` to
  `TrackFilter::LanguageIn { values: vec![<token>] }` — a single-language `in`
  set is exactly the semantics of an equality match, so no new variant is needed.
- Scope: only `==` for `language`, matching the spec's track-filter production
  (which lists `language == <quoted-token>` but not `!=`). Negation remains
  reachable via `not language == "eng"`. `codec ==` is **not** added — the spec
  track-filter grammar only offers `codec in [...]`.

### Form 2 — `media.container` / `media.duration_millis`

- **Validate**: add `"media"` to `is_core_field_root`, and a `"media"` arm to
  `is_valid_core_field_path` accepting exactly `container` and `duration_millis`
  (the only two `media.` fields in the grammar).
- Lower/evaluate: unchanged — `condition_from_text` already produces
  `FieldComparison { path: ["media", ...] }` and the planner already resolves it.

### Form 3 — optional `where` on `transcode audio` / `extract audio`

- **Validate**: when a ` where ` clause is present, validate the track-filter as
  today; when absent, accept the statement. Applies to
  `transcode audio to aac|opus` and `extract audio`. `keep` / `remove` keep
  `where` mandatory (the spec brackets `where` optional only for transcode/extract).
- Lower/plan: unchanged — absent filter already means "all audio tracks".

## Semantics of an unfiltered `extract audio`

`extract audio` with no `where` selects all audio streams; the planner's
`extract_audio_shape` already blocks with `MultipleMatches` when more than one
stream is selected. This is the existing, spec-consistent behavior (single-stream
extraction), so no new failure mode is introduced — an unfiltered extract on a
multi-audio file fails to plan exactly as an over-broad filter does today.

## Verification — spec-conformance fixture suite

The issue's central ask is a suite that exercises **every** grammar production
verbatim from the spec, decoupled from implementation-tracking fixtures. Add a
sibling `*_test.rs` conformance module in `voom-policy` that, for each production
in spec lines 640–692:

- compiles a minimal policy using the production, and
- asserts it validates without diagnostics and lowers to the expected compiled
  form (spot-checking the three newly-fixed forms against their exact compiled
  output).

The suite includes the three previously-failing forms plus the already-working
productions, so future grammar drift in either direction is caught.

## Non-goals

- No new grammar (that is the V1.1 work in #275/#276/#277).
- No change to language-filter *evaluation* semantics for untagged / no-match
  tracks — that is #272.
- No `codec ==`, no `!=` for language, no `media.*` fields beyond the two in the
  grammar.
