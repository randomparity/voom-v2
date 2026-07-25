# Issue 329 implementation plan: Evaluate published stream conditions

Base branch: `main`

Base commit: `856bacb8ff42954fede8830007145d0674d937bb`

Guardrails:

- `cargo test -p voom-policy condition`
- `cargo test -p voom-plan condition`
- `cargo test -p voom-control-plane stream_summary`
- `cargo test -p voom-control-plane linked_stream`
- `cargo test -p voom-control-plane --test published_grammar_corpus`
- `prek run`
- `just ci`

## Step 1: Preserve and rehydrate authoritative stream facts

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
- `crates/voom-control-plane/src/workflow/coordinator/mod.rs`
- `crates/voom-control-plane/src/workflow/coordinator/planning.rs`
- `crates/voom-control-plane/src/workflow/coordinator/resume.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`

Red tests:

- missing streams stay missing through both snapshot projections;
- null and non-array streams are copied rather than normalized;
- arrays retain exact members and derive the video count;
- missing and non-array streams retain the legacy zero-video sentinel;
- container-only/remux behavior is unchanged across missing, null, non-array,
  malformed-array, and empty-array inventories;
- stored plan, report, fresh execution, and resume replace linked cached facts
  from the same active chain-tip snapshot;
- the active version and latest snapshot are returned by one repository
  statement, and the snapshot belongs to that version;
- the repository selects greatest live version id and greatest snapshot id,
  with no fallback from malformed newest facts;
- post-commit phases use the produced version's refreshed snapshot;
- missing, non-file-version, and mismatched links fail every stored read path
  with the same error class and identifier context;
- repository failures propagate rather than becoming provenance errors;
- unlinked stored inputs keep their summary; and
- store-free planning keeps its supplied summary.

Implementation:

- make the shared stream-summary projection preserve source shape;
- route both production snapshot projections through it;
- add one identity-repository read that returns an active chain tip and its
  latest snapshot as a coherent per-file pair;
- add an async stored-input adapter that validates original link provenance,
  resolves active chain-tip snapshots, and projects complete current facts;
- use the adapter from stored plan, report, and coordinator entry points.

Expected failure before implementation:

- missing streams are currently normalized to `[]`;
- stored planning currently trusts the cached summary.

Verification:

```text
cargo test -p voom-control-plane stream_summary
cargo test -p voom-control-plane linked_stream
```

Commit:

`fix(policy): use authoritative linked stream facts`

## Step 2: Close the newly executable condition boundary

Files:

- `crates/voom-policy/src/compile/validate/conditions.rs`
- `crates/voom-policy/src/compile/validate_test.rs`
- `crates/voom-policy/src/compile/pipeline_test.rs`
- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner_test.rs`

Red tests:

- canonical audio/subtitle `exists` and `count` source forms compile;
- all six count comparators compile;
- filtered exists, video/attachment targets, non-numeric comparators, extra
  tokens, signs, non-ASCII digits, and overflowing counts fail compilation;
- canonical stored compiled shapes remain deserializable;
- unpublished compiled stream shapes fail plan generation;
- one unpublished stream leaf invalidates its complete Boolean tree; and
- an invalid leaf in a later phase fails full-policy validation before an
  earlier phase can open a job or dispatch;
- eligibility diagnostics use `invalid_planning_request`, a stable prefix, and
  deterministic phase/placement/operation/rule/Boolean path context;
- `generate_plan`, `plan_phase`, and stored preparation preserve the same
  eligibility diagnostic message;
- stream conditions in `run_if` fail plan generation while canonical predicates
  retain their existing unknown behavior.

Implementation:

- narrow source validation only for `Exists` and `Count`;
- traverse all phase and operation condition placements before planning;
- accept published stream shapes only on ordinary condition surfaces; and
- reject stream conditions in `run_if` without changing other compiled
  condition behavior;
- collect eligibility failures in deterministic policy traversal order with
  the existing `InvalidPlanningRequest` diagnostic code;
- call the same pure full-policy validation from `generate_plan`, `plan_phase`,
  and stored coordinator preparation before profile resolution or job creation.

Expected failure before implementation:

- source validation currently accepts broader stream shapes;
- the shared evaluator has no placement preflight.

Verification:

```text
cargo test -p voom-policy condition
cargo test -p voom-plan condition
```

Commit:

`fix(policy): enforce published stream condition shapes`

## Step 3: Evaluate published stream conditions

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
- preserve the current Boolean and consumer behavior for unknown facts.

Expected failure before implementation:

- every `Exists` and `Count` currently returns unknown.

Verification:

```text
cargo test -p voom-plan condition
cargo test -p voom-plan
cargo test -p voom-control-plane --test published_grammar_corpus
```

Commit:

`feat(plan): evaluate published stream conditions`

## Step 4: Review and ship

Review targets:

- branch diff against `main`;
- ADR 0036;
- #329 design and implementation plan.

Review focus:

- linked versus unlinked authority;
- original link provenance and active chain-tip authority;
- coherent per-file version/snapshot reads without claiming input-set-wide or
  plan-through-dispatch isolation (#352);
- read-side provenance validation without absorbing generic write hardening
  (#353);
- missing versus empty facts;
- remux non-regression for unavailable inventories;
- accidental activation of filtered/video/attachment conditions;
- Boolean short-circuiting around unpublished stream leaves;
- full-policy validation before phase mutation;
- stable provenance failure codes and context;
- `run_if` placement isolation; and
- existing compiled-version readability.

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
