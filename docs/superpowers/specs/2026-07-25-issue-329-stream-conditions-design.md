# Issue 329 design: Evaluate published stream conditions

## Objective

Make published audio/subtitle `exists` and `count` conditions select
control-flow branches from trustworthy per-file stream facts. Preserve unknown
only when the inventory cannot be validated.

## Scope and ownership

In scope:

- audio/subtitle unfiltered `exists`;
- audio/subtitle `count` with all six numeric comparators;
- `when`, phase `skip`, `rules first`, and `rules all`;
- missing, empty, malformed, duplicate, and known stream inventories;
- exact linked-snapshot rehydration for stored planning paths;
- source rejection of unpublished `Exists` and `Count` forms;
- fail-closed planning for unpublished compiled stream conditions; and
- readability of existing compiled policy versions.

Deferred with tracked ownership:

- track-filter source strictness: #350;
- full compiled-policy durable JSON strictness: #344;
- rollback runbook corrections: #351;
- per-file `run_if` history: #330; and
- filtered remux execution: #331 and #332.

#329 does not change track-filter validation, compiled JSON shapes, serde
strictness, database schema, migrations, rollback procedures, or parser
productions. A deferred concern returns to scope if this design depends on it
or worsens it.

## Current behavior

`voom-policy` lowers `exists` and `count` into `CompiledCondition`.
`voom-plan::evaluate_condition` maps both variants directly to unknown.

Track planning already uses `stream_facts`, which validates the entire stream
array before returning typed facts. An actual empty array succeeds. Missing or
non-array streams, malformed entries, duplicate identifiers, unknown targets,
and invalid provider indexes fail the projection.

Stored policy inputs duplicate stream facts in `stream_summary` and may link the
source snapshot through `existing_media_snapshot_id`. Historical projection
normalized a missing stream array to an empty array, although the linked
snapshot payload retains the distinction.

## Planning-input authority

The control plane adds one async adapter used by all stored planning paths:

```text
PolicyInputSet
    -> validate and rehydrate linked stream summaries
    -> PolicyInputSetDraft
    -> voom-plan
```

For each media input:

1. If `existing_media_snapshot_id` is absent, retain the stored summary.
2. If present, load that exact snapshot identifier.
3. Require the media input target to be the snapshot's exact file version.
4. Replace only `stream_summary` with a fresh projection of the source payload.
5. Fail with `PLAN_GENERATION_ERROR` when the link is missing or mismatched.

The adapter is used by stored plan display, compliance reporting, and
coordinator preparation. Store-free fixture planning continues to trust its
explicit draft. New control-plane writes reject invalid linked provenance
before persistence.

The coordinator already projects exact durable snapshots for each active phase.
Using the same stream-summary projection in both paths keeps single-shot and
phase planning aligned.

## Projection contract

`stream_summary_from_snapshot_payload` produces:

| Source `payload.streams` | Summary |
|---|---|
| missing | no `streams`; no `video_stream_count` |
| array | exact `streams`; derived integer `video_stream_count` |
| null or other non-array | exact `streams`; no `video_stream_count` |

Array members are not cleaned or filtered during projection. Validation belongs
to `stream_facts`, so one malformed or duplicate entry makes the complete
inventory unavailable.

This projection fixes future cached rows as well as planning rehydration.
Existing cached rows need no rewrite because linked planning no longer treats
them as authoritative.

## Source boundary

The existing condition parser remains unchanged. Validation narrows only the
leaves whose runtime behavior changes:

- `exists` requires exactly `audio` or `subtitle` and accepts no filter;
- `count` requires exactly `audio` or `subtitle`;
- `count` accepts only the six numeric comparison spellings; and
- the count value must contain one or more ASCII digits and parse as `u64`.

All other source-condition validation is unchanged. #350 separately owns
track-filter aliases and unpublished filter spellings.

## Stored compiled compatibility

No compiled type, discriminator, key, schema version, or serde annotation
changes.

Before evaluation, the planner checks every stream condition in:

- phase `skip`;
- conditional operations;
- `rules first|all`; and
- phase `run_if`.

Ordinary condition surfaces accept only unfiltered audio/subtitle `Exists` and
audio/subtitle `Count` with numeric comparators. `run_if` accepts no stream
condition. If a Boolean tree contains one invalid stream leaf, plan generation
fails before Boolean evaluation.

This check is limited to `Exists` and `Count`. It prevents #329 from activating
parser-only stream forms without claiming the broader compiled-policy contract
owned by #344.

Canonical historical compiled versions deserialize and execute. Historical
parser-only stream forms deserialize but fail plan generation with a message
that identifies an unpublished compiled stream condition.

## Evaluation semantics

For an eligible leaf, call `stream_facts(snapshot)`.

`Exists`:

```text
matched     = at least one fact has the requested target
not matched = no fact has the requested target
unknown     = stream_facts failed
```

`Count`:

```text
actual = number of facts with the requested target
result = compare(actual, operator, expected)
unknown = stream_facts failed
```

A known empty inventory produces false for `exists` and zero for `count`.
All six comparator boundaries use the existing numeric comparison helper.

`Not`, `And`, and `Or` retain their current three-valued truth tables. The
planner validates stream-condition eligibility before applying those tables.

Consumer behavior remains:

| Surface | True | False | Unknown |
|---|---|---|---|
| `when` | include nested operations | omit | block nested operations |
| `skip` | omit phase operations | plan | block phase operations |
| `rules first` | select and stop | continue | block rule and stop |
| `rules all` | select | omit | block that rule |

`run_if completed|modified` remains unknown under #329 and becomes executable
per file in #330.

## Failure behavior

- Missing or malformed facts create the existing
  `insufficient_snapshot_facts` blocked nodes.
- Missing or mismatched linked snapshots fail the stored planning call with
  `PLAN_GENERATION_ERROR`.
- Unpublished compiled stream conditions fail plan generation before any node
  is emitted.
- Source forms outside the newly executable leaves fail compilation.

No failure path mutates durable state.

## Test strategy

Focused tests prove:

- source acceptance for the four target forms and six comparators;
- source rejection for filtered exists, other targets, invalid comparators,
  extra tokens, and overflowing counts;
- true, false, zero, and comparator boundaries;
- missing, null, non-array, malformed, duplicate, empty, and populated facts;
- Boolean composition over known and unknown stream leaves;
- `when`, `skip`, `rules first`, and `rules all`;
- stream conditions remain unavailable in `run_if`;
- unpublished compiled stream leaves fail before Boolean short-circuiting;
- linked input rehydration uses the exact snapshot rather than the cached
  summary or latest snapshot;
- link target mismatches fail planning;
- unlinked and store-free inputs retain their supplied summary;
- both snapshot projection callers preserve missing and malformed values; and
- previously serialized canonical compiled policies remain readable.

The existing #326 coverage matrix already assigns C07-C11 and S10/S14-S17 to
#329. Focused owner tests satisfy its second acceptance layer without changing
the canonical policy sources.
