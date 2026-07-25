# ADR 0036: Evaluate stream conditions from authoritative snapshot facts

Status: Proposed

## Context

The published policy grammar includes:

```text
exists audio|subtitle
count audio|subtitle <op> <number>
```

Those forms compile into `CompiledCondition`, but `voom-plan` currently returns
unknown for every `Exists` and `Count`. The same evaluator feeds conditional
operations, phase `skip`, and both rule modes.

The planner already projects `stream_summary.streams` through `stream_facts`.
That projection accepts a real empty array and rejects a missing array,
non-array value, malformed entry, duplicate stream identifier, unknown target,
or invalid provider index. Stream conditions need the same fact boundary as the
track operations they guard.

Durable policy inputs may contain an `existing_media_snapshot_id`. Historical
projection code copied that snapshot's stream array into `stream_summary`, but
normalized a missing array to `[]`. The linked row still proves the input's
original file lineage. Existing coordinator semantics then resolve that
lineage's active chain tip and latest durable snapshot for every execution
phase (ADRs 0005, 0007, and 0008). Read-only plan and report paths need the same
current-fact authority once stream facts select control flow.

## Review charter

- **Base:** `origin/main` at `856bacb8ff42954fede8830007145d0674d937bb`.
- **Outcome:** published audio/subtitle `exists` and `count` select branches
  from trustworthy per-file stream facts.
- **Surfaces:** `voom-policy` validation, `voom-plan` evaluation, and the
  control-plane stored-input planning adapter.
- **Direct dependencies:** snapshot and active-version identity-repository
  reads, `stream_facts`, ADRs 0005/0007/0008, and the published condition
  grammar.
- **Persistence:** read existing rows; no schema, migration, trigger, or stored
  JSON shape change.
- **Review targets:** this ADR, the #329 design spec, and the #329
  implementation plan.

Explicit exclusions:

- Track-filter source aliases and unpublished spellings are owned by #350 under
  #325. #329 changes only `exists`/`count` condition validation.
- Full compiled-policy durable JSON strictness is owned by #344 under #325.
  #329 changes no compiled JSON shape or serde contract.
- Rollback health guidance is owned by #351 under #325. #329 adds no migration
  and does not edit the rollback runbook.
- `run_if completed|modified` execution is owned by #330. #329 leaves canonical
  predicates unknown.
- Filtered and attachment/commentary track execution is owned by #331 and #332.
  #329 does not evaluate track filters.
- Job-level isolation when a lineage advances after planning is owned by #352
  under #325. #329 makes each selected version/snapshot pair coherent but does
  not add a plan-through-dispatch concurrency guarantee.
- Generic policy-input write validation is owned by #353 under #325. #329
  rejects invalid durable links when they enter stored planning.

An excluded concern remains blocking if #329 depends on it or makes it worse.

## Decision

### Fact authority

Every stored entry point runs the raw `Exists`/`Count` shape gate,
deserialization, and complete typed eligibility pass before applying the
authority rules below. An unpublished condition therefore always fails with
the eligibility diagnostic; references here to a policy containing
`Exists`/`Count` mean an eligibility-approved policy containing a published
shape.

1. A store-free planning call uses the `MediaSnapshotInput` supplied by its
   caller.
2. Every durable input whose target is `FileVersion` selects that version's
   file lineage. When `existing_media_snapshot_id` is present, it additionally
   proves original provenance and must name a snapshot for the target version.
3. A linked durable input with any other target kind is invalid. An unlinked
   durable input with another target kind continues using its stored facts only
   when the eligibility-approved compiled policy contains no published
   `Exists` or `Count`. If either variant appears anywhere, every stored plan,
   report, fresh execution, and resume rejects the non-file member before
   planning. Store-free calls remain unchanged.
4. After loading selected `FileVersion` rows but before any active-tip read,
   group members by file-asset id. Two members selecting one file lineage,
   including different historical versions, invalidate the complete stored
   request.
5. Every stored plan, compliance report, fresh execution, and resumed execution
   resolves each selected file lineage's active chain tip and latest durable
   snapshot with one identity-repository read. The operation selects the
   non-retired `file_versions` row with the greatest identifier, then the
   `media_snapshots` row for that exact version with the greatest identifier.
   The returned pair belongs to one SQLite statement snapshot. The control
   plane projects the complete current `MediaSnapshotInput`, retaining the
   input member's ordinal.
6. The first execution phase therefore uses the same current-fact rule as a
   read-only plan or report. After a committed phase, the existing coordinator
   refreshes from the produced version's snapshot before planning the next
   phase, as required by ADRs 0005, 0007, and 0008.
7. Invalid links remain repository-readable but cannot be planned.

The input set is a durable selection of file lineage, not an immutable copy of
observed media facts. This is existing coordinator behavior made consistent
across all stored planning paths.

Authority is coherent per input member, not one wall-clock snapshot across the
complete input set. Conditions and operations are evaluated per file and never
join facts from different input members, so resolving members at different
committed instants cannot combine them into one branch decision. The selected
version may still be superseded after the read and before dispatch; #352 owns
that pre-existing coordinator isolation question. #329 neither widens that
window nor substitutes cached facts inside it.

The selected newest snapshot remains authoritative when its stream inventory is
missing or malformed. Planning does not fall back to an older valid snapshot:
doing so would hide the latest observation and make entry points disagree about
which durable facts are current.

### Stream projection

The shared snapshot projection preserves the source stream value without
changing unrelated remux eligibility:

- missing `payload.streams` produces no `streams` member;
- present `payload.streams` is copied exactly, including JSON null or a
  malformed non-array value;
- an array produces `video_stream_count` from entries whose string `kind` is
  `video`; and
- missing or non-array streams retain the existing
  `video_stream_count: 0` sentinel.

A successfully validated empty array is therefore known empty. Missing,
non-array, malformed, or duplicate stream facts remain unknown.
`Exists`/`Count` use only the validated `streams` projection. The retained
video-count sentinel preserves existing container/remux decisions and is not
evidence that an unavailable inventory is known empty.

### Published source and compiled shapes

Source validation for the newly executable leaves accepts only:

```text
exists audio
exists subtitle
count audio <op> <number>
count subtitle <op> <number>
```

`<op>` is one of `==`, `!=`, `<`, `<=`, `>`, or `>=`. `<number>` must contain
one or more ASCII digits and parse as `u64`. Filtered `exists`, other targets,
extra tokens, and non-numeric comparators are compile errors. This changes no
parser production.

The executable compiled shapes are:

```text
Exists { target: Audio | Subtitle, filter: None }
Count { target: Audio | Subtitle, op: Eq | Ne | Lt | Lte | Gt | Gte, value }
```

Before evaluation, a pure eligibility pass traverses the complete compiled
policy: every phase, `run_if`, `skip_if`, recursively nested conditional and
rule operation, rule condition, and Boolean child. A stream condition outside
the published shapes, or any stream condition in `run_if`, fails plan
generation as an unpublished compiled stream condition. This whole-policy
check prevents a decisive Boolean sibling or an earlier phase from hiding an
unpublished stream form.

`generate_plan` and `plan_phase` both validate the complete policy before
expanding any node. Stored coordinator preparation runs the same validation
before profile resolution, job creation, or dispatch, so an invalid later
phase cannot follow a committed earlier phase.

The validation is intentionally limited to `Exists` and `Count`, whose runtime
meaning changes in this issue. Other pre-existing compiled condition behavior
is unchanged.

Stored compiled JSON receives a bounded raw-shape gate before typed
deserialization can discard unknown fields. The gate recursively visits objects
tagged `type: "exists"` or `type: "count"`:

- `exists` requires `type` and `target`, permits optional `filter`, and rejects
  every other key;
- `count` requires exactly `type`, `target`, `op`, and `value`.

Missing required keys and extra keys fail with the same unpublished-condition
diagnostic and a deterministic JSON path. The subsequent typed eligibility
pass still rejects unpublished targets, non-null `exists` filters, invalid
comparators, and every stream condition in `run_if`. This narrow preflight
exists only because #329 makes these two variants executable. #344 still owns
unknown-field strictness for the rest of compiled policy JSON.

Existing canonical compiled policy versions remain deserializable because no
compiled type or JSON shape changes. Canonical stored versions gain the
published execution semantics. Previously accepted parser-only stream
conditions remain readable but fail planning.

Eligibility rejection uses
`PlanningDiagnosticCode::InvalidPlanningRequest` and the stable message prefix
`unpublished compiled stream condition at`. The message includes a
deterministic structural path containing the phase ordinal and name, the
`run_if`/`skip_if`/operation/rule placement, nested operation indexes, and
Boolean child indexes. It also names the rejected target, whether a filter is
present, and whether the placement is `run_if`.

The eligibility pass collects failures in policy traversal order.
`generate_plan` and `plan_phase` return the same `PlanGenerationError`
diagnostics. Stored preparation converts the first diagnostic through the
existing `PLAN_GENERATION_ERROR` boundary without replacing its message. No new
outer error code or planning-diagnostic enum variant is added.

### Evaluation

Evaluation calls the existing `stream_facts` projection once for each
`Exists` or `Count` leaf:

- `exists` is true when at least one validated fact has the requested target
  and false otherwise;
- `count` counts validated facts with the requested target and applies the six
  numeric comparison operators;
- an unavailable inventory returns unknown.

Existing three-valued Boolean composition remains unchanged. Existing consumer
behavior also remains unchanged:

- conditional `when`: true selects, false omits, unknown blocks;
- phase `skip`: true omits, false plans, unknown blocks;
- `rules first`: select the first true rule, continue past false, and block at
  the first unknown;
- `rules all`: select every true rule, omit false, and block unknown rules.

Canonical `run_if` predicates remain unknown until #330 supplies per-file
phase history.

### Failure contract

All stored read paths use `PLAN_GENERATION_ERROR` with the stable message
prefix `stored policy stream facts are invalid` for:

- a missing linked snapshot;
- a linked member whose target is not `FileVersion`;
- a target/snapshot file-version mismatch;
- a selected `FileVersion` or file lineage with no active version or latest
  snapshot;
- two members selecting the same file asset; and
- a non-`FileVersion` stored media member used with an eligibility-approved
  policy containing a published `Exists` or `Count`.

The message names the input-set identifier, member ordinal, target kind and
file-version identifier when present, optional linked snapshot identifier, and
snapshot file-version identifier when available. Repository failures such as
`DB_UNREACHABLE` propagate unchanged.

A duplicate-lineage message names the input-set identifier, shared file-asset
identifier, both member ordinals, and both selected `FileVersion` identifiers.

## Consequences

- Published existence and count conditions become executable without changing
  the parser or compiled schema.
- Stored plan, report, fresh execution, and resume use the same active
  chain-tip snapshot rule.
- Linked historical inputs recover missing-versus-empty truth from current
  durable snapshots without a data migration.
- Synthetic fixtures and direct planner callers remain self-contained.
- Stored synthetic/non-file inputs retain existing behavior for policies that
  do not use the stream conditions activated here.
- Malformed media facts continue producing the existing insufficient-facts
  planning behavior.
- Existing container/remux eligibility is unchanged for missing and malformed
  stream inventories.
- Parser-only stream condition shapes fail before evaluation instead of being
  activated accidentally.
- The policy input cache may still differ from current facts, but it is no
  longer authoritative for durable `FileVersion` stream-condition planning.

## Considered and rejected alternatives

### Repair and constrain every stored stream summary

Rejected because it would add a migration, triggers, rollback work, and a
second durable truth contract. The durable `FileVersion` target already
identifies the file lineage whose current snapshot supplies the needed fact.

### Treat missing streams as an empty inventory

Rejected because absence does not prove that a file has zero audio or subtitle
streams.

### Count raw JSON entries directly

Rejected because control flow could then accept an inventory that track
planning rejects.

### Evaluate every compiled `Exists` and `Count`

Rejected because compiled representation breadth is not authority to publish
video, attachment, or filtered-existence conditions.

### Keep read-only planning pinned to the original snapshot

Rejected because the phase coordinator already resolves the selected file
lineage's active chain tip and refreshes facts after every commit. Pinning only
read-only paths would make preview and execution select different condition
branches.
