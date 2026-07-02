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

This feature spans three crate-ownership boundaries under the parallel Sprint
12–17 workstream: the DSL edges (`voom-policy`), the wire/worker edges
(`voom-worker-protocol`, `voom-mkvtoolnix-worker`), and the middle
(`voom-plan` planner resolution + `voom-control-plane` selection population,
owned by the #272/audio workstream). This ADR fixes the contract at the edges
so the middle can be wired independently.

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
  `keep`/`remove` filters that match nothing), not an error.

The single-match enforcement, the plan-time diagnostic, and filter resolution to
a concrete stream are **planner responsibilities not implemented in this PR**
(see section 5 and the "Not in this PR" list under Consequences). This ADR fixes
the DSL/wire/worker contract; the compiler parses, validates the *shape* of, and
lowers these forms, but never counts matches — it cannot, because it does not see
the media's streams. Precedence when a strategy default and a filter-addressed
default target the same kind group is likewise a planner rule, deferred with the
resolution work; this PR does not define it.

### 2. Compiled schema (`voom-policy`, additive-only per ADR 0013)

- `SetDefaults` gains `filter: Option<TrackFilter>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`). `None`
  preserves the existing strategy-only meaning. When `Some`, the operation is
  filter-addressed and `strategy` is not consulted; the `where` form lowers
  `strategy` to `Preserve` so that, until the planner honours `filter`, the
  operation is inert rather than silently applying a group-wide default —
  `set_defaults_changes`'s `Preserve` arm returns `false`
  (`crates/voom-plan/src/planner/remux/mod.rs:510`), a verified no-op.
- `ReorderTracks` gains `head_filter: Option<TrackFilter>` (same serde
  attributes). `targets` keeps its meaning; `head_filter` pins one track first.
- New fieldful variant `SetForced { target: TrackTarget, filter: TrackFilter }`.
  A new variant (not a field on an existing one) because forced is a distinct
  operation with its own plan-operation kind, mirroring how the other track
  operations are modelled.

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

### 5. Boundary / deferral

Filter *resolution* (compiled filter → the concrete `RemuxStreamRef`, including
the "exactly one match or diagnostic" enforcement for the default and order
filters) is a planner responsibility in `voom-plan`, and populating
`head_streams` / `forced_streams` / `clear_forced_streams` into `RemuxSelection`
is a `voom-control-plane/remux` responsibility. Both are owned by the #272 /
audio workstream. This PR lands the edges — DSL validate + lower + fixtures,
wire + compiled fields, and worker emission with conformance tests — plus the
mechanical, behaviour-preserving edits needed to keep the workspace compiling.
The planner resolution and control-plane population are an explicitly-tracked
follow-up; until they land, the new fields default empty and the feature is
inert, never wrong.

## Consequences

- A policy using `defaults audio where …`, `order tracks where …`, or
  `forced subtitle where …` compiles, and golden fixtures pin the compiled
  shape. `compiled_json` stays backward compatible: absent fields read as
  `None`/empty, `source_hash` for existing policies is unchanged.
- The mkvmerge worker emits `--forced-track-flag` and head-pinned
  `--track-order`, covered by worker conformance tests that build a
  `RemuxRequest` directly.
- Adding `head_streams`/`forced_streams`/`clear_forced_streams` to
  `RemuxSelection` and `filter`/`head_filter`/`SetForced` to `CompiledOperation`
  forces mechanical edits at construction/destructure sites and the exhaustive
  `operation_kind` match in `voom-plan`, and at the `RemuxSelection` literals in
  `voom-control-plane/remux`. Those edits are additive and behaviour-preserving.
- `compiled_json` (`policy_versions.compiled_json`) is **Class P (passthrough
  `JsonValue`, no typed DB read)** in `docs/payload-contract-inventory.md`, so
  `CompiledOperation` is outside the Class-T `deny_unknown_fields` regime and no
  scope/inventory edit is needed. The new fields are additive with
  `#[serde(default, skip_serializing_if = "Option::is_none")]`: old compiled
  rows deserialize (absent ⇒ `None`) and unchanged policies serialize
  identically, so their `source_hash` — a hash of the *source text*, not the
  compiled JSON — is unaffected.
- Until the planner/control-plane follow-up lands, the three new intents are
  parsed and stored but do not yet change a produced artifact.

**Not in this PR (tracked follow-up, #272 / audio + control-plane workstream):**

- Planner filter resolution (compiled filter → concrete `RemuxStreamRef`).
- The single-match enforcement and its plan-time diagnostic for
  `defaults … where` and `order tracks … where`. The compiler validates only the
  shape; the "exactly one match or fail" acceptance criterion of #277 is met by
  the planner work, not here.
- Control-plane population of `head_streams` / `forced_streams` /
  `clear_forced_streams` into `RemuxSelection`.
- Extending the remux **event** payload (`voom-events`
  `ArtifactRemux…`) with the forced/head decisions. Today `default_streams` /
  `clear_default_streams` / `track_order` are recorded but forced and head-order
  decisions would not be, an observability gap to close when the population work
  lands. `voom-events` is outside this PR's scope.

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
