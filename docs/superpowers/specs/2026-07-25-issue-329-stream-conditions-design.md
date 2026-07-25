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
- linked-provenance validation and active-snapshot projection for stored paths;
- source rejection of unpublished `Exists` and `Count` forms;
- bounded raw-JSON validation for stored `Exists` and `Count` shapes;
- fail-closed planning for unpublished compiled stream conditions; and
- readability of existing compiled policy versions.

Deferred with tracked ownership:

- track-filter source strictness: #350;
- full compiled-policy durable JSON strictness: #344;
- rollback runbook corrections: #351;
- per-file `run_if` history: #330; and
- filtered remux execution: #331 and #332; and
- plan-through-dispatch lineage isolation: #352; and
- generic policy-input write validation: #353.

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

Stored policy inputs duplicate stream facts in `stream_summary` and may link
their original snapshot through `existing_media_snapshot_id`. Historical
projection normalized a missing stream array to an empty array. Existing
coordinator semantics use that link to select a file lineage, then plan every
phase from its active chain tip and latest durable snapshot.

## Planning-input authority

The control plane adds one async adapter used by all stored planning paths:

```text
PolicyInputSet
    -> validate original links and resolve current snapshots
    -> PolicyInputSetDraft
    -> voom-plan
```

For each media input:

1. A `FileVersion` target selects its file lineage whether or not
   `existing_media_snapshot_id` is present.
2. When the link is present, load it and require it to name the target's exact
   file version.
3. Resolve the target version's file asset, its active chain tip, and that
   version's latest snapshot with one identity-repository statement. Select the
   non-retired version with the greatest id, then the snapshot for that exact
   version with the greatest id.
4. Replace the complete media input with a projection of the current snapshot,
   preserving its ordinal.
5. Reject a linked non-`FileVersion` target. An unlinked input with another
   target kind retains its stored facts.
6. Fail with `PLAN_GENERATION_ERROR` when provenance or current facts cannot be
   resolved.

The adapter is used by stored plan display, compliance reporting, and
coordinator preparation. Store-free fixture planning continues to trust its
explicit draft. Invalid durable links remain readable but fail this adapter;
#353 separately owns generic write-time validation.

The coordinator already projects active snapshots for each phase under ADRs
0005, 0007, and 0008. Read-only plan/report paths adopt that same authority.
After a phase commits, its produced version's refreshed snapshot becomes the
next phase's authority. Resume resolves the active chain again before opening
its new job.

The repository read is coherent per input member. There is no input-set-wide
transaction because policy conditions and operations never join facts from
different files. A lineage can still advance after its pair is read; #352 owns
that existing coordinator isolation boundary. This change does not add another
read window or use cached facts during the existing one.

The newest snapshot remains authoritative when its stream inventory is missing
or malformed. The adapter never falls back to an older snapshot with valid
facts.

## Projection contract

`stream_summary_from_snapshot_payload` produces:

| Source `payload.streams` | Summary |
|---|---|
| missing | no `streams`; `video_stream_count: 0` |
| array | exact `streams`; derived integer `video_stream_count` |
| null or other non-array | exact `streams`; `video_stream_count: 0` |

Array members are not cleaned or filtered during projection. Validation belongs
to `stream_facts`, so one malformed or duplicate entry makes the complete
inventory unavailable.

The retained video-count sentinel preserves existing container/remux
eligibility. Stream conditions never use it as evidence of a known inventory.
Existing cached rows need no rewrite because linked planning no longer treats
them as current facts.

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

Before evaluation, the planner checks every stream condition in the complete
compiled policy, including:

- every phase, even when `plan_phase` names a different phase;
- phase `skip`;
- recursively nested conditional operations and `rules first|all`; and
- phase `run_if`.

Ordinary condition surfaces accept only unfiltered audio/subtitle `Exists` and
audio/subtitle `Count` with numeric comparators. `run_if` accepts no stream
condition. If a Boolean tree contains one invalid stream leaf, plan generation
fails before Boolean evaluation.

This check is limited to `Exists` and `Count`. It prevents #329 from activating
parser-only stream forms without claiming the broader compiled-policy contract
owned by #344.

Before stored compiled JSON is deserialized into `CompiledPolicy`, a bounded
raw-value gate recursively checks objects tagged `type: "exists"` or
`type: "count"`. `exists` requires `type` and `target`, allows optional
`filter`, and allows no other keys. `count` requires exactly `type`, `target`,
`op`, and `value`. Missing required or extra keys fail with a deterministic
JSON path before serde can discard them. Typed eligibility then enforces the
published values and placements.

The raw gate is not a complete parallel compiled schema and does not rewrite
JSON. It protects only the two variants made executable by #329. #344 remains
responsible for unknown-field strictness across every other compiled type.

`generate_plan` and `plan_phase` perform the complete validation before node
expansion. Stored coordinator preparation performs it before profile
resolution, job creation, or dispatch. An invalid later phase therefore cannot
follow an earlier committed phase.

Canonical historical compiled versions deserialize and execute. Historical
parser-only stream forms deserialize but fail plan generation with a message
that identifies an unpublished compiled stream condition.

Eligibility failures use the existing
`PlanningDiagnosticCode::InvalidPlanningRequest`. Their messages begin with
`unpublished compiled stream condition at` and include a deterministic path:
phase ordinal/name, guard or operation/rule placement, nested operation indexes,
and Boolean child indexes. The message also includes the target, filter
presence, and whether the condition appeared in `run_if`.

The pass collects every rejection in policy traversal order. Both planner entry
points return the same `PlanGenerationError` diagnostics. Stored preparation
preserves the first diagnostic's message when converting it to the public
`PLAN_GENERATION_ERROR`; no new public error or diagnostic code is introduced.

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
- Missing or invalid linked provenance and unavailable current snapshots fail
  every stored read path with `PLAN_GENERATION_ERROR` and the message prefix
  `stored policy stream facts are invalid`.
- Read-side messages include input-set id, member ordinal, target kind and
  target file-version id when present, snapshot id, and snapshot file-version
  id when available.
- Repository errors such as `DB_UNREACHABLE` propagate unchanged.
- Unpublished compiled stream conditions fail with
  `PLAN_GENERATION_ERROR`, `invalid_planning_request`, the stable message
  prefix, and structural context before any node is emitted.
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
- stored `exists`/`count` objects with missing or extra keys fail before typed
  deserialization can erase their shape;
- stored plan, report, fresh execution, and resume resolve the same active
  chain-tip snapshot before their first phase;
- each resolved active-version/latest-snapshot pair comes from one repository
  statement and the snapshot belongs to the returned version;
- a malformed newest snapshot remains authoritative rather than falling back
  to an older valid snapshot;
- post-commit phases use the produced version's refreshed snapshot;
- link target mismatches fail planning;
- unlinked durable `FileVersion` members resolve their active lineage in every
  stored entry point;
- unlinked durable non-file and store-free inputs retain their supplied facts;
- both snapshot projection callers preserve missing and malformed stream
  values without changing container/remux eligibility;
- an invalid stream leaf in a later phase fails before any earlier phase opens
  a job or dispatches;
- direct and stored planner entry points report the same deterministic
  eligibility diagnostics and structural paths;
- provenance failures have identical codes and context across stored plan,
  report, fresh execution, and resume; and
- previously serialized canonical compiled policies remain readable.

The existing #326 coverage matrix already assigns C07-C11 and S10/S14-S17 to
#329. Focused owner tests satisfy its second acceptance layer without changing
the canonical policy sources.
