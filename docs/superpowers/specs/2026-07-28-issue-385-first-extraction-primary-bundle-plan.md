# First-extraction primary bundle implementation plan

Issue: #385

Base: `main` at `e9438636293e914f952a8f07128026a7b79103d9`

## Step 1 — Centralize primary-bundle assembly

Files:

- `crates/voom-control-plane/src/cases/media/bundles.rs`
- `crates/voom-control-plane/src/cases/media/bundles_test.rs`
- `crates/voom-control-plane/src/scan/persist.rs`
- `crates/voom-control-plane/src/scan/persist_test.rs`

Behavior:

- add an in-transaction exact-active-version resolve-or-create operation;
- emit the four existing identity events only on creation;
- make scan call the shared operation without changing primary-only scan output.

TDD:

- first add tests for create, reuse, inactive version, wrong role, event order,
  and scan behavior;
- confirm the new use-case tests fail because the operation is absent;
- implement the shared operation and remove the scan-local assembly.

Verification:

```text
cargo test -p voom-control-plane cases::media::bundles
cargo test -p voom-control-plane scan::persist
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
```

Commit boundary: `refactor(media): centralize primary bundle assembly`

## Step 2 — Make first bundle and extraction plan atomic

Files:

- `crates/voom-store/src/repo/media/audio_extract_operations.rs`
- `crates/voom-store/src/repo/media/audio_extract_operations_test.rs`
- `crates/voom-control-plane/src/audio/mod.rs`
- `crates/voom-control-plane/src/audio/mod_test.rs`
- `crates/voom-control-plane/src/audio/workflow.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`

Behavior:

- expose validated exact-replay/create planning inside a caller transaction;
- for membership-absent workflow sources, create/reuse the primary bundle and
  create/exact-replay the extraction plan under one immediate transaction;
- preserve the established-bundle legacy-adoption path;
- keep dispatch behind successful transaction commit.

TDD:

- add a failure trigger after bundle assembly and prove no new `media_works`,
  `media_variants`, `asset_bundles`, `asset_bundle_members`,
  `audio_extract_operations`, or `audio_extract_operation_outputs` rows;
- at the workflow boundary, prove no worker request, staging leaf, target file,
  bundle-creation event, or audio-started/audio-failed event; prove the ticket
  attempt remains at its post-acquisition value, the ticket and lease epochs
  advance once into terminal states, and the held lease produces exactly one
  `lease.released` and one terminal ticket-failure event;
- add concurrent first-planning coverage over separate connections and prove
  one bundle, one operation, one output set, and one four-event bundle-creation
  sequence;
- add resume coverage and prove the same bundle, operation, output, and events
  are reused;
- confirm each test fails at the missing-membership boundary before
  implementation.

Verification:

```text
cargo test -p voom-store audio_extract_operations
cargo test -p voom-control-plane audio
cargo test -p voom-control-plane workflow::execution::executor
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Commit boundary: `feat(audio): plan first extraction with its bundle`

## Step 3 — Prove real process boundaries

Files:

- `crates/voom-control-plane/tests/audio_extract_flow.rs`

Behavior:

- remove manual primary-bundle seeding from audio extraction integration;
- resolve the created bundle from durable state and assert its primary and
  extracted members;
- verify the existing shipped-CLI audio corpus, which already has no direct
  store mutation.

TDD:

- first remove the seed and confirm extraction fails before worker dispatch;
- implement Steps 1–2, then prove the real out-of-process integration and CLI
  corpus succeed.

Verification:

```text
cargo test -p voom-control-plane --test audio_extract_flow
cargo test -p voom-cli --test published_grammar_execution_e2e \
  published_grammar_corpus_is_executable -- --exact
```

Commit boundary: `test(audio): cover unbundled process extraction`

## Step 4 — Full review and guardrails

Review the complete diff for transaction ownership, exact event counts,
rollback inspection, concurrency, legacy adoption, payload compatibility, and
unrelated scope. Run simplification review and the repository guardrails.

Verification:

```text
just ci
```
