# Issue #99: Deterministic multi-output audio extraction implementation plan

## Objective

Resolve every published `extract audio` match into a stable ordered descriptor,
execute that ordered descriptor set through the FFmpeg worker, preserve old
single-output data readability, and fail the current host before side effects
until #337 supplies the plural atomic commit unit.

## Constraints

- Add no DSL/parser/compiler form, migration, dependency, or host multi-artifact
  commit/report behavior.
- Preserve legacy singular extraction payload/request/result readability.
- Use unique ascending provider stream indexes as canonical source order.
- Pin the exact domain-separated operation/output identity contract in tests.
- Prove filename uniqueness after final sanitization and ASCII case-folding.
- Keep the current one-output filename and behavior unchanged.
- Keep functions within repository size and complexity limits.

## Step 1 — Resolve stable ordered planner descriptors

Files:

- `crates/voom-plan/src/planner.rs`
- `crates/voom-plan/src/planner/audio/mod.rs`
- `crates/voom-plan/src/planner/audio/payload.rs`
- `crates/voom-plan/src/planner/audio/selection.rs`
- sibling tests in the same modules

Tests first:

- Bare and broad extraction select all matches in ascending provider-index
  order despite shuffled snapshot JSON.
- Zero matches remain blocked; one match remains planned.
- Multiple known-role matches plan one descriptor each.
- Any unknown role or duplicate provider index blocks the whole node.
- Every plural output receives a deterministic fixed-width hash suffix while a
  one-output name remains unchanged.
- A crafted base that would collide with a selectively suffixed name remains
  unique, and a final normalized-name uniqueness check fails closed.
- Repeated plans emit identical operation/output identities and an exact known
  output-ID preimage has the documented value.

Expected red failure:

- Multiple extraction matches currently produce
  `AudioPlanningBlock::MultipleMatches`, and no descriptor or explicit
  operation identity exists.

Implementation:

- Reject duplicate provider indexes while reading audio facts.
- Resolve extraction matches in canonical source order and require every role.
- Add typed plan output descriptors and additive optional payload fields.
- Use the deterministic node ID as `operation_id`.
- Generate exact domain-separated `extract_output_...` IDs.
- Build all plural suffixes after sanitization and case folding, then assert
  final normalized-name uniqueness.

Verification:

```text
cargo test -p voom-plan --all-features extract_audio
cargo clippy -p voom-plan --all-targets --all-features -- -D warnings
```

Commit:

```text
feat(plan): describe deterministic audio outputs
```

## Step 2 — Add plural worker-protocol contracts compatibly

Files:

- `crates/voom-worker-protocol/src/operations/audio.rs`
- `crates/voom-worker-protocol/src/operations/audio_test.rs`
- `crates/voom-worker-protocol/src/lib.rs`
- every workspace `ExtractAudioRequest` / `ExtractAudioResult` struct-literal
  consumer in the FFmpeg worker, control plane, fakes, and conformance fixtures

Tests first:

- Historical singular request/result JSON without lists still deserializes as
  `None`.
- Legacy `None` serialization omits the field, and literal `outputs: null` is
  rejected by the presence-aware field deserializer.
- Literal request/result JSON with `outputs: []` deserializes as `Some([])` and
  is rejected by validation rather than reinterpreted as legacy.
- New one/many request and result lists round-trip in source order.
- Literal JSON assertions pin output ID, complete source reference, output
  settings/path, observed facts, and every singular-to-first projection field.
- Validation rejects an empty explicit list, first-projection disagreement,
  duplicate output IDs, duplicate source IDs, distinct source IDs sharing one
  provider stream index, duplicate normalized paths, and
  reordered/missing/extra result descriptors, including swapped IDs,
  selections, or paths.
- Two distinct descriptor identities/paths with identical output hashes, sizes,
  language, and title are accepted; observed facts are not uniqueness keys.

Expected red failure:

- The protocol models only one `output`, `selection`, and result fact set.

Implementation:

- Add deny-unknown `ExtractAudioOutputDescriptor` and
  `ExtractAudioOutputResult` structs with the exact design fields.
- Add optional ordered lists with a presence-aware serde codec: missing becomes
  `None`, `null` is rejected, an array becomes `Some`, and `None` is omitted.
- Keep singular fields as the invariant-checked first projection.
- Add pure validation helpers shared by worker and control-plane boundaries.
- Update fakes and conformance data without inventing a second operation kind.
- Mechanically initialize all deferred worker/control-plane consumers with
  `outputs: None` in this commit so the workspace remains buildable; Steps 3
  and 4 replace those compatibility initializers with plural behavior.

Verification:

```text
cargo test -p voom-worker-protocol --all-features extract_audio
cargo test -p voom-conformance --all-features
cargo test -p voom-fake-support --all-features
cargo clippy -p voom-worker-protocol --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features
```

Commit:

```text
feat(protocol): carry ordered audio extraction outputs
```

## Step 3 — Execute every descriptor in the FFmpeg worker

Files:

- `crates/voom-ffmpeg-worker/src/handler.rs`
- `crates/voom-ffmpeg-worker/src/handler_test.rs`
- `crates/voom-ffmpeg-worker/src/ffmpeg.rs`
- `crates/voom-ffmpeg-worker/src/ffmpeg_test.rs`

Tests first:

- One descriptor preserves the existing command and result projection.
- Two descriptors execute in request order and return one ordered result each.
- All descriptor paths are validated before the first provider invocation.
- A second-output provider/probe failure returns an operation error rather than
  a shortened success result.
- Duplicate IDs, selections, paths, or inconsistent first projections never
  invoke FFmpeg.
- Distinct snapshot stream IDs that share a provider index fail before FFmpeg;
  the provider index is the actual worker selector and must be unique.

Expected red failure:

- The handler and FFmpeg adapter consume only the singular request fields.

Implementation:

- Normalize legacy/new requests through the protocol validation helper.
- Preflight the complete descriptor set before provider work.
- Reuse the existing one-output FFmpeg execution for each ordered descriptor.
- Aggregate only a complete ordered result and retain the first-output
  projection for compatibility.

Verification:

```text
cargo test -p voom-ffmpeg-worker --all-features extract_audio
cargo clippy -p voom-ffmpeg-worker --all-targets --all-features -- -D warnings
```

Commit:

```text
feat(worker): execute ordered audio extractions
```

## Step 4 — Enforce the current host single-output boundary

Files:

- `crates/voom-control-plane/src/audio/selection.rs`
- `crates/voom-control-plane/src/audio/worker_contract.rs`
- `crates/voom-control-plane/src/audio/mod.rs`
- `crates/voom-control-plane/src/workflow/plan/policy_bridge.rs`
- `crates/voom-control-plane/src/workflow/plan/binding.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/tickets.rs`
- directly affected workflow bridge, binding, executor, and durable-workflow
  sibling tests
- sibling tests in the same modules

Tests first:

- Generate a real plural execution plan, bridge it into a workflow, render and
  persist its root ticket, reload the ticket, and assert byte-for-byte ordered
  preservation of `operation_id` and every output descriptor.
- Retry from that stored ticket and prove binding does not reevaluate the
  selector or regenerate descriptor identities/names.
- Regenerate the same plan through the resume planning path and prove the new
  ticket carries the same operation/output IDs, names, and order.
- A one-descriptor payload selects, dispatches, and validates exactly as before.
- A historical descriptor-less singleton payload remains executable.
- A plural payload fails before staging or target directory creation and before
  worker dispatch.
- Payload descriptors that disagree with the pinned snapshot, canonical order,
  role, ID, or final name fail visibly.
- A worker result with an incomplete/reordered/inconsistent output list is
  malformed and cannot reach the existing commit path.
- Fake-dispatcher results with projection disagreement, extra, missing, or
  reordered descriptors fail before verifier invocation and leave no target
  file, artifact/version row, or extraction success event.

Expected red failure:

- Runtime selection independently reevaluates a selector and rejects multiple
  matches without understanding planned descriptor identity.

Implementation:

- Preserve the complete typed audio operation payload through policy bridge,
  root-ticket binding, persistence, and reload; do not reconstruct descriptors
  from the selector at any workflow layer.
- Parse and validate authoritative descriptors against the re-read snapshot.
- Preserve the legacy one-stream fallback only when descriptors are absent.
- Fail plural operations immediately after snapshot validation and before path
  preparation.
- Build/validate the worker's ordered descriptor list for the singleton path.
- Wire result-list validation immediately after dispatch and before output-file
  observation, staging registration, verifier invocation, or commit.
- Leave all staging, verification, and commit code singular for #337.

Verification:

```text
cargo test -p voom-control-plane --all-features extraction_
cargo test -p voom-control-plane --all-features extract_audio
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
```

Commit:

```text
fix(control-plane): reject plural extract before staging
```

## Step 5 — Documentation, compatibility evidence, and full guardrails

Files:

- `docs/adr/0041-deterministic-multi-output-audio-extraction.md`
- `docs/adr/README.md`
- `docs/specs/voom-control-plane-design.md`
- `docs/superpowers/specs/2026-05-26-voom-sprint-14-design.md`

Tests and checks:

- Update the published planning/worker sections from exactly-one to ordered
  multi-output and retain #337 ownership of atomic commit/reporting.
- Confirm no parser/compiler fixture changed and old compiled policy fixtures
  still load.
- Re-read the complete diff for excluded campaign scope.

Expected red failure:

- Historical documentation still claims multiple matches block.

Verification:

```text
cargo test -p voom-policy --all-features
just ci
```

Commit:

```text
docs(audio): publish multi-output extraction contract
```
