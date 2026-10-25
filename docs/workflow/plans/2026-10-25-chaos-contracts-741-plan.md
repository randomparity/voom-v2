# Chaos consumer migration plan (#741)

Expected implementation size: 180–300 changed lines (M).
Frozen denominator: 250 changed lines (M), live #741/#740 slice A assessment.
Branch: feat/chaos-contracts-741. Base: main. Guardrails: just ci;
just chaos-e2e-ci; just chaos-e2e-local-script-test; mandatory prek pre-commit.

## Task 1 — pin and schema boundary

Files: third_party/chaos-librarian; crates/voom-cli/tests/support/chaos_librarian.rs;
crates/voom-cli/tests/chaos_librarian_e2e.rs; scripts/chaos-e2e-local.sh;
scripts/test-chaos-e2e-local.sh.

Interfaces: preserve discover/validate_ready/materialize/compare_final_state;
add validate_contract(&self, contract: &str, value: &Value) -> Result<(),
Box<dyn std::error::Error>>. It executes uv run python -c with explicit model
mapping, TypeAdapter.validate_json on an argument, and command_output's existing
loud nonzero handling. Map observed→ObservedState, materialization→
MaterializationReport, replay→ReplayBundle, asset→AssetReport,
scenario→Scenario, divergence→DivergenceReport (confirm actual exported name
before transcription). No runtime model routing or dependency additions.

Verification: Mode: focused-test. Add an equipped case that materializes the
existing static-library scenario, validates its scenario/replay/materialization/
asset reports, then changes schema_version to an unsupported integer and asserts
validation rejects each. Validate the exported observed state too; reject v4 and
future observed versions. Expected red: absent validator or acceptance of an
incompatible version. Green: just chaos-e2e-ci, with these cases included.
Pin synchronization is verified by the existing readiness test and shell harness
selftest; expected red is the old gitlink/guard SHA mismatch. Green:
just chaos-e2e-local-script-test and the heavy readiness case.

Steps: add the focused contract cases first; initialize the candidate checkout
and change the three guard literals together; implement explicit validation and
wire it into the existing materialize/compare helpers. Validate scenario YAML
using upstream ruamel.yaml to load then serialize the document for Scenario.
Read replay.json in the run directory and enumerate reports/assets/*.json;
require a nonempty asset-report set in the static-library regression. Upstream schema-export --check proves models match exported schemas.
Accept when valid emitted artifacts pass and mutated versions fail.

## Task 2 — evidence semantics

Files: crates/voom-cli/tests/support/observed_state.rs;
crates/voom-cli/tests/chaos_librarian_e2e.rs.
Interfaces: existing probed_stream_for_test(&Value) -> Option<Value>;
export_observed_state signature unchanged. Only v6 and the test projection change.

Verification: Mode: focused-test. Add observed_state_v6_dispositions tests for
true/false/missing audio/subtitle values and excluded video flags. Expected red:
audio flags missing from the current projection. Green:
cargo test -p voom-cli --test chaos_librarian_e2e observed_state.
Add an equipped synthetic observed-state round-trip case using the pinned model,
with null/omitted/empty/present-empty format_tags and incompatible versions.
Use compare_probed_media on exported audio/subtitle projections: deliberately
omit flags and assert a named default/forced difference. Expected red: missing
evidence passes the claimed strict comparison. Green: just chaos-e2e-ci.
No metadata coverage is claimed when format_tags is null.

Steps: add intent cases; change schema_version 4 to 6; change subtitle-only
flag projection to matches!(kind, "audio" | "subtitle"), copying only actual
booleans; emit format_tags null because this slice has no persisted extraction.
Confirm existing static and mutation cases remain green. This is a clean
extension of the current harness; no seeder or product contract change.

## Task 3 — assembled verification and handoff

Verification: Mode: task-test-not-applicable. Human design/plan prose has no
executable consumer; do not test wording. Machine contracts are covered above.
Run just fmt, focused cases, shell harness selftest, equipped just chaos-e2e-ci,
then just ci on the assembled candidate. Record commit/tree and elapsed times.
Run independent adversarial review, security pass for the upstream dependency
boundary, and simplification; disposition findings within the frozen surface.
Push through the claim-gated delivery workflow, wait for required CI, publish
exact-head review and MERGE-READY handshake. Do not merge or delete the worktree.

Rollback: revert gitlink and consumer changes together; preserve diagnostic
outputs on a failing guardrail and diagnose the observed cause before edits.
Deferrals: none beyond the frozen excluded owner set.
