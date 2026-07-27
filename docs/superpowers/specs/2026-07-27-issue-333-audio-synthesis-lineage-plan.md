# Issue #333 implementation plan

Base: `main` at `c508fd05aef39dffac848213133369f6aba2c611`.

Design:
[`2026-07-27-issue-333-audio-synthesis-lineage-design.md`](2026-07-27-issue-333-audio-synthesis-lineage-design.md).

## Step 1 — Publish stable synthesis descriptors

Files:

- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner/audio/{mod,payload,selection}.rs`
- sibling planner tests

Behavior:

- assign stable synthesis operation IDs;
- emit one ordered stable companion descriptor per selected source stream;
- require descriptors only for synthesis and preserve existing audio payloads.

Red tests:

- one/many matches expose stable source-order descriptors;
- regenerating a plan preserves operation and companion IDs;
- missing, duplicate, or malformed descriptors fail typed decoding;
- existing compiled synthesis policy versions still decode and replan.

Verification:

- `cargo test -p voom-plan synthesize`
- `cargo clippy -p voom-plan --all-targets --all-features -- -D warnings`

Commit: `feat(plan): publish stable audio synthesis companions`

## Step 2 — Add synthesis operation and lineage persistence

Files:

- `migrations/0025_recoverable_audio_synthesis.sql`
- `crates/voom-store/src/migrator.rs`
- `crates/voom-store/src/repo/media/audio_synthesis_operations.rs`
- sibling repository and migration inventory tests
- payload contract inventory if typed JSON is persisted

Behavior:

- persist exact operation, ordered companion, dispatch, claim, staged artifact,
  result, and lineage evidence while reusing the generic commit ledger;
- enforce semantic/path/result-stream/lineage uniqueness;
- provide claimed state transitions and exact committed replay;
- atomically finalize all companion lineage rows with operation state.

Red tests:

- duplicate or drifted replay fails;
- stale claim/generation cannot mutate;
- incomplete companion finalization rolls back;
- exact committed replay returns identical rows.

Verification:

- `cargo test -p voom-store audio_synthesis`
- `cargo test -p voom-store migration_inventory`
- `cargo clippy -p voom-store --all-targets --all-features -- -D warnings`

Commit: `feat(store): persist recoverable audio synthesis lineage`

## Step 3 — Wire selection, binding, request, and strict validation

Files:

- `crates/voom-control-plane/src/workflow/plan/binding.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/tickets.rs`
- `crates/voom-control-plane/src/audio/{selection,worker_contract,workflow}.rs`
- sibling tests

Behavior:

- allow published synthesis payloads through the transcode operation route;
- recompute and validate ordered descriptors against the pinned snapshot;
- send derived result IDs with source provider indexes;
- set add-track mode and target channels;
- validate complete worker and probe facts, including source preservation;
- bind stable companion IDs to their unique validated result provider indexes
  in the normalized precommit snapshot.

Red tests:

- binding accepts synthesis but rejects mismatched audio types;
- request contains add-track, target channels, and ordered derived IDs;
- malformed/partial/reordered companions and changed metadata fail before
  staging;
- duplicate, ambiguous, or occupied result provider-index bindings fail before
  publication.

Verification:

- `cargo test -p voom-control-plane synthesize`
- `cargo test -p voom-control-plane policy_transcode_audio`
- strict control-plane Clippy

Commit: `feat(audio): dispatch and validate synthesized companions`

## Step 4 — Implement staged publication, retry, and recovery

Files:

- `crates/voom-control-plane/src/audio/{mod,commit,stage,dispatch,workflow}.rs`
- `crates/voom-store/src/repo/media/audio_synthesis_operations.rs`
- sibling failure-injection tests

Behavior:

- resolve/create and claim the semantic operation;
- persist each dispatch generation before send;
- bind only a complete exact result from the current generation;
- verify and preprobe before prepare;
- add-only promote one result artifact;
- finalize result file/snapshot and all lineage in one transaction;
- resume staged operations through generic artifact recovery and replay
  committed operations without duplicates.

Red tests:

- retry before/after worker response preserves operation/companion IDs;
- late old generation cannot bind;
- crashes after stage, prepare, target install, and during finalize recover;
- every replay preserves file/artifact/commit/snapshot/lineage counts.

Verification:

- focused synthesis state/recovery tests in control-plane and store;
- `cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`

Commit: `feat(audio): publish synthesis lineage atomically`

## Step 5 — Report ordered companion relationships

Files:

- `crates/voom-control-plane/src/audio/{mod,events}.rs`
- `crates/voom-events/src/payload/artifact.rs`
- `crates/voom-control-plane/src/cases/policy/compliance.rs`
- payload inventory/scope files
- CLI/control-plane snapshots and sibling tests

Behavior:

- expose operation identity and ordered companion relationships in synthesis
  execution reports and audio events;
- collect strict ordered synthesis results in compliance execute/job reports;
- omit synthesis-only fields for ordinary replacement transcodes.

Red tests:

- reports expose source/result stream IDs, file/snapshot identities, facts, and
  lineage IDs in order;
- malformed durable synthesis result fails loud;
- historical transcode result/report shape remains readable.

Verification:

- focused compliance, event, workflow-result, and CLI tests;
- payload guards and strict Clippy

Commit: `feat(report): expose audio synthesis lineage`

## Step 6 — Generated-media evidence and full verification

Files:

- new or extended `crates/voom-control-plane/tests/audio_synthesis_flow.rs`
- generated-media helpers/fixtures only where required

Behavior:

- execute the published surround-to-stereo policy through real ffmpeg/ffprobe;
- prove original surround stream preservation and one companion per selected
  source;
- inspect committed snapshot facts and durable lineage;
- exercise one/many matches, malformed/partial output, retries, and crash
  boundaries through focused doubles plus real-media happy paths.

Expected pre-implementation failure:

- the current control plane rejects the published synthesis payload before
  dispatch and therefore cannot produce or query companion lineage.

Verification:

- `cargo test -p voom-control-plane --test audio_synthesis_flow -- --nocapture`
- all focused synthesis tests
- `just ci`

Commit: `test(audio): verify synthesized media and durable lineage`

## Review and shipping

1. Transition #333 to `status:in-review`.
2. Run the adversarial review loop over `origin/main...HEAD`; fix all material
   findings and re-review.
3. Run simplification review; apply only behavior-preserving reductions.
4. Rebase on current `origin/main`, rerun focused tests and `just ci`.
5. Push and open a PR containing `Closes #333`.
6. Post `WORK:REVIEW`, wait for hosted audit/coverage/Linux/macOS CI, and
   transition to `status:awaiting-merge` only when GitHub reports clean and
   mergeable.
