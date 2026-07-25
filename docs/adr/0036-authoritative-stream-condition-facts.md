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
normalized a missing array to `[]`. The exact linked `media_snapshots` row still
contains the authoritative value. Repairing every cached summary would add a
migration and a second durable identity contract when planning can instead read
the declared source.

## Review charter

- **Base:** `origin/main` at `856bacb8ff42954fede8830007145d0674d937bb`.
- **Outcome:** published audio/subtitle `exists` and `count` select branches
  from trustworthy per-file stream facts.
- **Surfaces:** `voom-policy` validation, `voom-plan` evaluation, and the
  control-plane stored-input planning adapter.
- **Direct dependencies:** existing `IdentityRepo::get_media_snapshot`,
  `stream_facts`, and the published condition grammar.
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

An excluded concern remains blocking if #329 depends on it or makes it worse.

## Decision

### Fact authority

1. A store-free planning call uses the `MediaSnapshotInput.stream_summary`
   supplied by its caller.
2. An unlinked durable policy input also uses its stored `stream_summary`.
3. Before any stored policy input reaches the planner, the control plane
   rehydrates each linked input from the exact `media_snapshots` row named by
   `existing_media_snapshot_id`.
4. A linked input is valid only when its target is `FileVersion` and that
   target equals the linked snapshot's `file_version_id`. A missing snapshot,
   other target kind, or mismatch fails planning before plan generation.
5. Rehydration replaces only `stream_summary`. Other accepted input facts keep
   their stored values.
6. New control-plane writes validate the same link relationship. Historical
   mismatches remain readable from the repository but cannot be planned.

The exact snapshot identifier is authoritative, not the latest snapshot for a
file version. This keeps accepted input sets deterministic.

### Stream projection

The shared snapshot projection preserves the source payload shape:

- missing `payload.streams` produces a summary with no `streams` member;
- present `payload.streams` is copied exactly, including JSON null or a
  malformed non-array value;
- an array produces `video_stream_count` from entries whose string `kind` is
  `video`;
- missing or non-array streams do not synthesize `video_stream_count`.

A successfully validated empty array is therefore known empty. Missing,
non-array, malformed, or duplicate stream facts remain unknown.

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

Before evaluation, the planner traverses phase `skip`, conditional operations,
and rule conditions. A stream condition outside those shapes, or any stream
condition in `run_if`, fails plan generation as an unpublished compiled stream
condition. This whole-tree check prevents a decisive Boolean sibling from
activating an unpublished stream form.

The validation is intentionally limited to `Exists` and `Count`, whose runtime
meaning changes in this issue. Other pre-existing compiled condition behavior
is unchanged.

Existing compiled policy versions remain deserializable because no compiled
type or JSON shape changes. Canonical stored versions gain the published
execution semantics. Previously accepted parser-only stream conditions remain
readable but fail planning.

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

## Consequences

- Published existence and count conditions become executable without changing
  the parser or compiled schema.
- Linked historical inputs recover missing-versus-empty truth from their exact
  source snapshot without a data migration.
- Synthetic fixtures and direct planner callers remain self-contained.
- Malformed media facts continue producing the existing insufficient-facts
  planning behavior.
- Parser-only stream condition shapes fail before evaluation instead of being
  activated accidentally.
- The policy input cache may still differ from its linked source, but it is no
  longer authoritative for linked stream-condition planning.

## Considered and rejected alternatives

### Repair and constrain every stored stream summary

Rejected because it would add a migration, triggers, rollback work, and a
second durable truth contract. The exact linked snapshot already contains the
needed fact.

### Treat missing streams as an empty inventory

Rejected because absence does not prove that a file has zero audio or subtitle
streams.

### Count raw JSON entries directly

Rejected because control flow could then accept an inventory that track
planning rejects.

### Evaluate every compiled `Exists` and `Count`

Rejected because compiled representation breadth is not authority to publish
video, attachment, or filtered-existence conditions.

### Re-read the latest snapshot for the file version

Rejected because an accepted input links one exact snapshot. Substituting a
later snapshot would make the same durable input set produce different plans.
