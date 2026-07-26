# Issue #336: Language-ranked remux defaults implementation plan

## Objective

Execute the published `defaults audio|subtitle best` strategy from the compiled
`config.languages` order and canonical provider stream order. Planning pins one
retained stream ID, execution trusts that ID without reranking, explicit
filter-addressed defaults remain authoritative, and produced media replans as
compliant.

## Constraints

- Add no grammar, compiled-policy field, durable schema, wire field, migration,
  dependency, or compatibility shim.
- Preserve existing compiled policy readability and legacy non-`best` defaults
  composition.
- Use ascending unique provider stream index as source order.
- Validate every retained candidate language only when an effective `best` has
  non-empty preferences.
- Keep functions within repository size and complexity limits.

## Step 1 — Resolve supported `best` actions in the planner

Files:

- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner/remux/mod.rs`
- `crates/voom-plan/src/planner/remux/payload.rs`
- `crates/voom-plan/src/planner_test.rs`

Tests first:

- Replace the old “best blocks” assertion with behavior tests proving a later
  preferred audio stream is pinned.
- Assert source-index tie-breaking despite shuffled snapshot JSON.
- Assert unmatched and empty preference lists select the first retained source
  stream.
- Assert an interleaved duplicate preference uses its first occurrence.
- Assert `defaults subtitle: best` pins its preferred candidate.
- Assert no retained subtitle omits the action without blocking.
- Assert an already-correct resolved selection is `NoOp`.

Expected red failure:

- Unshadowed `best` is emitted as a separate blocked `SetDefaults` node, and no
  selected snapshot ID is present.

Implementation:

- Remove the temporary candidate-support gate and outer explicit-target
  precomputation.
- Pass `CompiledConfig.languages` into grouped remux resolution.
- Rank retained target-kind streams by first language preference position and
  then provider stream index.
- Emit `Best` with `selected_snapshot_stream_id`; omit it for an empty retained
  candidate set.
- Broaden the payload field documentation from explicit filters to all
  planner-resolved defaults.

Verification:

```text
cargo test -p voom-plan --all-features defaults_best
cargo clippy -p voom-plan --all-targets --all-features -- -D warnings
```

Commit:

```text
feat(remux): resolve language-ranked defaults
```

## Step 2 — Enforce ranking fact and reduction boundaries

Files:

- `crates/voom-plan/src/planner/remux/mod.rs`
- `crates/voom-plan/src/planner/remux/selection.rs`
- `crates/voom-plan/src/planner_test.rs`
- `crates/voom-control-plane/src/remux/selection.rs`

Tests first:

- With non-empty preferences, a malformed language on a non-winning retained
  candidate blocks with `insufficient_snapshot_facts`.
- A missing candidate language maps to `und` and emits exactly the existing
  per-file warning.
- Empty preferences choose the first stream without reading malformed or
  missing languages and without emitting that warning.
- An explicit default shadows `best` without reading malformed or missing
  languages or warning, in both source orders.
- `best` plus another same-target strategy blocks in both source orders with an
  actionable diagnostic; multiple `best` actions also block.
- Legacy same-target strategies without `best` retain their existing behavior.

Expected red failure:

- The current resolver never ranks `best`, cannot expose its language-read
  decision, and has no `best`-conflict diagnostic.

Implementation:

- Resolve explicit-target precedence before inspecting strategy facts.
- Reject only effective same-target strategy sets containing `best`; do not
  broaden the rejection to legacy non-`best` combinations.
- Inspect every retained candidate language for a non-empty preference list,
  mapping missing to `und` and rejecting any malformed value.
- Carry whether effective ranking consumed an untagged candidate into warning
  assembly while preserving existing language-filter warnings.
- Add an actionable planner block for conflicting `best` strategies and map it
  to the existing unsupported-media diagnostic code.
- Extend the control plane's downstream exhaustive `RemuxPlanningBlock` match
  with the new planner-only conflict variant. Do not implement payload
  provenance/reduction changes until Step 3.

Verification:

```text
cargo test -p voom-plan --all-features defaults_best
cargo test -p voom-plan --all-features explicit_default
cargo check -p voom-control-plane --all-features
cargo clippy -p voom-plan --all-targets --all-features -- -D warnings
```

Commit:

```text
fix(remux): enforce best-selection boundaries
```

## Step 3 — Validate resolved defaults at the execution boundary

Files:

- `crates/voom-control-plane/src/remux/selection.rs`
- `crates/voom-control-plane/src/remux/selection_test.rs`

Tests first:

- Accept `best` plus a selected ID and set exactly that retained stream default,
  proving execution does not rank snapshot languages.
- Reject unresolved `best`.
- Reject selected IDs on `first` and `none`.
- Preserve explicit-over-`best` authority in both payload orders.
- Reject effective `best` combined with another strategy in both orders.
- Preserve legacy non-`best` multi-strategy execution.

Expected red failure:

- The current reducer classifies every selected ID as explicit, so a resolved
  `best` can incorrectly shadow or conflict with other actions and invalid
  selected-ID shapes are accepted.

Implementation:

- Classify `preserve` plus a selected ID as resolved explicit intent and `best`
  plus a selected ID as resolved ranked intent.
- Fail closed on selected IDs paired with `first` or `none`.
- Repeat planner reduction: explicit wins; multiple explicit actions block;
  without explicit intent, only same-target combinations containing `best`
  block.
- Keep selected stream identity authoritative and reuse existing pinned-snapshot
  and retained-stream checks.

Verification:

```text
cargo test -p voom-control-plane --all-features selection_
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
```

Commit:

```text
fix(remux): validate resolved best defaults
```

## Step 4 — Prove generated-media execution and compliant replanning

Files:

- `crates/voom-control-plane/tests/remux_flow.rs`

Tests first:

- Extend the generated source to English main, later Spanish main, and English
  commentary audio streams.
- Compile a policy with `config.languages: ["spa", "eng"]`, commentary removal,
  Spanish head ordering, `defaults audio: best`, subtitle cleanup, and font-only
  attachment retention.
- Assert the planned default ID equals the explicit order-head ID.
- Inspect the produced snapshot for exact track order, two preserved main audio
  streams, Spanish as the sole audio default, English non-default, no
  commentary, expected subtitle flags, and the one font attachment.
- Replan from the authoritative output snapshot and assert `NoOp`.

Sensitivity check:

- Steps 1–3 make the new end-to-end test capable of passing on its first run, so
  this is an integration acceptance step rather than a pre-implementation unit
  red.
- After the test passes, deliberately mutate the planner's language rank to
  prefer source order over `config.languages`, rerun this test, and confirm the
  exact selected-ID, Spanish-default/order, or `NoOp` assertion fails for the
  intended reason. Restore the implementation and rerun green before commit.

Implementation:

- Change only the deterministic media fixture, published policy text, and
  behavior assertions needed for the acceptance scenario.
- Keep all output checks against normalized probe facts and mkvmerge attachment
  inspection rather than process success alone.

Verification:

```text
cargo test -p voom-control-plane --all-features --test remux_flow -- --nocapture
```

Commit:

```text
test(remux): verify language-ranked media output
```

## Step 5 — Final review and repository guardrails

Review:

- Re-read the complete branch diff for unpublished DSL, schema changes,
  duplicate ranking logic, accidental legacy behavior changes, and functions
  exceeding project limits.
- Run the adversarial review loop, fix every defensible finding, and rerun it
  until approved.
- Run the simplification review and apply only behavior-preserving reductions.

Verification:

```text
just fmt
just lint
just test
just ci
git diff --check
git status --short
```

Commit any review fix as its own conventional, logical change. Do not create an
empty cleanup commit.
