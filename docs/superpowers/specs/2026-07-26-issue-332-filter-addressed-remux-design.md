# Issue #332: Filter-addressed remux defaults and ordering design

## Goal

Make the two published filter-addressed remux forms executable and
authoritative:

```text
defaults audio|subtitle where <track-filter>
order tracks [<target-list>] where <track-filter>
```

For each file, the filter must select exactly one stream that survived ordered
keep/remove actions. Zero or multiple matches block that file before execution
with an actionable diagnostic. Planning, execution, output inspection, and
replanning must agree on the selected stream.

`defaults audio|subtitle best` remains issue #336. The payload shape introduced
here must let a later best-selection action yield to an explicit
filter-addressed selection.

## Existing contracts

ADR 0023 already fixes the source grammar and the worker contract:

- a defaults filter selects one default in its target group and clears the
  group's other defaults;
- an order filter pins one ordinary track before optional group ordering;
- bare `order tracks where <filter>` keeps remaining ordinary tracks in source
  order;
- `RemuxSelection.head_streams` and per-track default vectors are the worker
  inputs;
- MKVToolNix emits the requested flags/order and validates produced defaults.
  Its output-order validation does not yet account for a head selection; that
  worker boundary is completed by this issue.

The compiler already stores `SetDefaults.filter` and
`ReorderTracks.head_filter` additively. Existing compiled policy versions with
absent fields remain readable and retain their meaning.

## Decision

### Resolve once during per-file planning

`voom-plan` resolves filter-addressed operations against structured
`SnapshotStreamFact` values after applying the ordered keep/remove reducer.

- A defaults filter considers only kept streams of its declared target kind.
- An order filter considers every kept ordinary stream. Attachments cannot be
  placed in MKV track order and therefore are not candidates.
- Structured fact parsing preserves the distinction between absent and
  malformed values. Missing or malformed facts required by an evaluated filter
  block planning, including beneath `not`; the published exception remains an
  absent language tag, which evaluates as `und`. A non-string language and a
  missing or non-boolean `default`, `forced`, or `commentary` disposition do not
  silently evaluate as false.
- Source order is the stable `provider_stream_index` order.

The resolver returns the final keep set, resolved default actions, resolved
head stream, requested group order, and whether the final observable state
differs from the snapshot. Payload construction and compliance status consume
that same result; they do not evaluate the filters independently.

### Carry snapshot identity, not filters, across execution

The typed remux operation payload gains two additive optional fields:

- `defaults[].selected_snapshot_stream_id`
- top-level `head_snapshot_stream_id`

For a filter-addressed default,
`selected_snapshot_stream_id` is present and `strategy` remains the historical
`preserve` sentinel for compiled-schema compatibility. Execution treats the
selected ID as authoritative and does not consult the strategy.

For strategy defaults, the selected ID is absent and existing behavior is
unchanged. This also gives issue #336 an explicit precedence rule: if any
default action for a target has a selected snapshot ID, strategy-based `best`
selection for that target cannot override it.

The head field is absent when no order filter exists. It may be present while
`track_order` is empty, representing bare `order tracks where ...`.

The control plane resolves each carried ID against the pinned snapshot and
requires it to be kept and of the expected kind where applicable. It never
reevaluates the source filter. It sorts retained stream references by ascending
`provider_stream_index` before constructing the execution selection, regardless
of the JSON array order in the snapshot.

### Cardinality diagnostics

Two additive planning diagnostic codes distinguish the failure modes:

- `empty_track_filter_selection`
- `ambiguous_track_filter_selection`

Messages name the operation (`defaults` or `order tracks`), the target kind
for defaults, and the retained match count. Both failures block only the
affected file before a ticket is executable.

### Default and ordering semantics

A filter-addressed default produces exactly one `default_streams` entry for
its target and places every other kept target stream in
`clear_default_streams`.

Desired ordinary-track order is calculated as:

1. the resolved head stream, when present;
2. kept streams in each requested target group, preserving source order;
3. all remaining kept ordinary streams, preserving source order.

Attachments remain outside MKV track ordering and retain their existing
selection order.

Planning compares this full desired snapshot-stream ID sequence with the
current kept ordinary sequence. It therefore recognizes both required changes
and compliant `NoOp` results, including head-only ordering.

The MKVToolNix worker treats ascending provider stream index as source order
rather than trusting request-vector order. Its argument construction and output
inspection use the same head, requested groups, then remaining-streams
algorithm. Before invoking the provider, the worker rejects duplicate head
references, heads absent from `keep_streams`, and attachment heads. This keeps a
malformed request from being silently normalized by head lookup.

### Durable event visibility

The durable remux event payloads gain additive, default-empty `head_streams`
fields. Started, progress, succeeded, and failed events record the same
resolved head selection sent to the worker. Existing stored events remain
readable because absent fields default to an empty vector.

Forced selections remain empty and outside this issue because no forced DSL
operation is published.

## Validation

Focused tests cover:

- defaults and order filters with zero, one, and multiple retained matches;
- filters that match a source stream removed by an earlier action;
- malformed or missing language/default/forced/commentary facts required by a
  selected filter, including negated filters;
- head-only and head-plus-group desired order;
- shuffled snapshot arrays and request vectors proving that provider stream
  index, not serialization order, defines source order;
- exact default set/clear vectors;
- payload parsing of old forms and rejection of malformed resolved IDs;
- control-plane rejection of IDs missing from or inconsistent with the pinned
  snapshot;
- worker rejection of malformed head selections and head-aware output-order
  inspection;
- durable event backward compatibility and head visibility.

The generated-media remux flow uses the published `where` forms, inspects
default flags and exact track order in the produced MKV, and proves that
replanning the produced snapshot is compliant/`NoOp`.

## Out of scope

- language-ranked `defaults ... best` (#336);
- forced DSL and forced selection population;
- audio execution and lineage issues;
- parser aliases or unpublished DSL forms;
- arbitrary per-track ordering beyond the single published head selector.
