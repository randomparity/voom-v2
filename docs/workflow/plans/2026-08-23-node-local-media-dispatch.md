# Node-local location-handle media dispatch — implementation plan

Issue #423 · spec: `docs/workflow/specs/2026-08-23-node-local-media-dispatch-design.md` · ADR 0075

Guardrails: `just ci` (fmt-check, lint, check-test-layout, test, doc, deny,
audit). Coupled ADR index gate: ADR 0075 row already added in this branch.
Migration 0042 is assigned. Conventions: AGENTS.md (deny_unknown_fields on
durable payloads; sibling *_test.rs; BEGIN IMMEDIATE in multi-write fixtures;
no tokio pause + SqlitePool).

Ordering note: the control-plane switchover (renderers stop emitting paths,
adapters die) must land as one atomic task per operation family — no window
may exist where renderers emit envelopes while path-parsing adapters still
run. T4 therefore only adds resolution plumbing and tests; emission flips
inside T6.

## T1 — Protocol envelopes (voom-worker-protocol, voom-core)

Files: `crates/voom-core/src/lib.rs` (PROTOCOL_VERSION 2→3, done),
`crates/voom-worker-protocol/src/operations/dispatch.rs` (+ `dispatch_test.rs`,
`mod.rs` wiring), re-export surface, version-pin test updates. Done in
commit 3c500438 except any review-driven adjustments (StageSource variant was
removed by decision: fenced intent owns that copy).

## T2 — Agent handle resolution + observation helpers (voom-node-agent)

Files: `crates/voom-node-agent/src/{media.rs,media_test.rs,runtime.rs,commit.rs,lib.rs}`.
Factor `resolve_rooted_path` (commit.rs) and the observation helper
(`try_observe_regular_file`) into shared internal functions usable by both
commit and media paths. No behavior change yet.
Verify: `cargo test -p voom-node-agent`.

## T3 — Agent media executor (voom-node-agent)

Files: `crates/voom-node-agent/src/media.rs` (+test), `runtime.rs`
(route byte-touching operations from `dispatch_outcome`; decode raw payload
pre-augment per spec C3 step 1).
Work per spec C3: strict decode → resolve → pre-observe → stale-residue
clear → child request build (move request-builder logic from soon-deleted CP
`audio/worker_contract.rs`, `remux/dispatch.rs`, `transcode/dispatch.rs`
here, parameterized by resolved paths) → dispatch → post-observe/probe →
`agent_observed` evidence in completion result. Staged probe via
`ChildEndpointRegistry::resolve(ProbeFile)`.
Verify: `cargo test -p voom-node-agent`.

## T4 — Renderer plumbing, no emission flip yet (voom-control-plane)

Files: `crates/voom-control-plane/src/workflow/plan/binding.rs`(+
`binding_test.rs` if present), handle-resolution helpers for staging/output/
backup roots (`LibraryRoot.default_*_root_id`; unset → render error).
Additive only: new render functions exist and are unit-tested; nothing calls
them from production paths yet.
Verify: `cargo test -p voom-control-plane workflow::plan`.

## T5 — Migration 0042

Files: `migrations/0042_*.sql`, `crates/voom-store/src/migrator.rs`.
Preflight guard mirroring migration 0038's abort semantics exactly: the
migration aborts (transaction fails, data untouched) when in-flight
non-terminal media workflow tickets exist. No row mutation.
Verify: `cargo test -p voom-store`.

## T6 — Atomic switchover + deletions (C2 flip, C4, C5, C7)

One task because renderers and consumers must flip together:

- Flip renderers to emit nested `media_dispatch` (scalar keys preserved).
- Delete CP-side: three `revalidate_source_file`, three
  `require_output_file_matches_result`, media canonicalization callers of
  `select_local_source` (keep byte-free `select_location`),
  `dispatch_control_plane_*` adapters + runtime dispatchers, backup bundled
  dispatcher (gate re-minted as ticket per C5), bundled verify dispatchers,
  staged-result ffprobe launches (audio/remux/transcode commit.rs probe
  helpers), orphaned `worker_process.rs`/`artifact/worker.rs` parts. The
  `scan/worker.rs` ffprobe launcher survives (policy tool_preflight, #424).
- Rewrite affected validators data-only (spec C4); backup/verify flows onto
  tickets (C5). Update all sibling tests.

Known cross-crate callers to migrate with the stage_copy/staging deletion
(T7 shares some): CLI command `artifact.stage_copy`
(`crates/voom-cli/src/commands/media/artifact.rs:249-330`) and its snapshots;
voom-api seeding helper (`crates/voom-api/src/commit_test.rs:202`);
integration suites seeding via `cp.stage_copy` directly
(`tests/staged_artifact_flow.rs`, `tests/commit_use_lease_gate.rs`,
`tests/recover_commit_gate.rs`, `artifact/inspect_test.rs`,
`artifact/verify_test.rs`, `artifact/commit/mod_test.rs`). These move to
fenced-intent-driven staging or direct DB seeding helpers — enumerate via
grep before deleting anything.
Verify: `cargo test -p voom-control-plane -p voom-api`.

## T7 — Commit-intent source handle (C6)

Files: `crates/voom-control-plane/src/artifact/commit/{intent,prepare,recovery}.rs`,
`crates/voom-node-agent/src/client.rs` (`OpenCommitIntent` fields),
`commit.rs` (applying does source→staging materialization then promote),
`crates/voom-test-support/src/commit_node.rs`,
`crates/voom-artifact` if receipt enums extend (additive only),
`artifact/stage.rs` deletion + its cross-crate callers from T6's list,
orphaned `artifact/fs.rs` helpers.
Extend receipts additively; recovery stays receipt-only.
Verify: `cargo test -p voom-control-plane -p voom-node-agent -p voom-test-support -p voom-cli`.

## T8 — E2E migration to agent-driven execution

Files: `crates/voom-cli/tests/operator_execution_e2e.rs` (+support),
chaos override relocation to the agent-side executor, any remaining
workflow-level tests relying on control-plane adapters.
Drive media ops through an in-process `AgentRuntime` (lifecycle pattern).
Verify: `cargo test -p voom-cli --test operator_execution_e2e` then full
suite.

## T9 — Full suite + docs

Runbook touch-ups only where commands change
(`docs/runbooks/operator-real-media-execution.md`); AGENTS.md crate map
unchanged (no new crates). `just ci` green.

Ordering: T1 → T2 ∥ T4 ∥ T5 → T3 → T6 → T7 → T8 → T9.

## Verification mapping (acceptance criteria)

- Owner-node execution: e2e through in-process AgentRuntime (T8).
- Cross-root isolation: binding-miss tests (T3).
- Mismatch before lease execution: schema/decode rejection tests (T1/T3);
  PROTOCOL_VERSION bump (T1).
- Durable truth under crash/retry/cancel: existing lease/intent suites +
  new executor classification tests (T3/T7).
- Unified contracts: no separate control-plane adapter path remains (T6).
- No control-plane byte work: grep gate in review — no stat/hash/copy/probe
  calls on the named pipeline post-T6/T7 (inspect.rs, promotion.rs, and
  policy tool_preflight excluded per charter/#436/#424).
