# 0021 — Language-filter semantics for untagged tracks and zero-match keeps

## Status

Accepted

## Context

Real libraries are full of tracks with no language tag. Two behaviors made a
language-filtered policy (the flagship "keep only my language" shape) misbehave
on such libraries:

1. **A missing language tag hard-blocked the whole operation.** In both filter
   evaluators — `crates/voom-plan/src/planner/audio/selection.rs` (`LanguageIn`)
   and `crates/voom-plan/src/planner/remux/selection.rs` (`LanguageIn`) — a
   `stream.language` of `None` returned `Err(InsufficientSnapshotFacts)`. A single
   untagged audio stream therefore blocked planning for the entire target, so the
   policy failed on a large fraction of a real library rather than on the one odd
   file. The control-plane execution selectors
   (`crates/voom-control-plane/src/{audio,remux}/selection.rs`) call the same two
   evaluators, so both the planning and execution paths inherited the block.

2. **A zero-match `keep` on audio could strip all audio.** In
   `crates/voom-control-plane/src/remux/selection.rs` the `KeepTracks` arm removes
   every stream of the target and then extends the keep set with the (possibly
   empty) match set, with no guard. `keep audio where language in ["fra"]` against
   a file with no French audio produced a valid-looking selection that kept zero
   audio streams — an unplayable artifact. This is the concern open issue #158
   raised: "if no track matches the preferred language, does voom delete the
   file?" The answer must never be "yes."

The transcode/extract audio path already guards the zero-match case
(`AudioPlanningBlock::ZeroMatches`, surfaced as an error by
`crates/voom-control-plane/src/audio/selection.rs`). The gap was specific to the
remux `keep` path.

`und` (ISO 639-2 "undetermined") is already a first-class, valid language token
in the DSL: the policy compiler's `validate_language_tokens` accepts
`eng` / `und` / any 3-letter lowercase ASCII code (ADR context in the #271 DSL
grammar work). So a policy author can already write `language in ["und"]`.

## Decision

### 1. A missing language tag matches as `und`

In both filter evaluators, `TrackFilter::LanguageIn { values }` treats a stream
whose `language` is `None` as the language `und` instead of returning
`InsufficientSnapshotFacts`. Evaluation is otherwise unchanged: the stream matches
iff `und` is in `values`.

Consequences of this single rule:

- `keep audio where language in ["eng"]` on an untagged stream: `und` is not in
  `["eng"]`, so the untagged stream does **not** match and is excluded — without
  blocking, and without stripping the tracks that *do* match.
- `keep audio where language in ["und"]` (or `language == "und"`) explicitly
  keeps untagged tracks. Operators opt in to untagged audio deliberately.
- `LanguageIn` no longer produces `InsufficientSnapshotFacts` at all. The
  `And`/`Or` "insufficient" bookkeeping stays intact for the selectors that can
  still be genuinely unknown (`codec`, `channels`, `title`, `commentary`).

`und` is chosen over a dedicated "unknown language" state because the DSL already
gives `und` meaning as a real, addressable language code; mapping absent tags onto
it keeps the filter algebra total (every stream evaluates to a definite
match/no-match) with no new selector vocabulary.

### 2. A missing tag emits a per-file `Warning` planning diagnostic

Defaulting an untagged track to `und` is a judgment the operator should see, not a
silent transform (AGENTS.md Rule 12). When a target's language filter is evaluated
against a snapshot that contains at least one untagged stream of the filtered
kind, the planners
(`crates/voom-plan/src/planner/{audio,remux}/mod.rs`) attach a
`Warning`-severity `PlanningDiagnostic` with a new code
`UntaggedTrackLanguageDefaulted`. The warning is per-file (per target) and does
**not** change node status — a `Planned` node stays `Planned`. It surfaces in the
plan's `diagnostics` array (the agent-facing JSON surface), so an operator can
see which files carried untagged tracks the language filter treated as `und`.

`PlanningDiagnosticCode` is planning output, not a durable typed DB column (it is
absent from `docs/payload-contract-inventory.md` /
`scripts/payload-contract-scope.txt` and unreferenced by `voom-store`), so adding
a variant is additive and outside the ADR 0013 deny-unknown-fields contract.

### 3. A zero-match `keep` never yields empty audio

`crates/voom-control-plane/src/remux/selection.rs` enforces one invariant after
applying all track actions and re-adding video: **if the source snapshot has at
least one audio stream, the resulting keep set must retain at least one audio
stream.** Otherwise it returns `VoomError::Config` with a specific message
(`"remux would leave the file with no audio; no audio track survived the track filters"`).

This is an invariant on the *result*, checked once, rather than a special case
inside the `KeepTracks` arm. It is the last line of defense that builds the
concrete selection, so it also rejects an explicit `remove audio` that would empty
the audio. It is scoped to **audio**: a file with zero audio is unplayable, while a
file with zero subtitles is a normal, safe outcome, so subtitle keeps that match
zero are left to produce an empty subtitle set.

Returning an error (rather than blocking at plan time) is deliberate: the file is
attempted and fails **per file**. The terminal-failure machinery (ADR 0018 / T5)
turns that failure into a `terminal_failure` issue and the file is skipped, while
other files in the same operation proceed. Blocking at the planner would risk
withholding the whole operation instead of isolating the one bad file.

## Consequences

- The flagship "keep only my language" policy runs across a real library:
  untagged files are handled predictably instead of blocking planning, and files
  with no matching audio fail individually (issue + skip) instead of producing a
  silently audio-less artifact.
- Both the planning and execution paths change from the two shared evaluators; no
  duplicate logic.
- One new `PlanningDiagnosticCode` variant; no migration, no durable-payload
  inventory change.
- `keep subtitle where language in [...]` that matches zero still yields an empty
  subtitle set (unchanged, safe).
- Two `voom-plan` remux unit tests that asserted the old block-on-untagged
  behavior are updated to the new `und` semantics (they encoded the bug).

## Considered & rejected

- **Keep blocking on a missing language tag.** Rejected: this is the exact
  real-library failure the issue exists to remove; a policy must not block an
  entire target because one stream lacks a tag.
- **Introduce a distinct "unknown language" match state / new selector.** Rejected
  (AGENTS.md Rule 3): `und` already exists as a valid DSL language code and
  captures the meaning. A new state would force the filter algebra to model a
  third truth value and expand the vocabulary for no gain.
- **Scope the zero-audio guard to the `KeepTracks` arm only.** Rejected in favor of
  a result invariant: checking the final keep set is simpler, catches an explicit
  `remove`-all-audio too, and states the actual contract ("never an empty-audio
  plan") directly instead of inferring it from the action shape.
- **Also guard subtitles against zero matches.** Rejected: a subtitle-less file is
  a valid, playable outcome, and stripping non-preferred subtitles is a legitimate
  policy goal. Erroring there would break intended use.
- **Block the file at plan time on a zero-match keep.** Rejected: the issue wants a
  *per-file* failure that opens an issue and skips that file, not a broad planning
  block. The execution-boundary error isolates the bad file (ADR 0018) while the
  rest of the operation proceeds.
- **Emit the untagged-tag signal only as a `tracing::warn!` log line.** Rejected:
  the CLI is agent-facing and its structured `diagnostics` array is the durable,
  queryable surface; a stderr log is easy to miss and not agent-native.
