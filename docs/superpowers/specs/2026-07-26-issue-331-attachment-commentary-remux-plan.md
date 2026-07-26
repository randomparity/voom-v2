# Issue #331: Attachment and commentary remux implementation plan

## Inputs

- Base: `694ced7c5a3b520676c924f4da9170542293516f`
- Branch: `feat/remux-attachment-commentary`
- Design:
  `docs/superpowers/specs/2026-07-26-issue-331-attachment-commentary-remux-design.md`
- Decision: `docs/adr/0040-structured-remux-attachment-selection.md`
- Full guardrail: `just ci`

## Step 1 — Record the approved design

### Files

- `docs/adr/0040-structured-remux-attachment-selection.md`
- `docs/adr/README.md`
- design and plan documents under `docs/superpowers/specs/`

### Behavior

Record the exact snapshot facts, font vocabulary, provider mapping, failure behavior, campaign
exclusions, and verification evidence before changing source.

### Verification

```bash
git diff --check
rg -n "attachment|commentary|font|#332|#336" \
  docs/adr/0040-structured-remux-attachment-selection.md \
  docs/superpowers/specs/2026-07-26-issue-331-attachment-commentary-remux-{design,plan}.md
```

### Commit

`docs(remux): design structured attachment selection`

## Step 2 — Normalize and evaluate authoritative selector facts

### Files

- `crates/voom-ffprobe-worker/src/normalize.rs`
- `crates/voom-ffprobe-worker/src/normalize_test.rs`
- `crates/voom-plan/src/planner/remux/selection.rs`
- `crates/voom-plan/src/planner/remux/selection_test.rs`
- `crates/voom-plan/src/planner/remux/mod.rs`
- `crates/voom-plan/src/planner_test.rs`

### Red tests

1. ffprobe attachment tags produce exact `filename` and `mime_type` facts, including folded raw tag
   names.
2. a normalized commentary disposition becomes `Some(true)`/`Some(false)`;
3. missing or malformed commentary blocks only a referenced commentary filter;
4. every official/legacy ADR 0040 font MIME matches exactly;
5. a non-font MIME is false, and a missing/non-string attachment MIME blocks;
6. attachment and commentary keep/remove operations plan as changed or `NoOp` from their facts;
7. a container-only remux with attachments is supported and preserves them conceptually.

Expected initial failures: attachment tags are absent, `SnapshotStreamFact` lacks commentary,
commentary returns `UnsupportedMediaShape`, attachment candidates/source media are rejected, and
the MIME substring test accepts unpublished values.

### Implementation

- add explicit attachment tag projections without modifying raw JSON;
- carry optional commentary through `stream_facts`;
- implement the closed MIME allowlist and fail-closed fact access;
- remove only the attachment/commentary planner rejection gates;
- retain the `TitleMatches` and attachment-order rejections owned by later work.

### Verification

```bash
cargo test -p voom-ffprobe-worker --lib normalize
cargo test -p voom-plan --lib planner::remux
cargo test -p voom-plan --lib attachment
cargo test -p voom-plan --lib commentary
cargo fmt --all -- --check
cargo clippy -p voom-ffprobe-worker -p voom-plan --all-targets --all-features -- -D warnings
```

### Mutation checks

- temporarily replace the commentary unknown error with `false` and prove the missing-fact test
  fails;
- temporarily replace exact font membership with substring matching and prove the unpublished MIME
  rejection test fails;
- restore production code and rerun the focused tests.

### Commit

`feat(remux): evaluate attachment and commentary facts`

## Step 3 — Resolve attachment and commentary actions into the keep set

### Files

- `crates/voom-plan/src/planner/remux/payload.rs`
- `crates/voom-plan/src/planner/remux/payload_test.rs`
- `crates/voom-control-plane/src/remux/selection.rs`
- `crates/voom-control-plane/src/remux/selection_test.rs`

### Red tests

1. typed execution payloads accept attachment keep/remove targets and round-trip the exact filter;
2. malformed action targets and filters still fail with actionable context;
3. attachment actions retain matching attachments and every unselected other-kind stream;
4. commentary actions remove/keep only audio streams whose boolean fact matches;
5. missing commentary facts fail selection closed;
6. removing the only commentary audio triggers the existing final-audio diagnostic.

Expected initial failures: the typed payload and control plane reject attachment targets/source
attachments, and remux filter evaluation rejects commentary.

### Implementation

- remove the typed-payload attachment rejection;
- remove the control-plane attachment-source/action rejections;
- reuse the existing target-scoped keep-set algorithm;
- keep the video re-addition and final-audio guard unchanged.

### Verification

```bash
cargo test -p voom-plan --lib planner::remux::payload
cargo test -p voom-control-plane --all-features --lib remux::selection
cargo fmt --all -- --check
cargo clippy -p voom-plan -p voom-control-plane --all-targets --all-features -- -D warnings
```

### Commit

`feat(control-plane): resolve remux attachment selectors`

## Step 4 — Execute and inspect mkvmerge attachments

### Files

- `crates/voom-mkvtoolnix-worker/src/mkvmerge.rs`
- `crates/voom-mkvtoolnix-worker/src/mkvmerge_test.rs`
- `crates/voom-mkvtoolnix-worker/src/handler.rs`
- `crates/voom-mkvtoolnix-worker/src/handler_test.rs`

### Red tests

1. identify mapping assigns ordinary tracks then top-level attachments to ffprobe provider indexes;
2. selected attachment IDs emit `--attachments`, while an empty selection emits
   `--no-attachments`;
3. attachments never appear in `--track-order` or default/forced flag arguments;
4. missing/duplicate provider references fail before provider execution;
5. output inspection accepts selected filename/size fingerprints and recognized font MIME
   canonicalization;
6. missing, extra, wrong-kind, renamed, resized, or font-to-non-font output attachments fail;
7. changed commentary disposition fails even for a single selected audio track;
8. the worker result includes kept attachment snapshot IDs.

Expected initial failures: top-level attachments are absent from the mapping, attachment references
are rejected, and output inspection counts only ordinary tracks.

### Implementation

- replace the internal track-only model with a source-item mapping;
- parse top-level attachment IDs, names, MIME values, and byte sizes strictly;
- partition ordinary-track and attachment arguments;
- remove the blanket attachment-reference rejection;
- include attachments in expected/output item validation while excluding them from track-only
  options.

### Verification

```bash
cargo test -p voom-mkvtoolnix-worker --lib mkvmerge
cargo test -p voom-mkvtoolnix-worker --lib handler
cargo fmt --all -- --check
cargo clippy -p voom-mkvtoolnix-worker --all-targets --all-features -- -D warnings
```

### Commit

`feat(mkvtoolnix): execute attachment selections`

## Step 5 — Prove generated-media execution and compliant replanning

### Files

- `crates/voom-control-plane/tests/remux_flow.rs`
- directly affected runbook/spec text if an existing statement still calls the behavior unsupported

### Red test

Extend the real fixture and policy with:

- main and commentary audio;
- retained subtitle dispositions;
- font and non-font attachments;
- commentary removal and exact font attachment selection.

Assert source facts, produced attachment inventory, commentary absence, retained dispositions,
kept snapshot IDs, and `NoOp` replanning.

Expected initial failure before Steps 2–4: planner/control-plane attachment rejection. If written
after those steps, temporarily run against the pre-implementation base or revert one required
worker branch locally to demonstrate the test's failure before restoring the implementation.

Add a focused control-plane test for the only-audio-is-commentary rejection if Step 3's unit test
does not already exercise the complete runtime selector.

### Implementation

Generate the ordinary streams with ffmpeg, then attach a font and a non-font file with mkvmerge.
Use only published policy forms. Inspect the authoritative result snapshot and mkvmerge identify
JSON; do not infer success from a worker status alone.

### Verification

```bash
cargo test -p voom-control-plane --all-features --test remux_flow -- --nocapture
cargo test -p voom-control-plane --all-features remux
cargo test -p voom-mkvtoolnix-worker --all-features
just fmt-check
just lint
```

### Commit

`test(remux): prove attachment and commentary execution`

## Step 6 — Integrated verification and cleanup

### Review

1. Re-read every changed function for the 100-line and complexity limits.
2. Search for stale unsupported-attachment/commentary comments and assertions.
3. Confirm no #332/#336 behavior or unpublished DSL form entered the diff.
4. Run the adversarial review loop, security-focused review of provider JSON/argument handling, and
   three-pass simplification review.

### Verification

```bash
rg -n "unsupported attachment|commentary.*unsupported|contains\\(\"font\"\\)" \
  crates docs
git diff --check
prek run
just ci
git status --short --branch
```

Every warning and failure is blocking. The normal explicitly ignored opt-in chaos/toxiproxy tests
must be reported; no unexpected skip may be described as a full pass.

### Commit

Only if review or verification requires a behavior-preserving correction:

`refactor(remux): simplify attachment selection`
