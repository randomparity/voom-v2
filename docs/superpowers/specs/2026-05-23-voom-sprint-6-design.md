---
name: voom-sprint-6-design
description: Sprint 6 design for compliance reports, durable noncompliance issues, and narrow synthetic execution of Sprint 5-supported plan nodes.
status: proposed
date: 2026-05-23
sprint: 6
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-22-voom-mvp-roadmap-rescope-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md
  - docs/superpowers/specs/2026-05-23-voom-sprint-5-design.md
---

# VOOM Sprint 6 - Compliance Reports And Synthetic Policy Execution

## 1. Purpose

Sprint 6 closes the policy and planning MVP by turning Sprint 5 execution
plans into agent-readable compliance reports, actionable durable issues, and
narrow synthetic execution. It proves the product can explain whether stored
policy inputs comply with an accepted policy, persist noncompliance findings,
and run only the supported planned work through the existing synthetic worker
path.

Sprint 6 deliberately stays within Sprint 5 operation semantics. It does not
expand the policy evaluator beyond operations that Sprint 5 can already plan.
For this sprint, that means the supported compliance slice is container
normalization through `set_container`, with `set_container` planned work mapped
to synthetic remux execution.

The sprint boundary is strict: compliance reporting is layered on top of the
Sprint 5 `ExecutionPlan`. Sprint 6 must not introduce a second interpreter for
policy text or compiled policy operations.

## 2. Scope

Sprint 6 delivers:

- A deterministic `ComplianceReport` model generated from a Sprint 5
  `ExecutionPlan`.
- Report status and check status mapping for `planned`, `no_op`, and `blocked`
  plan nodes.
- Stable report ids, report hashes, report summaries, diagnostics, and JSON
  fixtures.
- Durable control-plane use cases for report generation, issue application, and
  synthetic policy execution.
- A narrow issue repository surface for creating, updating, deduplicating, and
  resolving `policy_noncompliant` issues.
- A durable issue dedupe key so repeated report application is idempotent.
- CLI commands for compliance report, compliance apply, and compliance execute.
- A mapper from supported planned `PlanNode`s to a minimal synthetic
  `WorkflowPlan`.
- Synthetic execution through the existing Sprint 2 `WorkflowExecutor` path.
- Event and state inspection sufficient to verify report application and
  synthetic execution.
- Closeout acceptance matrix tying reports, issues, CLI envelopes, and
  execution behavior to the architecture.

Sprint 6 explicitly does not deliver:

- Broader policy operation semantics beyond Sprint 5-supported planning.
- Real media scan, probe, remux, transcode, backup, verification, or commit
  workers.
- Remote execution.
- Daemon loops.
- Web UI reporting.
- Plugin-defined policy operations or plugin-defined compliance schemas.
- Durable execution-plan storage.
- Scheduler scoring, locality, path mapping, or artifact access plans.
- Compliance decisions based on worker execution results.

## 3. Architecture

Compliance reporting is report-first and plan-derived:

```text
CompiledPolicy + PolicyInputSet
          |
          v
   Sprint 5 ExecutionPlan
          |
          v
   ComplianceReport
      |          |
      |          v
      |   durable policy_noncompliant issues
      v
planned supported nodes only
          |
          v
WorkflowExecutor synthetic run
```

`voom-plan` owns pure report-domain types and report generation. It already owns
plan-domain types and pure plan generation, so deriving compliance from a plan
keeps policy interpretation in one place. `voom-plan` must not depend on
`voom-store`, `voom-control-plane`, `voom-cli`, or worker crates.

`voom-control-plane` owns composition. It loads accepted policy versions and
policy input sets, calls the Sprint 5 planner, generates a report, optionally
applies issue changes, and optionally executes supported planned nodes through
`WorkflowExecutor`. "Accepted policy version" means the requested
`policy_version_id` must be the current accepted version on its owning
`policy_documents` row at command time, not merely a historical immutable
version row.

`voom-store` owns narrow persistence support for issue lifecycle changes. The
existing `issues` and `issue_links` tables are the durable store for
noncompliance findings, but Sprint 6 adds a `dedupe_key` column to make
idempotent report application explicit.

`voom-cli` owns the public JSON envelope commands. Each command emits exactly
one JSON object on stdout. Logs remain stderr-only.

`WorkflowExecutor` remains the synthetic execution mechanism. Sprint 6 adds a
small bridge from planned Sprint 5 nodes to `WorkflowPlan`; it does not reuse
the broad Sprint 2 default synthetic workflow as the policy execution path.

## 4. Compliance Report Model

`ComplianceReport` is a deterministic serializable projection generated from an
`ExecutionPlan`. It does not read the database, wall clock, filesystem, or
worker state.

The top-level report contains:

- `schema_version`;
- `report_id`;
- `report_hash`;
- `plan_id`;
- `plan_hash`;
- policy identity copied from the plan;
- input identity copied from the plan;
- `summary`;
- `checks`;
- `diagnostics`;
- `provenance`.

Report status is:

- `compliant`: every relevant check is compliant, and no check is noncompliant
  or blocked;
- `noncompliant`: at least one supported check is noncompliant and no check is
  blocked;
- `blocked`: no supported check is noncompliant, but at least one check is
  blocked;
- `mixed`: at least one supported check is noncompliant and at least one check
  is blocked;
- `not_applicable`: the plan has no nodes.

Each Sprint 5 `PlanNode` maps to one `ComplianceCheck`:

- `NodeStatus::NoOp` maps to `check_status: compliant`;
- `NodeStatus::Planned` maps to `check_status: noncompliant`;
- `NodeStatus::Blocked` maps to `check_status: blocked`.

Each check records:

- `check_id`;
- source `node_id`;
- target reference;
- compliance kind;
- operation kind;
- desired state payload;
- observed state payload when the plan exposes it;
- check status;
- reason;
- issue action hint;
- execution eligibility.

For Sprint 6, `set_container` maps to compliance kind `container`.
`set_container` against a non-MKV snapshot is noncompliant. `set_container`
against an already-MKV snapshot is compliant. `set_container` against an
unknown container is blocked because the planner lacks the fact required to
decide.

Unsupported Sprint 5 operations remain blocked checks with diagnostics. They
are visible in the report, but they do not become actionable media-library
issues by default.

`report_hash` is computed from canonical deterministic report JSON excluding
`report_hash` itself and any future invocation metadata. `report_id` is derived
from a stable preimage and must not be derived by hashing the already-hashed
report.

## 5. Issue Application

Sprint 6 persists issues only from durable mutating compliance commands. A
source-only dry run must not create, update, or resolve durable issues.

Issue application rules:

- A `noncompliant` check creates or updates a `policy_noncompliant` issue with
  status `planned`.
- A `blocked` check caused by insufficient facts creates or updates a
  `policy_noncompliant` issue with status `open`.
- Unsupported operation checks remain report diagnostics and do not create
  durable issues by default.
- A matching previously open or planned issue is marked `resolved` when a later
  durable report contains the matching check as `compliant`, or when a newer
  accepted version of the same policy document no longer emits that compliance
  check.
- Re-applying the same report is idempotent.

The issue dedupe key is derived from:

```text
policy_document_id + input_set_id + target_ref + compliance_kind + operation_kind
```

The dedupe key deliberately uses `policy_document_id`, not `policy_version_id`.
Policy version rows are immutable and older accepted versions remain queryable,
but there should be one live issue for "this policy document currently requires
this target compliance property." A new accepted version of the same policy
document updates or resolves that issue instead of leaving stale issues from the
previous accepted version behind. The issue body, lifecycle events, and report
provenance still record the concrete `policy_version_id` that produced each
change.

Sprint 6 adds a nullable `dedupe_key` column to `issues` plus a unique partial
index over non-null `dedupe_key` values. The migration must use SQLite-supported
steps (`ALTER TABLE ... ADD COLUMN`, then `CREATE UNIQUE INDEX ... WHERE
dedupe_key IS NOT NULL`) rather than a table-level rewrite hidden behind the
word "column." The column is nullable so existing issue rows remain valid and
because not every issue kind needs the same deduplication strategy.
`policy_noncompliant` issues created by Sprint 6 must have a non-empty dedupe
key.

Issue application must run in a transaction. It upserts each actionable check by
`dedupe_key`, treats unique-index conflicts as a retryable read/update path, and
resolves matching `policy_noncompliant` issues in the same policy document,
input set, target, compliance kind, and operation kind when the current report
marks that check compliant or no longer emits that compliance check for the
current accepted policy version. It must not resolve unrelated policy issues for
the same target, same input set, or same policy document through broader target
scans.

Issue titles and bodies should be deterministic and concise. They explain the
target, policy version, desired state, observed state when known, and why the
issue is planned or open. User-facing prose is not part of the dedupe key.

Issue lifecycle changes should append issue events when the existing event
vocabulary supports them. If the current vocabulary lacks required issue
events, Sprint 6 adds only minimal issue lifecycle events required to inspect
create, update, and resolve behavior.

## 6. Synthetic Execution

Sprint 6 execution starts from a generated plan and report. It submits only
supported planned nodes. Issue application and workflow submission are separate
durable phases inside `execute`: issues are applied first and are not rolled
back if workflow submission or synthetic execution later fails. The command
response must make that partial state explicit by returning the report, issue
application summary, and execution summary or execution diagnostic.

Execution rules:

- Execute only `PlanNode`s with `status = planned`.
- Execute only Sprint 6-supported operation mappings.
- Initially, `set_container` maps to synthetic `OperationKind::Remux`.
- `no_op` nodes are not submitted.
- `blocked` nodes are not submitted.
- If there are no executable planned nodes, the command succeeds with a report,
  issue application summary, and an execution summary showing zero submitted
  work without creating a job.
- If a planned node has no Sprint 6 mapping, execution fails with a stable
  policy execution diagnostic before creating a job. Issue application has
  already occurred and is reported as completed.
- The mapper preserves the source plan node id in workflow node ids or payload
  metadata so tickets, events, and execution summaries can be traced back to
  compliance checks.

The generated `WorkflowPlan` is minimal. It contains one workflow operation per
supported planned plan node and no injected scan, probe, hash, transcode,
backup, or external-sync steps.

The execution summary contains:

- `plan_id`;
- `report_id`;
- `job_id` when work was submitted;
- submitted node count;
- skipped no-op count;
- blocked count;
- dispatch count;
- failure count;
- per-operation summary.

Synthetic execution does not decide compliance. Compliance is decided before
execution from the plan. Execution proves the policy-derived planned work can
flow through the existing worker path.

## 7. CLI Contract

Sprint 6 adds a `compliance` command family.

Read-only report generation:

```text
voom compliance report --policy-version-id 1 --input-set-id 1
```

This command opens the database, loads durable policy and input rows, generates
the plan and report, and emits them. It does not create issues, jobs, tickets,
leases, or events.

Issue application:

```text
voom compliance apply --policy-version-id 1 --input-set-id 1
```

This command generates the plan and report, applies issue lifecycle changes,
and emits the report plus an issue application summary. It does not execute
work.

Synthetic execution:

```text
voom compliance execute --policy-version-id 1 --input-set-id 1
```

This command generates the plan and report, applies issue lifecycle changes,
executes supported planned nodes through `WorkflowExecutor`, and emits the
report plus execution summary.

All repository-backed compliance commands reject a `policy_version_id` that is
not the current accepted version for its policy document with
`POLICY_VALIDATION_ERROR` rather than silently planning against superseded policy
intent. `NOT_FOUND` remains reserved for policy version ids or input set ids
that do not exist.

All commands emit the existing single JSON envelope. Successful `execute`
output has this shape:

```json
{
  "schema_version": "0",
  "command": "compliance",
  "status": "ok",
  "data": {
    "report": {},
    "execution": {
      "submitted_node_count": 1,
      "job_id": 12,
      "dispatch_count": 1,
      "failure_count": 0
    }
  },
  "warnings": [],
  "error": null
}
```

Stable error code mapping:

- policy parse failures keep `POLICY_PARSE_ERROR`;
- policy validation and compile failures keep `POLICY_VALIDATION_ERROR`;
- planning failures keep `PLAN_GENERATION_ERROR`;
- compliance report failures use `COMPLIANCE_REPORT_ERROR`;
- policy-plan execution bridge failures use `POLICY_EXECUTION_ERROR`;
- missing policy versions or input sets use `NOT_FOUND`;
- database access failures use existing database error codes.

Sprint 6 does not add a source-only compliance CLI. Sprint 5 already provides
source-only plan dry-run inspection, while Sprint 6 acceptance depends on
durable issues and synthetic execution.

## 8. Control-Plane Use Cases

Sprint 6 adds narrow use cases:

- generate compliance report from accepted policy version id and policy input
  set id;
- apply compliance issues from accepted policy version id and policy input set
  id;
- execute supported planned policy work from accepted policy version id and
  policy input set id.

The report use case must be read-only. It must not insert, update, or delete
issue, event, job, ticket, lease, artifact, policy, or input rows.

The apply use case may mutate only issues and issue lifecycle events. It must
not create jobs, tickets, leases, artifacts, policy rows, or input rows.

The execute use case may mutate issues and issue lifecycle events, then create
jobs, tickets, leases, and workflow events through `WorkflowExecutor`. It must
not mutate media identity, artifacts, policy rows, or input rows.

The durable policy-loading path continues Sprint 5 behavior: accepted compiled
policy JSON is deserialized from the policy version row and is not recompiled
as the normal path.

Before any of these use cases generate a plan, the control plane must verify
that the requested policy version exists and is still the policy document's
`current_accepted_version_id`. This prevents report, issue, or execution state
from being produced against stale policy versions that remain queryable because
policy version rows are immutable.

## 9. Diagnostics

Compliance diagnostics are separate from policy diagnostics and planning
diagnostics. They describe problems discovered while deriving or applying a
report from an already-generated plan.

Required diagnostic codes include stable variants for:

- unsupported compliance operation;
- unsupported execution operation;
- missing durable policy identity for issue application;
- missing durable input identity for issue application;
- invalid report request;
- issue application conflict;
- deterministic serialization failure.

Diagnostics include severity, code, message, optional plan id, optional report
id, optional node id, optional check id, optional target reference, and optional
suggestion.

## 10. Testing

Sprint 6 verification includes:

- `voom-plan` unit tests for report status mapping:
  `no_op -> compliant`, `planned -> noncompliant`, `blocked -> blocked`, and
  planned plus blocked -> `mixed`.
- Report id and hash tests proving deterministic output from identical plans.
- Golden report JSON fixtures for compliant, noncompliant, blocked, and mixed
  synthetic cases.
- Control-plane tests proving durable report generation is read-only.
- Issue application tests proving `compliance apply` creates planned and open
  issues, deduplicates repeated runs, resolves matching issues after
  compliance, and does not create issues for unsupported operation scope gaps.
- Migration/repository tests for `issues.dedupe_key`, including the nullable
  column, unique partial index, repeated apply, and conflict retry/update path.
- Execution bridge tests proving only planned supported nodes become workflow
  nodes.
- Integration tests proving `compliance execute` creates a job and tickets and
  runs `set_container -> Remux` through `WorkflowExecutor`.
- Integration tests proving `compliance execute` reports issue application as
  completed when later bridge validation or workflow execution fails, and that
  no-executable-work execution creates no job.
- Control-plane tests proving report, apply, and execute reject stale
  non-current policy version ids even though the underlying policy version row
  still exists.
- CLI envelope snapshots for report, apply, execute, stable missing-input
  errors, report generation errors, and policy execution errors.
- Explicit mutation-count tests:
  - `report` mutates no issue, job, ticket, lease, event, or artifact rows;
  - `apply` mutates issues and issue events only;
  - `execute` mutates issues, issue events, jobs, tickets, leases, and workflow
    events through the workflow path.
- Documentation placeholder scan.
- `just ci`.

Tests must use the existing sibling unit-test layout for source files in `src/`.
CLI integration tests that use snapshots must be reviewed with
`cargo insta review` after deliberate output changes.

## 11. Acceptance Matrix

| Requirement | Sprint 6 coverage | Deferral |
|---|---|---|
| Explain compliance and noncompliance for the policy MVP. | `ComplianceReport` generated from Sprint 5 `ExecutionPlan` with deterministic checks and summaries. | Broader operation semantics are deferred until their planner support lands. |
| Preserve one policy interpretation path. | Reports derive from plans instead of re-reading compiled policy operations. | Future richer reports may add fields to the plan before deriving reports. |
| Persist actionable noncompliance. | Durable `policy_noncompliant` issues are created, updated, deduplicated, and resolved from durable report application. | Non-policy issue providers and richer priority policies remain later work. |
| Avoid fake work for unsupported scope. | Unsupported operations stay report diagnostics and do not create issues or execution nodes by default. | Plugin and broader operation support are later sprints. |
| Execute policy-derived synthetic work. | Supported planned `set_container` nodes map to synthetic `Remux` and run through `WorkflowExecutor`. | Real media execution starts in Sprint 10 and later. |
| Preserve CLI envelope contract. | `voom compliance report/apply/execute` emit exactly one JSON envelope with stable error codes. | Web UI reporting is deferred. |

## 12. Open Decisions

No product decisions remain open for Sprint 6. Exact Rust module names,
fixture filenames, and event enum names can be finalized in the implementation
plan as long as they preserve this design's boundaries.
