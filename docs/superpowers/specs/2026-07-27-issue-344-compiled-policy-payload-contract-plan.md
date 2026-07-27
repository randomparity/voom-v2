# Issue #344: Compiled-policy payload contract implementation plan

## Objective

Apply ADR 0013 directly to `policy_versions.compiled_json` using public tagged
enums with distinct strict newtype content structs. Preserve every existing JSON
and DSL contract while accepting and completing the internal Rust source
migration.

## Constraints

- Use one distinct content struct for every one of the 41 variants.
- Preserve enum names, variant names, tags, fields, defaults, and omission rules.
- Add no parser/DSL form, migration, dependency, compatibility shim, or alternate
  payload format.
- Keep content types under `voom_policy::compiled`; do not broaden root exports.
- Keep the existing payload guard implementation unchanged.
- Preserve legacy config, `Skip`, profile, `title_matches`, and fixture reads.
- Every commit must compile and pass its relevant guardrails.

## Step 0 — Commit the approved design

Files:

- `docs/superpowers/specs/2026-07-27-issue-344-compiled-policy-payload-contract-design.md`
- `docs/superpowers/specs/2026-07-27-issue-344-compiled-policy-payload-contract-plan.md`

Verification:

```text
git diff --check
```

Commit:

```text
docs(policy): design compiled payload contract
```

## Step 1 — Record strictness and compatibility failures

Files:

- `scripts/payload-contract-scope.txt`
- `crates/voom-policy/src/compile/compiled_test.rs`
- `crates/voom-policy/src/data/video_profile_test.rs`
- `crates/voom-control-plane/src/cases/policy/plans_test.rs`
- `crates/voom-control-plane/src/cases/policy/compliance_test.rs`

Tests first:

- Add all defining graph files to a temporary scope and run the production guard.
- Add root, ordinary-struct, and exhaustive per-variant unknown-field assertions.
- Add exact current JSON expectations for all 41 variants.
- Add stored planning and mutating compliance failure-order tests.

Expected red evidence:

- The guard reports current inline tagged variants, missing ordinary struct
  attributes, and `CompiledRunIf`'s delegated public adapter.
- Root and nested unknown fields deserialize successfully under current derives.
- The stored planning and compliance calls proceed past the unknown nested field.

Do not commit the red state. Preserve the exact failing commands and reasons in
the implementation checkpoint.

## Step 2 — Perform the compile-atomic public newtype migration

Files:

- `crates/voom-policy/src/compile/compiled.rs`
- `crates/voom-policy/src/compile/lower/conditions.rs`
- `crates/voom-policy/src/compile/lower/operations.rs`
- `crates/voom-policy/src/compile/track_filter.rs`
- `crates/voom-policy/src/compile/validate/operations.rs`
- `crates/voom-policy/src/diagnostic.rs`
- `crates/voom-policy/src/syntax/span.rs`
- `crates/voom-plan/src/eligibility.rs`
- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner/audio/mod.rs`
- `crates/voom-plan/src/planner/audio/selection.rs`
- `crates/voom-plan/src/planner/remux/mod.rs`
- `crates/voom-plan/src/planner/remux/selection.rs`
- `crates/voom-control-plane/src/cases/policy/plans.rs`
- `crates/voom-control-plane/src/transcode/resolve.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`
- `crates/voom-control-plane/tests/published_grammar_corpus.rs`
- `crates/voom-plan/src/eligibility_test.rs`
- `crates/voom-plan/src/planner/audio/selection_test.rs`
- `crates/voom-plan/src/planner/remux/payload_test.rs`
- `crates/voom-plan/src/planner/remux/selection_test.rs`
- `crates/voom-plan/src/planner_test.rs`
- `crates/voom-policy/src/compile/compiled_test.rs`
- `crates/voom-policy/src/compile/pipeline_test.rs`
- `crates/voom-policy/src/compile/track_filter_test.rs`
- strictness and stored failure tests from Step 1
- `crates/voom-policy/src/fixtures/policy_fixtures_test.rs`
- `docs/payload-contract-inventory.md`
- `scripts/payload-contract-scope.txt`

Newly scoped defining files that need no production edit:

- `crates/voom-policy/src/data/video_profile.rs`
- `crates/voom-core/src/media/transcode_video_profile.rs`

Implementation:

- Define 41 distinct public content structs with unchanged fields and serde field
  attributes.
- Derive `Serialize`/`Deserialize` and deny unknown fields on every content
  struct.
- Convert all four enums to newtype variants.
- Update all 575 workspace variant references, preserving exhaustive matching.
- Do not share content structs or add associated compatibility constructors.
- Add deny attributes to every ordinary reachable struct.
- Derive strict `CompiledConfig` and move legacy parsing to field
  deserializers, preserving present-null rejection.
- Add the existing guard exemption to `CompiledRunIf` and keep its strict wire.
- Retain and directly test the audited `VideoProfileRef` visitor.
- Reclassify `compiled_json`, inventory both typed consumers and the full graph,
  and make guard scope match every defining file.
- In the mutating compliance test, first apply a valid report to seed a matching
  issue and event. Snapshot complete, deterministically ordered rows for:
  - `issues`;
  - `events`;
  - `policy_input_sets`;
  - `policy_input_set_fixture_labels`;
  - `policy_input_synthetic_targets`;
  - `policy_media_snapshot_inputs`;
  - `policy_identity_evidence_inputs`;
  - `policy_bundle_target_inputs`;
  - `policy_quality_profile_selections`; and
  - `policy_issue_inputs`.
- Snapshot the raw `policy_versions.compiled_json` text, corrupt it, invoke the
  test-runtime execution entry point, and require contextual
  `PLAN_GENERATION_ERROR`, `partial: None`, identical row projections, identical
  corrupted JSON bytes, and zero `jobs`, `tickets`, and `leases`.

Verification:

```text
cargo fmt --all
cargo test -p voom-policy compiled
cargo test -p voom-policy video_profile
cargo test -p voom-policy policy_fixtures
cargo test -p voom-core transcode_video_profile
cargo test -p voom-plan
cargo test -p voom-control-plane stored_compiled_policy
cargo test -p voom-control-plane compliance_rejects_unknown_compiled
./scripts/check-payload-deny-unknown.sh
./scripts/check-payload-deny-unknown-selftest.sh
cargo clippy -p voom-policy -p voom-plan -p voom-control-plane \
  --all-targets --all-features -- -D warnings
```

Commit:

```text
fix(policy): enforce compiled payload contract
```

## Step 3 — Prove complete current and historical compatibility

Files:

- `docs/release-process.md`

Tests:

- Exact deserialize/serialize equality for every current canonical compiled
  fixture.
- Unchanged published grammar compiled goldens.
- Semantic assertions for legacy config statement strings, `Skip`, legacy bare
  profile strings, tagged/inline profiles, `title_matches`, and config-less
  roots.
- Exact current JSON and unknown-field rejection for all 41 variant vectors.
- Byte-preserving stored-row failure evidence.

Sensitivity:

- Temporarily remove one content struct's deny attribute and confirm its
  unknown-field vector and production guard fail.
- Temporarily swap two same-typed fields in a test expectation and confirm the
  exact-wire vector fails.
- Restore the implementation and rerun green before committing.

Documentation:

- Record compiled policy explicitly under ADR 0013's additive-only,
  reader-before-writer ordering.
- State that rollback after a newer field is written requires the pre-upgrade
  snapshot.
- Do not edit #351's executable health/runbook procedure.

Verification:

```text
cargo test -p voom-policy policy_fixtures
cargo test -p voom-policy legacy_
cargo test -p voom-policy historical_
cargo test -p voom-control-plane --test published_grammar_corpus
just check-payload-deny-unknown
just check-payload-deny-unknown-selftest
just ci
```

Commit:

```text
docs(payload): record compiled-policy rollback contract
```

## Step 4 — Review and ship

- Review the complete branch diff against `4fc6bf7`.
- Run a security-focused hostile-input and rollback review.
- Run simplification review without reintroducing shared content or wire
  indirection.
- Re-run `just ci`.
- Require `git status --short --untracked-files=all` to be empty.
- Push without force, open a factual PR with `Closes #344`, post review and
  trajectory annotations, and drive all checks green and mergeable.
- Do not merge; hand the PR to the campaign orchestrator.
