# Issue #336: Language-ranked remux defaults design

## Goal

Make the published strategy forms executable:

```text
defaults audio: best
defaults subtitle: best
```

For each file, `best` selects one retained stream of the requested kind using
the compiled policy's ordered `config.languages` list and canonical source
order. Planning resolves the winner once and carries its snapshot stream ID to
execution. Explicit filter-addressed defaults remain authoritative.

## Existing contracts

- `config.languages` is a validated `Vec<String>` in `CompiledConfig`; source
  order is preserved and old compiled string forms deserialize into that type
  under ADR 0035.
- Remux planning applies ordered keep/remove actions before resolving defaults.
- `defaults ... where` already carries a resolved
  `selected_snapshot_stream_id` in the typed remux payload.
- The control plane validates the pinned ID against the pinned media snapshot
  and sends exact default/set-clear vectors to the worker.
- Provider stream indexes are unique canonical source-order keys. Snapshot JSON
  order is not semantic.
- A missing language tag evaluates as `und`; a present non-string value is
  malformed.

No grammar, compiled-policy shape, wire field, migration, or dependency is
needed.

## Ranking

For one unshadowed `DefaultStrategy::Best` action:

1. Build candidates from streams that survived ordered keep/remove actions and
   whose kind equals the action target.
2. Sort candidates by ascending provider stream index.
3. If there are no candidates, omit the action from the remux payload. This is
   valid for subtitles. The control plane's existing final-audio guard still
   rejects an execution that would remove every source audio stream.
4. If `config.languages` is empty, select the first candidate. No language fact
   is needed because the policy supplied no language preference.
5. Otherwise, read the language fact for every candidate before ranking. Map a
   missing language tag to `und` and reject any present malformed language fact
   as insufficient snapshot facts, even if another candidate would win.
6. Give a candidate the zero-based position of its language in
   `config.languages`. Languages absent from the list share a fallback rank
   after every configured language.
7. Select the candidate with the lowest `(language rank, provider stream
   index)` pair.

The fallback rank makes a file with no configured-language match deterministic:
its first retained source stream wins. Duplicate preference entries, if present
in legacy compiled data, have the rank of their first occurrence and do not
change the result.

The same algorithm applies independently to audio and subtitle actions.

## Precedence and reduction

Defaults operations reduce per target before ranking:

- no explicit filter-addressed action and zero or one strategy action: the
  strategy remains, and `best` resolves as above;
- no explicit action and multiple strategy actions including `best`: the file
  blocks with an actionable conflicting-strategies diagnostic rather than
  allowing source order to decide how a resolved winner composes with another
  outcome;
- no explicit action and multiple legacy strategies without `best`: preserve
  the existing ordered behavior;
- one explicit action: it is the only effective action for that target, so all
  strategy actions, including `best`, are discarded without reading language
  facts;
- multiple explicit actions: the file blocks with the existing actionable
  unsupported-shape diagnostic.

After #336, `best` is a supported remux candidate. The temporary candidate gate
that rejected an unshadowed `best` is removed. The outer planner no longer
needs to precompute explicit targets solely to admit a shadowed `best`; the
remux resolver owns both precedence and support.

## Payload and execution boundary

A resolved `best` action uses the existing payload shape:

```json
{
  "target": "audio",
  "strategy": "best",
  "selected_snapshot_stream_id": "stream-2"
}
```

`selected_snapshot_stream_id` means a planner-resolved default selection,
whether its source was an explicit filter or `best`. The control plane treats
the selected ID as authoritative and does not rank again. An unresolved `best`
payload remains invalid at execution, so manually constructed or stale
payloads cannot select a different fallback.

When the target has no retained candidates, the planner omits the default
action rather than serializing `best` without an ID.

The additive payload predates a distinct provenance field, so the trust boundary
uses the published lowering invariant:

- `preserve` plus a selected ID is a resolved explicit filter action;
- `best` plus a selected ID is a resolved ranked action; and
- a selected ID on `first` or `none` is invalid.

The control plane then repeats the planner's per-target reduction. Multiple
explicit actions block; one explicit action discards all strategies; without an
explicit action, multiple strategies block when at least one is `best`. Legacy
multi-strategy payloads without `best` retain their existing behavior.

## Compliance and warnings

The planner's existing default-state comparison consumes the resolved ID.
Exactly that stream must be default and every other retained stream of the kind
must be non-default. A matching snapshot is `NoOp`; any mismatch is `Planned`.

When a non-empty language preference list causes `best` to evaluate at least
one untagged candidate as `und`, planning emits the existing
`UntaggedTrackLanguageDefaulted` warning. A shadowed `best` and an empty
preference list do not evaluate language and therefore do not emit that warning
on their own.

## Generated-media acceptance

The remux fixture contains:

- an English non-commentary audio stream;
- a later Spanish non-commentary audio stream;
- an English commentary audio stream;
- ordinary and forced subtitle streams; and
- a font and non-font attachment.

The policy removes commentary, prefers `["spa", "eng"]`, orders the selected
Spanish audio first, and uses `defaults audio: best`. The produced MKV must
retain both non-commentary audio streams, make only Spanish default, preserve
the unselected English stream, remove commentary and forced subtitle tracks,
retain only the font attachment, use the expected track order, and replan as
`NoOp`.

## Failure behavior

- Missing stream inventory, duplicate provider indexes, or malformed language
  used for non-empty preference ranking blocks with
  `insufficient_snapshot_facts`.
- Multiple unshadowed strategy actions for one target block with a conflicting
  defaults-strategies diagnostic when at least one action is `best`.
- No configured language match falls back to the first retained source stream;
  it is not an error.
- No retained target stream emits no default action.
- An unresolved `best` reaching the control plane is a configuration error.
- Explicit defaults never consult or yield to ranking.

## Compatibility and rollback

This change is semantic only. Old compiled policy versions, remux payloads, and
durable events keep their existing shapes and remain readable. After a rollback,
the old planner again blocks an unshadowed `best` when generating a new plan.
An already-materialized resolved `best` operation remains executable by the old
control plane because `selected_snapshot_stream_id` predates this change and
the old selector already treats a present selected ID as authoritative. Pending
ranked-default work therefore continues across downgrade. The published CLI and
API expose no job-cancellation action, so rollback is not a way to stop that
work: operators must either let it drain before downgrade or quiesce workers and
leave it pending. The missing operator cancellation surface is tracked
independently in #364 and is not added by this issue. No migration or data
rewrite is required.

## Test strategy

- Planner tests cover both audio and subtitle `best`. The audio matrix covers
  language-list priority, same-language ties, unmatched and empty-list fallback,
  zero candidates, shuffled snapshot arrays, and `NoOp` replanning; a focused
  subtitle case proves the same target-independent resolver selects and pins the
  configured-language winner.
- Fact-boundary tests prove that non-empty preferences validate every retained
  candidate: a malformed non-winner blocks, and any missing language emits the
  `und` warning. With empty preferences, malformed or missing languages are not
  read, the first retained stream wins, and no warning is emitted. A shadowed
  `best` likewise ignores malformed or missing language facts and emits no
  warning in both source orders.
- Reduction tests block `best` combined with another same-target strategy in
  both source orders while preserving legacy multi-strategy behavior that does
  not include `best`.
- Control-plane tests accept a resolved `best`, reject an unresolved `best`,
  reject invalid selected-ID shapes and `best` conflicts, preserve legacy
  non-`best` composition, and prove no second ranking occurs. Acceptance of the
  unchanged resolved payload shape covers execution of already-materialized
  work across rollback.
- The generated-media remux flow inspects exact audio defaults/order,
  commentary removal, attachment and subtitle dispositions, and compliant
  replanning.
- Focused strict Clippy and the repository-wide `just ci` gate remain clean.

## Out of scope

- Per-kind language preference lists or scoring beyond ordered language then
  source order.
- Audio execution and lineage work in #99, #337, and #333.
- Parser aliases, new defaults strategies, or unpublished DSL forms.
- Other campaign-excluded issues unless required for this behavior.
