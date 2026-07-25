# Issue 329 implementation plan: Evaluate published stream conditions

Base branch: `main`

Base commit: `856bacb8ff42954fede8830007145d0674d937bb`

Guardrails:

- `cargo test -p voom-policy condition`
- `cargo test -p voom-policy policy_fixtures`
- `cargo test -p voom-plan condition`
- `cargo test -p voom-store workflow_file_run_start`
- `cargo test -p voom-control-plane stream_summary`
- `cargo test -p voom-control-plane stored_stream`
- `cargo test -p voom-control-plane reconcile_resume`
- `cargo test -p voom-control-plane --test published_grammar_corpus`
- `prek run`
- `just ci`

## Step 1: Close the published condition boundary

Files:

- `crates/voom-policy/src/compile/validate/conditions.rs`
- `crates/voom-policy/src/compile/validate_test.rs`
- `crates/voom-policy/src/compile/pipeline_test.rs`
- `crates/voom-policy/src/fixtures/policy_fixtures.rs`
- `crates/voom-policy/src/fixtures/policy_fixtures_test.rs`
- `crates/voom-policy/fixtures/diagnostics/production-normalize-reduced.json`
- `crates/voom-policy/fixtures/compiled/production-normalize-reduced.json`
- `crates/voom-plan/src/eligibility.rs`
- `crates/voom-plan/src/eligibility_test.rs`
- `crates/voom-plan/src/lib.rs`
- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner_test.rs`
- `crates/voom-control-plane/src/cases/policy/plans.rs`
- `crates/voom-control-plane/src/cases/policy/plans_test.rs`

Red tests:

- canonical audio/subtitle `exists` and `count` source forms compile;
- all six count comparators compile;
- filtered exists, video/attachment targets, non-numeric comparators, extra
  tokens, signs, non-ASCII digits, and overflowing counts fail compilation;
- `production-normalize-reduced.voom` is a negative source fixture with a
  diagnostic golden;
- its unchanged compiled JSON still deserializes and fails typed eligibility;
- canonical stored compiled shapes remain deserializable;
- stored `exists` and `count` objects with missing or extra keys fail with a
  deterministic JSON path before serde can erase the shape;
- raw traversal reaches phase guards, nested conditional operations, rules,
  nested rule operations, and every Boolean child;
- unrelated metadata, provenance, profile, and compiled-value objects tagged
  `exists` or `count` remain readable;
- unknown fields outside `exists` and `count` retain their current behavior
  under #344;
- one unpublished stream leaf invalidates its complete Boolean tree;
- an invalid leaf in a later phase fails full-policy validation;
- eligibility diagnostics use `invalid_planning_request`, the stable prefix,
  and deterministic phase/placement/operation/rule/Boolean context;
- `generate_plan`, `plan_phase`, and stored policy loading preserve the same
  eligibility diagnostic; and
- stream conditions in `run_if` fail while canonical predicates retain their
  current unknown behavior.

Implementation:

- narrow source validation only for `Exists` and `Count`;
- move the filtered-exists source fixture into the invalid corpus while
  retaining its compiled golden as compatibility evidence;
- add a raw JSON gate before stored compiled-policy deserialization;
- traverse only schema-defined phase guard, operation/rule condition, nested
  operation, and Boolean-child edges in deterministic array order;
- emit deterministic JSON-pointer paths without descending into arbitrary
  JSON-bearing fields;
- add one pure full-policy typed eligibility pass;
- accept published stream shapes only on ordinary condition surfaces;
- reject stream conditions in `run_if` without changing other condition
  behavior; and
- call the same pass from both planner entry points and stored policy loading.

Expected failure before implementation:

- source validation accepts broader stream shapes;
- serde erases extra keys;
- no complete placement-aware eligibility pass exists.

Verification:

```text
cargo test -p voom-policy condition
cargo test -p voom-policy policy_fixtures
cargo test -p voom-plan condition
```

Commit:

`fix(policy): enforce published stream condition shapes`

## Step 2: Rehydrate authoritative stored stream facts

Files:

- `crates/voom-store/src/repo/media/identity.rs`
- `crates/voom-store/src/repo/media/identity_test.rs`
- `crates/voom-control-plane/src/media_snapshot.rs`
- `crates/voom-control-plane/src/media_snapshot_test.rs`
- `crates/voom-control-plane/src/cases/policy/policy_inputs.rs`
- `crates/voom-control-plane/src/cases/policy/policy_inputs_test.rs`
- `crates/voom-control-plane/src/cases/policy/plans.rs`
- `crates/voom-control-plane/src/cases/policy/plans_test.rs`
- `crates/voom-control-plane/src/cases/policy/compliance.rs`
- `crates/voom-control-plane/src/cases/policy/compliance_test.rs`

Red tests:

- missing streams stay missing through both snapshot projections;
- null and non-array streams are copied rather than normalized;
- arrays retain exact members and derive the video count;
- missing and non-array streams retain the legacy zero-video sentinel;
- container/remux behavior is unchanged for every unavailable inventory shape;
- stored plan and report replace linked cached facts from the active snapshot;
- unlinked durable `FileVersion` targets resolve the same active snapshot;
- an optional snapshot link proves the selected version's provenance;
- duplicate selected lineages fail before any active-tip read;
- stored policies containing eligible `Exists`/`Count` reject non-file
  members, while other policies retain their existing stored behavior;
- eligibility rejection wins over non-file-member rejection;
- the active version and latest snapshot come from one repository statement;
- the repository selects greatest live version id and greatest exact-version
  snapshot id without fallback from malformed newest facts;
- missing targets, invalid links, and unavailable current snapshots fail with
  the stable stored-facts contract;
- repository failures propagate unchanged; and
- store-free planning keeps supplied snapshot facts.

Implementation:

- make the shared stream-summary projection preserve the source stream shape;
- route both production snapshot projections through it;
- add one identity-repository read returning the active chain tip and its
  latest snapshot as a coherent per-file pair;
- add an async stored-input adapter returning `StoredPlanningInput` with a
  projected draft and authority-bearing `ResolvedFileInput` records;
- perform selected-version/link validation and duplicate-lineage grouping
  before active-pair reads;
- make non-file handling condition-aware after eligibility succeeds;
- preserve each member ordinal and current authority record; and
- route stored plan and report through the adapter.

Expected failure before implementation:

- missing streams are normalized to `[]`;
- stored planning trusts cached summaries;
- active version and snapshot require separate reads.

Verification:

```text
cargo test -p voom-control-plane stream_summary
cargo test -p voom-control-plane stored_stream
```

Commit:

`fix(policy): use authoritative stored stream facts`

## Step 3: Persist crash-safe execution starts

Files:

- `migrations/0022_workflow_file_run_starts.sql`
- `crates/voom-store/src/migrator.rs`
- `crates/voom-store/src/schema_test.rs`
- `crates/voom-store/tests/migration_inventory.rs`
- `crates/voom-store/src/repo/execution/workflow_summaries.rs`
- `crates/voom-store/src/repo/execution/workflow_summaries_test.rs`
- `crates/voom-store/src/repo/mod.rs`
- `crates/voom-control-plane/src/cases/execution/jobs.rs`
- `crates/voom-control-plane/src/cases/execution/jobs_test.rs`
- `crates/voom-control-plane/src/cases/policy/compliance.rs`
- `crates/voom-control-plane/src/cases/policy/compliance_test.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod.rs`
- `crates/voom-control-plane/src/workflow/coordinator/planning.rs`
- `crates/voom-control-plane/src/workflow/coordinator/resume.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`

Red tests:

- the migration creates the strict job-owned run-start table with the accepted
  foreign keys, checks, and primary key;
- repository batch insertion is atomic and inspection is deterministic;
- fresh job opening atomically publishes `job.opened` and one start row per
  branch at phase zero;
- fresh execution uses one adapter result for its initial report, safety and
  worker checks, branch ids, run starts, and first phase;
- production and injected-runtime execute paths share that preparation;
- first-phase files use the retained active version and snapshot without a
  second authority read;
- resume uses the prior starting version when no committed row exists;
- historical selected versions already superseded before the prior job do not
  create false backfills;
- a real commit without a row backfills exactly once;
- an intermediate resumed job with no ordinary rows retains its nonzero start;
- prior row shapes accept only an optional committed empty-ticket seed at
  `start - 1` and a contiguous tail from `start`;
- gaps, out-of-range starts or rows, early rows, rows after blocked, lineage
  mismatches, and changed terminal tips fail before opening a new job;
- terminal branches are recorded at exactly `phase_count` and never backfill;
- pre-migration file jobs fail with `resume state is incomplete`;
- zero-file jobs require no run-start rows and retain zero-work behavior;
- current branches `{a, b}` with prior starts `{a, c}` fail exact-set
  validation with `resume state is incomplete` before any durable write;
- job creation, start rows, and seed backfills roll back together on failure;
- every #329 preparation rejection leaves issue, job, ticket, file-version,
  run-start, and workflow-summary rows unchanged;
- post-commit phases still use the produced version's refreshed snapshot; and
- all existing ADR 0009 resume behavior remains green.

Implementation:

- add the `workflow_file_run_starts` migration and typed repository model;
- register the migration in both `MIGRATOR` and the exact file inventory;
- batch-load and batch-insert immutable job/branch starting cursors;
- split resume reconciliation into a read-only preparation that validates
  the exact prior/current branch set, row shape, lineage, terminality, and one
  lost commit;
- prepare the new job's post-reconciliation starts and optional seed rows;
- add one transaction that creates the job, appends `job.opened`, inserts every
  start, and inserts seed rows atomically;
- remove `PhaseFile.start_version_id`;
- build fresh and resumed `PhaseFile` values from retained authority records;
- replace duplicate fresh-execute report/coordinator loading with one internal
  preparation result; and
- preserve #352's plan-through-dispatch boundary without another authority
  read.

Expected failure before implementation:

- resume defaults to the historical input-set version and phase zero;
- jobs do not persist their per-file starting cursor;
- fresh report and coordinator preparation load authority independently.

Verification:

```text
cargo test -p voom-store workflow_file_run_start
cargo test -p voom-store --test migration_inventory
cargo test -p voom-control-plane reconcile_resume
cargo test -p voom-control-plane stored_stream
```

Commit:

`fix(policy): persist authoritative execution starts`

## Step 4: Evaluate published stream conditions

Files:

- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner_test.rs`

Red tests:

- `exists` returns true and false from known inventories;
- `count` covers zero and every comparator boundary;
- missing, null, non-array, malformed, and duplicate inventories return
  unknown;
- a real empty array returns known false/zero;
- `not`, `and`, and `or` retain three-valued behavior; and
- outcomes propagate through `when`, `skip`, `rules first`, and `rules all`.

Implementation:

- reuse `stream_facts`;
- evaluate eligible audio/subtitle existence;
- count eligible facts and call the existing numeric comparison helper; and
- preserve current Boolean and consumer behavior for unknown facts.

Expected failure before implementation:

- every eligible `Exists` and `Count` still returns unknown.

Verification:

```text
cargo test -p voom-plan condition
cargo test -p voom-plan
cargo test -p voom-control-plane --test published_grammar_corpus
```

Commit:

`feat(plan): evaluate published stream conditions`

## Step 5: Review and ship

Review targets:

- branch diff against `main`;
- ADRs 0036 and 0037;
- #329 design and implementation plan.

Review focus:

- source versus compiled compatibility boundary;
- linked versus unlinked authority and duplicate lineages;
- missing versus empty facts and remux non-regression;
- complete typed eligibility before mutation;
- one-result fresh execution preparation;
- run-start transaction and chained-resume crash safety;
- fail-closed persisted row validation;
- stable error codes and context;
- `run_if` placement isolation; and
- #344, #352, and #353 ownership boundaries.

Verification:

```text
prek run
just ci
```

Shipping:

1. Rebase on current `origin/main`.
2. Re-run focused tests and `just ci`.
3. Push and open a PR closing #329.
4. Wait for hosted checks and verify mergeability.
5. Merge under campaign authorization and clean the branch.
