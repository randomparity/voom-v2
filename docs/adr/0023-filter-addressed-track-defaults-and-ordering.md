---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0023 — Filter-addressed track defaults, track-level ordering, and forced flag

## Context

The remux surface can only address tracks by *kind group*, not by individual
identity. Three concrete gaps block a common real-media policy — "the eng eac3
5.1 non-commentary track is audio #1, default, and the forced-narrative subtitle
is forced":

- **`defaults audio|subtitle first|best|none|preserve`** picks a default by a
  fixed *strategy* over the whole kind group. A policy cannot say "make *this
  specific* filter-selected track the default."
- **`order tracks <target-list>`** reorders by `RemuxTrackGroup`
  (video/audio/subtitle). It cannot pin one individual track ahead of its group.
- **The forced flag is not settable at all.** `RemuxSelection` has no forced
  field and `mkvmerge.rs` never emits `--forced-track-flag`, even though it
  already emits per-track `--default-track-flag id:1|0`
  (`crates/voom-mkvtoolnix-worker/src/mkvmerge.rs:365-399`).

The low-level worker plumbing for per-track *defaults* already exists: the wire
`default_streams` / `clear_default_streams` carry individual `RemuxStreamRef`s
and mkvmerge emits per-track `--default-track-flag`. The missing pieces are the
DSL surface to express filter-addressed intent, the compiled/wire schema fields
to carry it, and the worker emission for ordering and forced flags. Filter
*resolution* to a concrete stream is a planner concern.

This feature spans three crate-ownership boundaries: the DSL edges
(`voom-policy`), the wire/worker edges (`voom-worker-protocol`,
`voom-mkvtoolnix-worker`), and the middle (`voom-plan` planner resolution plus
`voom-control-plane` selection population).

## Decision

### 1. Grammar V1.1 delta (see spec amendment)

Three additive productions; every existing form is unchanged:

```text
defaults audio|subtitle where <track-filter>
order tracks [<target-list>] where <track-filter>
forced audio|subtitle where <track-filter>
```

- `defaults … where <filter>` makes the single track the filter selects the
  default for its kind group and clears the group's other defaults. The filter
  **must select exactly one track at plan time**; zero or many matches fail the
  file with a plan-time diagnostic. This is orthogonal to ordering: "default"
  and "first" are set independently and composed.
- `order tracks … where <filter>` pins the single track the filter selects to
  the head of the track order, ahead of the group order. The optional
  `<target-list>` keeps its existing group-ordering meaning for the remaining
  tracks; `order tracks where <filter>` alone pins the head track and leaves the
  rest in source order. The head filter must also select exactly one track at
  plan time.
- `forced audio|subtitle where <filter>` marks every track the filter selects
  with the forced flag and clears it on the group's other tracks. Unlike the two
  above it is not single-track-constrained: a title can have multiple forced
  tracks, and a filter that matches zero tracks is a no-op (consistent with
  `keep`/`remove` filters that match nothing), not an error. **The `forced … where`
  DSL surface and its compiled `SetForced` variant are deferred (see "Not in this
  PR"); this PR delivers forced only at the wire + worker layer.** The spec's V1.1
  amendment therefore publishes only the `defaults … where` and `order tracks …
  where` productions as compiler-accepted.

The compiler parses, validates the *shape* of, and lowers the two shipped forms,
but never counts matches because it does not see a file's streams. The planner
resolves each filter against streams retained after ordered keep/remove actions.
Zero or multiple retained matches block that file before execution with distinct
diagnostics.

Planning carries the resolved snapshot stream ID in the typed remux payload.
The control plane validates that ID against the pinned snapshot and populates
`default_streams` or `head_streams`; it does not reevaluate the filter. Explicit
filter-addressed defaults are authoritative over strategy selection for the
same target group regardless of source position. One explicit action discards
all strategy actions for that target. Multiple explicit actions for one target
are an unsupported policy shape and block the file with an actionable message.
The planner performs this reduction before payload construction, and the
control plane repeats it at the execution trust boundary. Without an explicit
action, more than one strategy action for the same target is also unsupported
and blocks the file; source order must not silently decide between conflicting
default outcomes.

A shadowed `best` action is discarded before strategy support is checked or
language facts are read. An unshadowed `best` uses `config.languages` order and
then provider stream index to resolve one retained target-kind stream. When the
preference list is non-empty, planning reads the language fact for every
retained candidate before choosing a winner. A missing language is `und`; any
present non-string language blocks the file, even when another candidate would
otherwise win. Evaluating at least one missing language emits the existing
per-file `UntaggedTrackLanguageDefaulted` warning from ADR 0021. With no
preferred-language match or an empty preference list, the first retained source
stream wins. Empty preferences do not read language facts or emit that warning.
With no retained target stream, the action is omitted.

### 2. Compiled schema (`voom-policy`, additive-only per ADR 0013)

- `SetDefaults` gains `filter: Option<TrackFilter>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`). `None`
  preserves the existing strategy-only meaning. When `Some`, the operation is
  filter-addressed and `strategy` is not consulted. The `where` form continues
  to lower `strategy` to `Preserve` as a compatibility sentinel; planning
  carries the resolved snapshot stream ID separately.
- `ReorderTracks` gains `head_filter: Option<TrackFilter>` (same serde
  attributes). `targets` keeps its meaning; `head_filter` pins one track first.
- A compiled representation for forced (`SetForced { target, filter }`) is
  **deferred** (see "Still deferred"). A new `CompiledOperation` variant forces a
  new `PlanOperationKind`, which ripples through exhaustive matches in `voom-plan`
  (`model.rs`, `planner.rs`, `compliance/report.rs`) and `voom-control-plane`
  (`policy_bridge.rs`, `cases/policy/compliance.rs`). Publishing that separate
  operation requires its own planning, execution, and acceptance work.

### 3. Wire schema (`voom-worker-protocol::RemuxSelection`, additive)

Three new `Vec<RemuxStreamRef>` fields, each `#[serde(default)]`:

- `head_streams` — streams pinned to the front of the track order.
- `forced_streams` — streams to mark forced (`--forced-track-flag id:1`).
- `clear_forced_streams` — streams to clear forced (`--forced-track-flag id:0`),
  mirroring the existing `default_streams` / `clear_default_streams` pair.

### 4. Worker emission (`voom-mkvtoolnix-worker`)

- `track_order()` emits `head_streams` first (in listed order), then the
  existing group order, then any remaining kept tracks — so a head stream pins
  ahead of its group.
- A new `extend_forced_flags()` emits `--forced-track-flag id:1` for
  `forced_streams` and `id:0` for `clear_forced_streams`, mirroring
  `extend_default_flags()` (set wins over clear on collision).

### 5. Planner and control-plane boundary

`voom-plan` owns filter evaluation, retained-stream cardinality, and the resolved
snapshot identity stored in the per-file remux payload.
`voom-control-plane/remux` owns validation of that identity against the pinned
snapshot and conversion to `RemuxStreamRef`. The MKVToolNix worker receives only
resolved stream references.

For `defaults ... best`, planning uses the same resolved snapshot-ID payload
field as an explicit filter selection while retaining `strategy: "best"` as
the source intent. Execution never repeats the language ranking. An unresolved
`best` payload is invalid at that boundary.

The existing payload has no separate provenance field. The control-plane trust
boundary therefore recognizes only these selected-ID shapes:

- `strategy: "preserve"` plus a selected ID is a resolved explicit filter
  action, matching the compiler's compatibility sentinel.
- `strategy: "best"` plus a selected ID is a planner-resolved `best` action.

A selected ID on `first` or `none` is invalid. The control plane applies the
same per-target reduction as the planner: multiple explicit actions block, one
explicit action discards every strategy action, and multiple strategy actions
without an explicit action block.

Defaults filters are scoped to kept streams of their declared target kind.
Order filters range over kept ordinary tracks; attachments are not track-order
candidates. Bare `order tracks where <filter>` carries an empty group order plus
one resolved head stream, so every remaining ordinary track stays in source
order.

Source order is ascending provider stream index, independent of the snapshot
JSON array or request-vector order. Provider indexes must be unique within a
snapshot; duplicates are insufficient facts and block planning. The control
plane canonicalizes retained references before dispatch. The worker applies
that same ordering rule to both `--track-order` construction and output
inspection, and rejects head references that are duplicated, outside the
retained set, or attachments.

Fact evaluation fails closed when a referenced structured fact is missing or
malformed, including below `not`. The language filter retains its published
special case: an absent language tag is `und`, while a present non-string value
is malformed. Disposition filters require a boolean fact rather than treating
missing or malformed values as false.

## Consequences

- A policy using `defaults audio where …` or `order tracks … where …` compiles,
  and the golden fixture `filter-addressed-tracks.{voom,json}` pins the compiled
  shape. `compiled_json` stays backward compatible: absent fields read as
  `None`/empty, `source_hash` for existing policies is unchanged.
- The typed per-file remux payload carries additive optional resolved snapshot
  stream IDs for filter-addressed defaults and ordering. Old payloads read with
  those fields absent.
- The mkvmerge worker emits `--forced-track-flag id:1|0` and head-pinned
  `--track-order`, covered by worker conformance tests that build a
  `RemuxRequest` directly.
- Adding `head_streams`/`forced_streams`/`clear_forced_streams` to
  `RemuxSelection` forces one-line empty-vector additions at the three
  `RemuxSelection` literals in `voom-control-plane/remux`; adding `filter` /
  `head_filter` to `CompiledOperation` forces `.., ` in a few `voom-plan`
  destructures and `filter: None` / `head_filter: None` in its test literals.
  `operation_kind` uses `{ .. }`, so no exhaustive-match edit is needed. All
  edits are additive and behaviour-preserving.
- `compiled_json` (`policy_versions.compiled_json`) is **Class P (passthrough
  `JsonValue`, no typed DB read)** in `docs/payload-contract-inventory.md`, so
  `CompiledOperation` is outside the Class-T `deny_unknown_fields` regime and no
  scope/inventory edit is needed. The new fields are additive with
  `#[serde(default, skip_serializing_if = "Option::is_none")]`: old compiled
  rows deserialize (absent ⇒ `None`) and unchanged policies serialize
  identically, so their `source_hash` — a hash of the *source text*, not the
  compiled JSON — is unaffected.
- Durable remux events record resolved `head_streams` additively. Existing
  events deserialize with an empty head selection.

**Still deferred:**

- The `forced audio|subtitle where <filter>` DSL op and its compiled `SetForced`
  variant. Forced is delivered only at the wire (`RemuxSelection.forced_streams`
  / `clear_forced_streams`) and worker (`--forced-track-flag`) layer here; the
  DSL surface needs a new `PlanOperationKind` and a separate published
  execution contract.
- Populating `forced_streams` / `clear_forced_streams` from policy remains tied
  to the unpublished forced DSL operation. No forced selection is inferred from
  defaults or ordering.

## Considered & rejected

- **Fold "first" into `defaults … where` (one op sets default *and* first).**
  Rejected: default and first are independent facts; a policy may want a track
  default but not first, or first but not default. Orthogonal ops compose and
  match the existing split between `defaults` and `order tracks`.
- **Make `defaults` strategy/filter mutually exclusive by retyping `strategy`
  to `Option<DefaultStrategy>`.** Rejected: a retype violates the additive-only
  durable-schema contract (ADR 0013) and breaks `voom-plan` destructures that
  read `strategy` as `DefaultStrategy`. An additive `Option<TrackFilter>`
  alongside the unchanged `strategy` is backward compatible.
- **Model forced as a field on `SetDefaults` / `ReorderTracks`.** Rejected:
  forced is a distinct outcome (not a default, not an order) that maps to its
  own plan-operation kind; overloading an existing variant would blur the plan
  vocabulary. A dedicated `SetForced` variant is clearer.
- **Change `track_order` from `Vec<RemuxTrackGroup>` to a group-or-stream
  enum list.** Rejected as premature: a separate `head_streams` list pinned
  ahead of the group order covers "pin a track first" with a purely additive
  change and no churn to the events payload that mirrors `track_order`.
- **Enforce single-match at compile time.** Rejected: the compiler does not see
  the media's streams; only the planner, with snapshot facts, can count matches.
  The single-match rule is therefore a plan-time diagnostic, consistent with how
  the rest of the filter machinery resolves against facts.
