# Sample policy defaults (#827)

## Problem

The flagship and language-cleanup samples require exactly one English audio
track and one forced subtitle. Real media with several retained English tracks
or no forced subtitle therefore cannot plan (#827).

## Scope

Use the existing `defaults audio best` and `defaults subtitle preserve` strategies
in both committed fixtures. Keep language filters and every other phase intact.
The operator approved this remedy and empty exclusions on 2026-11-01 in campaign
`eea194a5a966-6c546866-0556-4c27-aee2-0e72f70f565c`; scope token `q827-bd8b8f48`.
ADR 0023 governs existing best resolution and exact-one filter semantics.
Reuse the planner's owner and existing direct consumer `sample_policies_plan.rs`;
no ownership transition or new engine behavior is needed. Update fixture comments
and the operator real-media runbook's sample descriptions. #339 continues to own
final flagship real-media acceptance; #794's defaults-free canary is historical.

### Failure model

Supported: valid synthetic media snapshots representing multiple retained English
audio tracks, optional unknown-language audio, and zero or several non-forced
subtitles. Existing best ranking must choose deterministically by configured
language preference then provider index; it does not classify commentary.
Preserve leaves subtitle defaults unchanged, including zero or multiple defaults.
Engine validation and explicit-filter exact-one contracts remain unchanged. Best
omits empty-group actions; execution still rejects stripping all source audio.

## Success

Both samples plan without defaults-cardinality blocks on the supported snapshots.
The normalize payload pins existing best audio selection and subtitle preservation,
including source-order tie-breaking and an already-compliant no-op. All previous
sample-policy tests and exact-one defaults negative tests remain green. The
runbook explains best/preserve behavior without claiming real-corpus acceptance.

## Validation

- Contract: committed fixtures handle multiple English audio tracks and no forced
  subtitle. Mode: focused-test; extend `sample_policies_plan.rs` with store-backed
  cases for both samples, varied subtitle defaults and source order. Expected red:
  normalize blocks on the former audio/subtitle filters. Green command:
  `cargo test -p voom-control-plane --test sample_policies_plan`.
- Contract: exact-one filters stay strict. Mode: focused-test; existing planner
  defaults negative cases remain unchanged; command
  `cargo test -p voom-plan track_remux_filter_default` must retain zero/many-match failures.
- Surface: fixture comments and runbook prose. Mode: task-test-not-applicable;
  no executable consumer validates this explanatory wording; review it against
  the exercised payloads rather than creating a prose-search test.
- Run `just fmt-check`, `just lint`, mandatory commit hooks and final `just ci`.
