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
- durable per-job file starts for safe reconciliation of historical inputs;
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
strictness, rollback procedures, or parser productions. Its only schema change
is ADR 0037's per-job file-start table and migration. A deferred concern returns
to scope if this design depends on it or worsens it.

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
coordinator semantics use every `FileVersion` target to select a file lineage,
then plan each phase from its active chain tip and latest durable snapshot.

## Planning-input authority

The control plane adds one async adapter used by all stored planning paths:

```text
CompiledPolicy + PolicyInputSet
    -> validate original links and resolve current snapshots
    -> StoredPlanningInput {
           draft: PolicyInputSetDraft,
           files: [ResolvedFileInput]
       }
    -> voom-plan
```

Each `ResolvedFileInput` carries the member ordinal, selected input-set
`FileVersion` id, file-asset id, complete resolved active `FileVersion`, and
exact latest `MediaSnapshot` returned by the authority read.

The adapter receives the already raw-gated, deserialized, and typed-eligible
policy. It performs two passes.

The first pass classifies and validates every media input:

1. A `FileVersion` target selects its file lineage whether or not
   `existing_media_snapshot_id` is present. Load the selected version to obtain
   its file-asset id.
2. When the link is present, load it and require it to name the target's exact
   file version.
3. Reject a linked non-`FileVersion` target.
4. An unlinked input with another target kind retains its stored facts only
   when the eligibility-approved policy contains no published `Exists` or
   `Count`. If either condition appears anywhere, every stored entry point
   rejects the non-file member before planning; store-free callers remain
   unchanged.

Before resolving any active tip, group selected versions by file-asset id and
reject a duplicate group, including different historical versions of one
lineage. The failure names the input-set id, shared file-asset id, and both
member ordinals and selected-version ids.

The second pass resolves each unique file lineage:

1. Read its active chain tip and that version's latest snapshot with one
   identity-repository statement. Select the non-retired version with the
   greatest id, then the snapshot for that exact version with the greatest id.
2. Replace the complete media input with a projection of the current snapshot,
   preserving its ordinal.

Unresolvable provenance or current facts fail with `PLAN_GENERATION_ERROR`.
The adapter emits no partial `StoredPlanningInput`.

Stored plan display and compliance reporting pass `draft` to `voom-plan`.
Coordinator preparation also retains `files`: it derives branch ids from the
resolved active versions and constructs first-phase `PhaseFile` state directly
from those records. Fresh and resume do not repeat the initial active-tip or
snapshot read. Store-free fixture planning continues to trust its explicit
draft. Invalid durable links remain readable but fail this adapter; #353
separately owns generic write-time validation.

## Single execute preparation

Fresh compliance execution must not call the public stored-report path and
coordinator preparation independently. One internal preparation flow:

1. loads the accepted policy version and durable input set once;
2. runs raw eligibility, deserializes, and runs typed eligibility;
3. calls the stored-input adapter exactly once;
4. resolves profiles and performs tool preflight;
5. generates the initial plan/report from `StoredPlanningInput.draft`;
6. applies safety and live-worker checks to that plan; and
7. derives branch ids and first-phase files from the same
   `StoredPlanningInput.files`.

It returns the report data and `PhaseBarrierRunInputs` together. Issue
application consumes that report before the prepared coordinator opens its
job. The test-only runtime-registry execute path uses the same preparation
function. Public plan/report calls and direct fresh/resume coordinator calls
each perform one independent adapter call for their own invocation.

The coordinator already projects active snapshots for each phase under ADRs
0005, 0007, and 0008. Read-only plan/report paths adopt that same authority.
After a phase commits, its produced version's refreshed snapshot becomes the
next phase's authority.

## Resume run-start authority

The input-set selector cannot serve as resume's starting version because it may
be historical. Each phase-barrier job therefore records ADR 0037's immutable
per-file run start: branch id, authoritative active version at job start, and
the first phase ordinal that file may enter.

Fresh files map into `PhaseFile` as follows:

```text
version_id = resolved active FileVersion id
snapshot = resolved latest snapshot
resume_ordinal = 0
```

Resume resolves the current authority once, then reads the prior job's
run-start and phase rows before opening a new job. For each branch:

- the prior starting version and every committed-row version must belong to
  the current selected file lineage;
- highest phase row plus one determines the next ordinal;
- with no row, the prior run's starting ordinal determines it;
- highest committed row determines the recorded tip;
- with no committed row, the prior run's starting version determines it;
- a blocked row or next ordinal at `phase_count` is terminal and requires the
  current tip to equal the recorded tip; and
- only a non-terminal current tip different from the recorded tip backfills
  exactly one phase and advances the ordinal.

Missing, branch-mismatched, lineage-mismatched, or terminal-tip-mismatched
prior state fails with
`POLICY_EXECUTION_ERROR` and prefix `resume state is incomplete` before the new
job opens. Pre-migration jobs without run-start rows fail rather than guessing.

The new job records the post-reconciliation current version and next ordinal
for every branch, including terminal branches at `phase_count`. Job creation,
the `job.opened` event, all new run-start rows, and any backfilled file-phase
rows commit in one transaction. The coordinator then constructs `PhaseFile`
values from the already resolved authority records:

```text
version_id = resolved current FileVersion id
snapshot = resolved latest snapshot
resume_ordinal = reconciled next ordinal
```

`PhaseFile.start_version_id` is removed. A chained resume therefore remains
correct even when a resumed job crashes before writing its first ordinary
phase row.

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
Existing cached rows need no rewrite because durable `FileVersion` planning no
longer treats them as current facts.

## Source boundary

The existing condition parser remains unchanged. Validation narrows only the
leaves whose runtime behavior changes:

- `exists` requires exactly `audio` or `subtitle` and accepts no filter;
- `count` requires exactly `audio` or `subtitle`;
- `count` accepts only the six numeric comparison spellings; and
- the count value must contain one or more ASCII digits and parse as `u64`.

All other source-condition validation is unchanged. #350 separately owns
track-filter aliases and unpublished filter spellings.

`production-normalize-reduced.voom`, which contains filtered `exists`, moves
from the valid compilation corpus to the invalid source corpus with a
diagnostic golden. Its existing compiled JSON remains unchanged as a dedicated
stored-compatibility fixture: deserialization succeeds, then typed eligibility
rejects the filtered leaf before planning. The fixture transition narrows
published source without discarding evidence that historical compiled versions
remain readable.

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
raw-value gate walks only schema-defined condition slots:

- each `phases[i].run_if` and `phases[i].skip_if`;
- `condition` on a conditional operation;
- each `rules[j].condition` on a rules operation;
- nested operations inside conditional operations and rules; and
- `inner` for `not` plus `conditions[k]` for `and` and `or`.

The operation walk follows phase, operation, and rule array order. Within a
phase it visits `run_if`, `skip_if`, then operations. It never descends into
metadata, provenance flags, resolved profiles, compiled values, or other
non-condition fields.

At those slots, objects tagged `type: "exists"` or `type: "count"` receive the
bounded shape check. `exists` requires `type` and `target`, allows optional
`filter`, and allows no other keys. `count` requires exactly `type`, `target`,
`op`, and `value`. Missing required or extra keys fail before serde can discard
them. Raw paths use deterministic JSON-pointer form, such as
`/phases/0/operations/1/rules/0/condition/conditions/1`. Typed eligibility uses
the semantic phase/placement path described below.

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

Typed paths use a form such as
`phase[0:"normalize"].operations[1].rules[0].condition.and[1]`. The pass uses
the same phase, guard, operation, rule, and Boolean traversal order as the raw
walk and collects every rejection in that order. Both planner entry points
return the same `PlanGenerationError` diagnostics. Stored preparation preserves
the first diagnostic's message when converting it to the public
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
- Duplicate selected file lineages and non-file members used with any
  `Exists`/`Count` condition use that same error code and prefix.
- Read-side messages include input-set id, member ordinal, target kind and
  target file-version id when present, snapshot id, and snapshot file-version
  id when available.
- Duplicate-lineage messages also include both ordinals and selected-version
  ids and the shared file-asset id.
- Repository errors such as `DB_UNREACHABLE` propagate unchanged.
- Unpublished compiled stream conditions fail with
  `PLAN_GENERATION_ERROR`, `invalid_planning_request`, the stable message
  prefix, and structural context before any node is emitted.
- Source forms outside the newly executable leaves fail compilation.

Every #329 raw-shape, typed-eligibility, provenance, duplicate-lineage,
non-file-member, and unavailable-authority failure occurs before issue
application, job creation, or dispatch and performs no durable mutation.
Runtime failures after issue application, dispatch, or commit retain the
existing honest partial-state contracts from ADRs 0007 and 0008.

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
- coordinator preparation retains the resolved authority records and performs
  no second initial-tip or snapshot read;
- fresh execute generates its initial report, safety decisions, and
  first-phase state from one adapter result;
- fresh job creation atomically records every branch's resolved starting
  version and phase zero;
- resume with a historical selected version and no prior phase rows does not
  mistake a version active before the prior job for a lost phase commit;
- resume backfills a real commit whose row was lost, then records the advanced
  phase cursor atomically with the new job;
- chained resume preserves a nonzero starting ordinal when the intermediate
  resumed job wrote no ordinary phase row;
- pre-migration, missing, duplicate, or branch-mismatched run-start state fails
  before a new job opens;
- run-start and committed-row versions from another file lineage fail before a
  new job opens;
- blocked and complete cursors never backfill an out-of-range phase, and a
  changed terminal tip fails closed;
- first-phase construction uses an injected authority result even when a later
  repository read would return another tip; #352 owns future pre-dispatch
  revalidation;
- each resolved active-version/latest-snapshot pair comes from one repository
  statement and the snapshot belongs to the returned version;
- a malformed newest snapshot remains authoritative rather than falling back
  to an older valid snapshot;
- post-commit phases use the produced version's refreshed snapshot;
- link target mismatches fail planning;
- unlinked durable `FileVersion` members resolve their active lineage in every
  stored entry point;
- duplicate members that select one file asset fail identically across
  plan, report, fresh execution, and resume;
- a stored policy with `Exists`/`Count` rejects non-file media members across
  every stored entry point, while other stored policies and store-free inputs
  retain their existing behavior;
- a request combining an unpublished stream condition with a non-file stored
  member fails eligibility first with no durable mutation;
- both snapshot projection callers preserve missing and malformed stream
  values without changing container/remux eligibility;
- an invalid stream leaf in a later phase fails before any earlier phase opens
  a job or dispatches;
- direct and stored planner entry points report the same deterministic
  typed-eligibility diagnostics and semantic paths;
- raw-shape failures report deterministic JSON-pointer paths while unrelated
  metadata and provenance objects tagged `exists` or `count` remain readable;
- filtered `exists` is a negative source fixture while its unchanged compiled
  JSON remains readable and fails typed eligibility;
- provenance failures have identical codes and context across stored plan,
  report, fresh execution, and resume; and
- every #329 preflight rejection leaves issue, job, ticket, file-version, and
  workflow-summary rows unchanged; and
- previously serialized canonical compiled policies remain readable.

The existing #326 coverage matrix already assigns C07-C11 and S10/S14-S17 to
#329. Focused owner tests satisfy its second acceptance layer without changing
the canonical policy sources.
