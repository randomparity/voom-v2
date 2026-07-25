---
name: requires-tools-preflight-plan
description: TDD implementation plan for executable metadata.requires_tools semantics.
status: accepted
date: 2026-07-24
issue: 327
base_branch: main
base_commit: 54d5dba
---

# Policy Tool Requirement Preflight Implementation Plan (#327)

Implement
[`2026-07-24-issue-327-requires-tools-preflight-design.md`](2026-07-24-issue-327-requires-tools-preflight-design.md)
in dependency order. Each step starts with behavior tests that fail for the
named reason, then adds the smallest implementation that makes them pass.

## Step 1 — Type and validate the published tool vocabulary

Files and modules:

- `crates/voom-policy/src/compile/compiled.rs` and
  `crates/voom-policy/src/compile/compiled_test.rs`;
- `crates/voom-policy/src/compile/validate.rs` and
  `crates/voom-policy/src/compile/validate_test.rs`;
- `crates/voom-policy/src/diagnostic.rs` and public re-exports in
  `crates/voom-policy/src/lib.rs`;
- affected `voom-plan` warning tests and published-policy fixtures/goldens.

Behavior:

- add the closed `PolicyTool` enum and a typed accessor over the existing
  `metadata.requires_tools` JSON;
- accept legacy canonical JSON strings without changing serialized
  `CompiledPolicy`;
- validate exactly one source setting containing only the three published,
  unquoted identifiers;
- deduplicate list entries in source order;
- replace the deferred warning with actionable validation errors and remove the
  obsolete warning only from newly compiled/in-memory policy data.

Red tests:

- valid declarations currently retain a deferred warning;
- quoted, scalar, unknown, and repeated settings currently compile or lower
  without the required diagnostic;
- malformed stored JSON has no typed failure path.

Verification:

```text
cargo test -p voom-policy
cargo test -p voom-plan metadata_requires_tools
```

Commit boundary: `feat(policy): type published tool requirements`

## Step 2 — Add protocol-v2 identity challenge-response

Files and modules:

- `crates/voom-core/src/lib.rs`;
- new sibling-tested identity wire module under
  `crates/voom-worker-protocol/src/wire/`;
- `crates/voom-worker-protocol/src/transport.rs`;
- `crates/voom-worker-protocol/src/http/client.rs`,
  `crates/voom-worker-protocol/src/http/server.rs`, and their sibling tests;
- protocol public exports and existing fake/conformance `ClientHandle`
  implementations affected by the additive trait method;
- workspace/crate manifests only if an already-workspace-pinned dependency must
  be exposed directly to `voom-worker-protocol`.

Behavior:

- bump the exact-match protocol constant from 1 to 2;
- add deny-unknown identity request/response wire types;
- derive and verify the domain-separated BLAKE3 proof without transmitting the
  worker secret;
- generate every production challenge from the OS-seeded `rand` CSPRNG through
  a deterministic test seam, and compare decoded proof bytes with the existing
  constant-time equality helper;
- require exact protocol, worker ID, and epoch matches;
- bound request plus response collection to ten seconds;
- keep handshake and operation semantics otherwise unchanged.

Red tests:

- the identity route is currently absent;
- a hostile echo endpoint can claim any visible identity because no proof is
  requested;
- two calls have no freshness contract, and a proof captured for one challenge
  has no replay-rejection test against the next challenge;
- a hung endpoint has no identity-specific timeout;
- wrong secret, ID, epoch, and version have no identity verification path.

Verification:

```text
cargo test -p voom-worker-protocol
cargo test -p voom-conformance
cargo test -p voom-fakes
```

Commit boundary: `feat(protocol): authenticate worker identity challenges`

## Step 3 — Make ffprobe dependency readiness a startup invariant

Files and modules:

- `crates/voom-ffprobe-worker/src/ffprobe.rs`,
  `crates/voom-ffprobe-worker/src/ffprobe_test.rs`, and public exports;
- `crates/voom-ffprobe-worker/src/main.rs` and `main_test.rs`;
- `crates/voom-ffprobe-worker/tests/probe_worker.rs`.

Behavior:

- make configuration construction return a typed error when `ffprobe -version`
  cannot start, times out, exits nonzero, or has malformed output;
- construct the checked configuration before binding the HTTP server;
- map failure to `WorkerStartupError::Dependency`;
- remove the `"unknown"` provider-version fallback and any now-unused
  convenience API rather than keeping a compatibility shim.

Red tests:

- missing, timed-out, nonzero, and malformed version helpers currently produce
  a ready worker with provider version `"unknown"`;
- the worker currently prints `BOUND` despite those dependency failures.

Verification:

```text
cargo test -p voom-ffprobe-worker
```

Commit boundary: `fix(ffprobe): fail startup when dependency is unavailable`

## Step 4 — Reserve provider identities and serialize ffprobe bootstrap

Files and modules:

- `crates/voom-control-plane/src/cases/workers/registry.rs` and sibling tests;
- `crates/voom-control-plane/src/local_worker.rs` and sibling tests;
- `crates/voom-control-plane/src/scan/bootstrap.rs` and sibling tests;
- worker repository query helpers only where a transaction-scoped reserved-name
  lookup is required.

Behavior:

- reject supervisor-owned ffmpeg, mkvtoolnix, and ffprobe names on public
  node-less and node-owned registration paths;
- add one internal registration path used by run-local and built-in bootstrap;
- resolve built-in ffprobe under `BEGIN IMMEDIATE`, re-read live reserved rows,
  adopt the sole legacy/live row, or create one unique incarnation;
- fail a denied sole row or multiple live reserved rows with operator context;
- preserve event emission for internally registered workers.

Red tests:

- public registration currently accepts every reserved name;
- a retired exact `builtin.ffprobe` row blocks recovery;
- two concurrent absent-row bootstraps are not required to converge;
- multiple live built-in rows have no deterministic invariant failure.

Verification:

```text
cargo test -p voom-control-plane cases::workers
cargo test -p voom-control-plane scan::bootstrap
cargo test -p voom-control-plane local_worker
```

Commit boundary: `feat(control-plane): reserve concrete provider identities`

## Step 5 — Enforce requirements in prepared execution inputs

Files and modules:

- new sibling-tested policy preflight module under
  `crates/voom-control-plane/src/cases/policy/`;
- `crates/voom-control-plane/src/cases/policy/compliance.rs` and sibling tests;
- `crates/voom-control-plane/src/workflow/coordinator/mod.rs` and sibling tests;
- `crates/voom-control-plane/src/worker_process.rs` and
  `crates/voom-control-plane/src/scan/worker.rs` for a bounded, always-reaped
  bundled ffprobe readiness session;
- focused runtime-registry helpers needed to inspect recorded identity,
  capability, grant, deny, status, and endpoint evidence.

Behavior:

- type requirements before observation and normalize the obsolete warning only
  in memory;
- observe each declared tool as available or unavailable with a stable reason
  and guidance;
- require reserved local identity, effective capability, liveness, and a valid
  identity proof for mutation tools;
- start, identify, shut down, and reap the real bundled ffprobe worker before
  resolving its durable built-in incarnation;
- collect all nonfatal unavailable observations in source order; preserve
  malformed-metadata, database, and reserved-invariant failures;
- compute effective eligibility across all grant rows for an operation, so one
  matching deny defeats an allow during preflight even though lease-time atomic
  enforcement remains #343;
- prepare policy/input/tool inputs once before compliance issue writes or job
  creation, and reuse them for fresh execution and resume.

Red tests:

- requirements currently have no execution effect;
- missing, denied, ungranted, retired, dead, wrong-identity, and wrong-secret
  providers currently do not participate in a requirement preflight;
- split allow/deny grant rows can be mistaken for an effective capability
  unless preflight aggregates the rows before deciding availability;
- mixed failures do not report all per-tool reasons deterministically;
- compliance execute can apply issues before discovering a requirement failure;
- direct and resumed execution do not run a tool preflight;
- ffprobe readiness has no execution-preflight session or cleanup assertion.

Verification:

```text
cargo test -p voom-control-plane tool_preflight
cargo test -p voom-control-plane compliance
cargo test -p voom-control-plane phase_barrier
cargo test -p voom-control-plane resume
```

Commit boundary: `feat(control-plane): preflight policy tool requirements`

## Step 6 — Prove the published corpus and integrated contract

Files and modules:

- published grammar coverage matrix and compiled golden introduced by #326;
- affected policy/planner/control-plane integration tests;
- ADR/spec status and issue-facing documentation touched by the implementation.

Behavior:

- move the `requires_tools` coverage row from deferred to executable;
- retain the schema-version-2 compiled metadata shape;
- assert no parser-only or unpublished spelling was accepted;
- cover built-in verify-artifact as explicitly unrelated to the published tool
  vocabulary;
- re-read the full diff and remove superseded warning/deferred code.

Red tests:

- the current corpus records `requires_tools` as deferred;
- no integrated golden proves typed execution while preserving stored shape.

Verification:

```text
cargo test -p voom-policy
cargo test -p voom-plan
cargo test -p voom-worker-protocol
cargo test -p voom-ffprobe-worker
cargo test -p voom-control-plane
just fmt-check
just lint
just ci
```

Commit boundary: `test(policy): execute published tool requirements`

## Merge guardrails

Before opening the PR, run `prek run` and `just ci`. After rebasing onto the
then-current `main`, rerun focused tests and `just ci`, push the rebased branch,
wait for required GitHub checks, and merge only while green. Do not merge or
fold #343 or #344 into this issue; their authorization and durable-payload
boundaries remain documented in ADR 0034.
