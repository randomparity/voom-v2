---
name: voom-sprint-5-design
description: Sprint 5 design for pure plan DAG generation and dry-run CLI inspection without creating durable execution state.
status: proposed
date: 2026-05-23
sprint: 5
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-22-voom-mvp-roadmap-rescope-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md
---

# VOOM Sprint 5 - Plan DAG Generation And Dry-Run CLI

## 1. Purpose

Sprint 5 turns accepted policy intent and policy-domain inputs into an
inspectable execution-plan projection. The sprint proves that the
control plane can explain what it would do before any worker is
dispatched and before any durable execution rows are created.

Sprint 5 follows the newer roadmap rescope. Older Sprint 1 references
that grouped real ffprobe, FFmpeg, MKVToolNix, backup, verification, and
commit workers into Sprint 5 are superseded. Real media workers remain
deferred to Sprint 10 and later.

The sprint boundary is strict: a Sprint 5 plan is a deterministic
projection, not queued work. It may be persisted in later sprints only
after execution semantics are designed.

## 2. Scope

Sprint 5 delivers:

- `voom-plan` as the owner of plan-domain types and pure plan
  generation.
- Deterministic `ExecutionPlan` JSON generated from a Sprint 4
  `CompiledPolicy` and a Sprint 3 policy input set.
- Phase dependency handling based on compiled policy phase order and
  `depends_on`.
- Stable plan node ids, plan hash, operation ids, and edge ids.
- Scheduling metadata placeholders that are inspectable but not used
  for execution.
- Fixture-backed plan golden files for compliant and noncompliant
  synthetic inputs.
- Control-plane use cases that compose policy loading, input-set
  loading, and plan generation without writing execution state.
- CLI plan-only inspection commands that emit the existing single JSON
  envelope.
- Stable planning diagnostics and deterministic CLI error envelopes.
- Closeout acceptance matrix tying the CLI JSON contract to plan schema
  fixtures and error behavior.

Sprint 5 explicitly does not deliver:

- Durable execution-plan tables.
- Job or ticket creation from plans.
- Worker dispatch.
- Compliance reports.
- Full planning semantics for every Sprint 4 compiled operation.
- Issue creation or issue state changes from planning.
- Synthetic policy execution.
- Real media scan, probe, transcode, remux, backup, verification, or
  commit workers.
- Daemon scheduling.
- Remote workers or artifact access plans.
- UI plan rendering.
- Plugin-defined operation schemas.

## 3. Architecture

`voom-plan` becomes a real crate. It depends on `voom-core` and
`voom-policy`, but not on `voom-store`, `voom-control-plane`, `voom-cli`,
or worker crates. This preserves a pure planning boundary: the same
planner can be exercised by unit tests, control-plane use cases, CLI
commands, API handlers, and future daemon flows without database or
process side effects.

`voom-policy` remains the owner of policy source parsing, validation,
compiled policy IR, and policy input models. Sprint 5 should not add
planner-only behavior to the policy crate. Policy compilation answers
"what did the user declare"; planning answers "what work would this
declaration imply for this input."

`voom-control-plane` owns composition. It loads policy versions and
policy input sets from SQLite through existing repositories, converts
repository rows into the planner's input shape, calls `voom-plan`, and
returns the projection. Control-plane planning use cases must not call
job, ticket, lease, artifact, issue, or event mutation APIs.

`voom-cli` owns agent-facing inspection. It adds plan-only commands that
return the existing CLI envelope shape. Logs remain stderr-only, and
stdout remains exactly one JSON object per invocation.

The flow is:

```text
CompiledPolicy + PolicyInputSet
          |
          v
   voom-plan planner
          |
          v
  ExecutionPlan projection
          |
          v
 control-plane / CLI JSON
```

No row in `jobs`, `tickets`, `ticket_dependencies`, `leases`, `events`,
`issues`, or artifact tables may be inserted as part of Sprint 5 plan
generation.

## 4. Planner Inputs

The planner accepts:

- a `CompiledPolicy`;
- a policy input set with snapshots, bundle targets, identity evidence,
  quality selections, and issue inputs;
- a deterministic planning context.

The planning context includes:

- plan schema version;
- optional stable policy document id and policy version id;
- optional stable policy input set id;
- input source label for source-only CLI planning;
- optional generated-at timestamp, supplied explicitly by a caller when
  it wants invocation metadata in the projection;
- deterministic feature flags, initially empty.

The planner must not read the wall clock. Source-only CLI planning omits
generated-at metadata by default so identical policy and input contents
produce identical JSON across invocations. Tests that include a
generated-at value must inject a fixed timestamp.

`voom-plan` should define its own `PlanningRequest` and
`PlanningContext` types rather than using control-plane repository
types directly. This avoids coupling the planner to SQLite row shapes
and keeps source-only CLI planning possible.

Repository-backed input sets should be converted into the same planning
input shape as fixture-backed input sets. If the repository shape cannot
round-trip a field from `PolicyInputSetDraft`, Sprint 5 should add an
explicit converter test that documents the chosen behavior.

## 5. Execution Plan Model

`ExecutionPlan` is a serializable, deterministic projection. It is not a
worker protocol and not a durable execution row.

The top-level plan contains:

- `schema_version`;
- `plan_id`;
- `plan_hash`;
- policy identity: slug, source hash, optional document id, optional
  version id;
- input identity: slug or source label, optional input-set id, fixture
  labels;
- optional `generated_at`;
- `summary`;
- `nodes`;
- `edges`;
- `warnings`;
- `diagnostics`;
- `provenance`.

`summary` contains:

- total node count;
- executable node count;
- no-op node count;
- blocked node count;
- target count;
- operation counts by kind.

Each plan node contains:

- stable `node_id`;
- phase name;
- ordinal within the topological plan order;
- target reference;
- operation kind;
- operation payload;
- node status: `planned`, `no_op`, or `blocked`;
- status reason;
- capability hints;
- scheduling hints;
- resource estimates;
- artifact expectations;
- safety hints.

Each edge contains:

- stable `edge_id`;
- `from_node_id`;
- `to_node_id`;
- dependency kind.

Sprint 5 dependency kind is limited to `phase_depends_on`. Later
sprints may add artifact, verification, approval, rollback, or
runtime-use dependencies.

Plan ids and node ids must be deterministic. They should be derived
from canonical serialized plan inputs and stable path components rather
than database autoincrement values. If a policy version id or input-set
id is available, it may contribute to the identity block, but the same
source-only plan generated from identical policy and input contents must
produce the same node and edge ids.

`plan_hash` is computed from the deterministic JSON projection excluding
`plan_hash` itself and fields that represent the invocation host or
generated-at metadata. The hash includes plan diagnostics and warnings
because those fields are part of the machine-readable projection an
agent may act on. If `generated_at` is included in the public
projection, it must not participate in `plan_hash`.

`plan_id` is derived from the same canonical preimage as `plan_hash`,
but it is a shorter stable identifier for humans and logs. It must not
be derived by hashing the already-hashed public projection.

## 6. Planning Semantics

Sprint 5 planning is intentionally conservative. It plans only from
facts represented by Sprint 3 inputs and operations represented by the
Sprint 4 compiled model.

Required Sprint 5 operation behavior is limited to a deterministic
container-planning slice:

- `SetContainer { container: "mkv" }` produces a planned node for each
  media snapshot whose container is known and not `mkv`.
- `SetContainer { container: "mkv" }` produces a no-op node for each
  media snapshot whose container is already `mkv`; summary fields count
  that node. The planner must not choose between no-op node emission and
  summary-only reporting because golden output needs one stable shape.
- `SetContainer` against a snapshot with unknown container produces a
  blocked node with a diagnostic explaining that the planner lacks the
  fact needed to decide.
- Track operations (`KeepTracks`, `RemoveTracks`, `ReorderTracks`,
  `SetDefaults`, `ClearTrackActions`) produce blocked nodes with either
  `unsupported_operation_for_sprint5` or `insufficient_snapshot_facts`.
  Sprint 5 must not silently skip them.
- Tag operations (`ClearTags`, `SetTag`, `DeleteTag`) produce blocked
  nodes with `unsupported_operation_for_sprint5` because Sprint 3 inputs
  do not define target metadata facts.
- Conditional blocks and rules are traversed deterministically. If a
  condition can be resolved from Sprint 3 inputs, only the selected
  branch contributes nodes. If a condition cannot be resolved, operations
  under that branch produce blocked nodes with
  `insufficient_snapshot_facts`.

The planner must never invent media facts. If a field is absent from a
snapshot, the plan output records the uncertainty.

The first required golden fixture pair is:

- compliant synthetic input plus a container policy produces an empty or
  no-op plan with explicit rationale; for a matching media snapshot this
  means a no-op node, not only a summary count;
- noncompliant synthetic input plus the same policy produces a planned
  containerization node and deterministic phase dependency structure.

Sprint 5 does not compute full compliance reports. The plan may include
localized no-op or blocked reasons, but the user-facing compliance
report model belongs to Sprint 6.

## 7. Scheduling Metadata

Sprint 5 records scheduling metadata so later scheduler work has a
stable place to attach decisions, but no scheduling decision is made.

Each planned node may include:

- priority class, defaulting to `normal`;
- operation capability hint, such as `remux_container`;
- estimated CPU class, defaulting to `unknown`;
- estimated GPU class, defaulting to `none`;
- estimated disk bytes, defaulting to `unknown`;
- estimated network bytes, defaulting to `unknown`;
- expected duration, defaulting to `unknown`;
- concurrency key, defaulting to target identity when available.

These fields are descriptive placeholders in Sprint 5. They must not be
used to select workers, claim tickets, or affect retry behavior.

## 8. Diagnostics And Errors

Planning diagnostics are separate from Sprint 4 policy diagnostics.
They describe problems discovered after a policy has already compiled.

Required diagnostic codes include stable variants for:

- missing policy input target;
- unsupported operation for Sprint 5 planning;
- insufficient snapshot facts;
- ambiguous target selection;
- empty policy phases;
- empty input set;
- invalid planning request;
- deterministic serialization failure.

Planning diagnostics have severity `error` or `warning`, a stable code,
message, optional target reference, optional phase name, optional
operation kind, and optional suggestion.

Public CLI/control-plane errors should map to stable envelope codes:

- policy parse failures keep Sprint 4's `POLICY_PARSE_ERROR`;
- policy validation and compile failures keep Sprint 4's
  `POLICY_VALIDATION_ERROR`;
- planning failures use `PLAN_GENERATION_ERROR`;
- missing policy versions or input sets use existing `NOT_FOUND`;
- database access failures use existing database error codes.

Warnings from policy compilation and planning should both be visible in
the plan output. The top-level CLI envelope `warnings` field remains a
short host-facing list; detailed machine-readable warnings belong in
`data.warnings` or `data.diagnostics`.

## 9. CLI Contract

Sprint 5 adds a plan-only command family with two public commands:

1. Source-only planning:

```text
voom plan dry-run --policy-file path/to/policy.voom --input-fixture synthetic_noncompliant_transcode_needed
```

2. Durable planning:

```text
voom plan show --policy-version-id 1 --input-set-id 1
```

Both workflows emit one JSON envelope on stdout. Both are read-only with
respect to execution state. Durable planning may read from SQLite but
must not insert, update, or delete execution rows.

Source-only planning must not open a control-plane database connection
and must not require an initialized database. It has all required inputs
from the policy file and fixture or input file. This preserves the
`connect()` versus `init()` invariant: source-only dry-run cannot create
database files or directories as a side effect of asking for a plan.

The successful envelope has:

```json
{
  "schema_version": "0",
  "command": "plan",
  "status": "ok",
  "data": {
    "plan": {
      "schema_version": 1,
      "plan_id": "plan_...",
      "plan_hash": "sha256:...",
      "nodes": [],
      "edges": []
    }
  },
  "warnings": [],
  "error": null
}
```

Error envelopes use the existing CLI contract: `status` is `error`,
`data` is `null`, and `error.code` is stable. Detailed policy
diagnostics may be summarized in `error.message` for Sprint 5 CLI
errors; a richer diagnostic error payload can be added only if the
existing envelope type is explicitly extended across CLI tests.

## 10. Control-Plane Use Cases

Sprint 5 should add narrow use cases:

- plan from compiled policy and in-memory policy input draft;
- plan from policy source and in-memory policy input draft;
- plan from accepted policy version id and policy input set id.

The first two support source-only CLI tests and unit-level composition.
The third supports durable planning against Sprint 4 policy registry
rows and Sprint 3 policy input-set rows.

The durable use case must:

- read the policy version;
- deserialize `policy_versions.compiled_json` into `CompiledPolicy`;
- verify the deserialized policy source hash and schema version match
  the policy-version row;
- read the policy input set;
- convert repository rows into planner input;
- call `voom-plan`;
- return the plan projection.

The durable use case must not recompile accepted policy source as its
normal path. Recompilation would make old accepted versions vulnerable
to compiler drift and would undermine Sprint 4's immutable compiled
projection. Source-only planning may compile policy source because no
accepted version exists yet.

It must not:

- create jobs;
- create tickets;
- create leases;
- append events;
- create issues;
- mutate artifacts;
- mutate policy versions or input sets.

## 11. Testing

Sprint 5 verification includes:

- `voom-plan` unit tests for plan ids, node ids, edge ids, plan hash,
  phase dependencies, summaries, and diagnostics.
- Fixture tests proving compliant and noncompliant synthetic inputs
  produce deterministic golden plan JSON.
- Planner tests for known container, already-compliant container, and
  unknown-container blocked behavior.
- Planner tests proving unsupported or insufficiently evidenced Sprint
  4 operations fail loud with planning diagnostics.
- Planner tests proving track and tag operations produce deterministic
  blocked nodes rather than being silently skipped or opportunistically
  planned.
- Control-plane tests proving durable planning reads policy and input
  rows but does not change job, ticket, lease, event, issue, or artifact
  counts.
- Control-plane tests proving durable planning deserializes stored
  compiled JSON and detects source-hash or schema mismatches without
  recompiling accepted source.
- CLI golden-output tests for source-only dry-run success, durable plan
  success, policy parse error, policy validation error, missing input
  set, and plan generation error.
- CLI tests proving source-only dry-run succeeds with no database URL,
  no initialized database, and no filesystem creation outside the
  explicitly supplied input/output paths.
- Schema fixture tests that deserialize every golden plan through
  `voom-plan` public types.
- Documentation placeholder scan.
- `just ci`.

Tests must use the existing sibling unit-test layout for source files in
`src/`. CLI integration tests that use snapshots must be reviewed with
`cargo insta review` after deliberate output changes.

## 12. Acceptance Matrix

| Requirement | Sprint 5 coverage | Deferral |
|---|---|---|
| Turn compiled policy and policy inputs into inspectable work intent. | `voom-plan::ExecutionPlan` generated from `CompiledPolicy` plus policy input set. | Compliance report wording and issue creation move to Sprint 6. |
| Keep planning side-effect free. | Planner is a pure crate and control-plane use cases are read-only for execution state. | Durable execution-plan storage is deferred until execution semantics require it. |
| Preserve phase dependencies. | Plan edges represent compiled phase `depends_on` relationships using stable ids. | Artifact, verification, approval, rollback, and use-lease edges are later sprints. |
| Expose agent-friendly dry-run output. | CLI emits one JSON envelope containing deterministic plan JSON. | Web/API plan rendering is later. |
| Provide stable machine-readable failures. | Planning diagnostics and `PLAN_GENERATION_ERROR` cover planner-specific failures. | Rich nested CLI diagnostic error payloads are optional future envelope work. |
| Avoid real media and worker scope. | No real workers, no job/ticket creation, no dispatch. | Real scan/probe starts in Sprint 10; staged commit starts in Sprint 11. |

## 13. Open Decisions

No product decisions remain open for Sprint 5. Exact module names and
fixture filenames can be finalized in the implementation plan as long as
they preserve this design's boundaries. Public CLI command names are
fixed in this design.
