# Node-local location-handle media dispatch — implementation plan

Issue #423 · spec: `docs/workflow/specs/2026-08-23-node-local-media-dispatch-design.md` · ADR 0075

Guardrails: `just ci` (fmt-check, lint, check-test-layout, test, doc, deny,
audit). Coupled ADR index gate: ADR 0075 row already added in this branch.
Migration 0042 is assigned. Conventions: AGENTS.md (deny_unknown_fields on
durable payloads; sibling *_test.rs; BEGIN IMMEDIATE in multi-write fixtures;
no tokio pause + SqlitePool).

## T1 — Protocol envelopes (voom-worker-protocol, voom-core)

Files: `crates/voom-core/src/lib.rs` (PROTOCOL_VERSION 2→3),
`crates/voom-worker-protocol/src/operations/dispatch.rs` (+ `dispatch_test.rs`,
`mod.rs` wiring), re-export surface.
Work per spec C1. Reuse existing fact/selection types from the op modules;
planned-output locators are plain validated strings at this layer.
Verify: `cargo test -p voom-worker-protocol -p voom-core`.

## T2 — Agent handle resolution + observation helpers (voom-node-agent)

Files: `crates/voom-node-agent/src/{media.rs,media_test.rs,runtime.rs,commit.rs,lib.rs}`.
Factor `resolve_rooted_path` (commit.rs) and the observation helper
(`try_observe_regular_file`) into shared internal functions usable by both
commit and media paths. No behavior change yet.
Verify: `cargo test -p voom-node-agent`.

## T3 — Agent media executor (voom-node-agent)

Files: `crates/voom-node-agent/src/media.rs` (+test), `runtime.rs`
(route byte-touching operations from `dispatch_outcome`; keep
`augment_payload` for non-envelope ops).
Work per spec C3 steps 1–6: strict decode → resolve → pre-observe → child
request build (move request-builder logic from deleted CP
`audio/worker_contract.rs`, `remux/dispatch.rs`, `transcode/dispatch.rs` here,
parameterized by resolved paths) → dispatch → post-observe/probe →
`agent_observed` evidence in completion result. Staged probe via
`ChildEndpointRegistry::resolve(ProbeFile)`.
Verify: `cargo test -p voom-node-agent`.

## T4 — Ticket rendering to envelopes (voom-control-plane)

Files: `crates/voom-control-plane/src/workflow/plan/binding.rs`(+test),
`workflow/plan/ticket_payload.rs` (validate derivation against new payload
fields), staging/output root resolution (`LibraryRoot.default_*_root_id`).
Work per spec C2. Deterministic relative locator scheme documented inline.
Verify: `cargo test -p voom-control-plane workflow::plan`.

## T5 — Migration 0042 + durable-payload contract

Files: `migrations/0042_*.sql`, `crates/voom-store/src/migrator.rs`,
`docs/payload-contract-inventory.md`, `scripts/payload-contract-scope.txt` if a
new typed column lands (prefer none).
Fail closed on in-flight non-terminal media workflow tickets (cancel with
recorded reason, mirroring 0038's disposition comment).
Verify: `cargo test -p voom-store`.

## T6 — Control-plane data-only validation + deletions (C4/C5/C7)

Delete CP-side: three `revalidate_source_file`, three
`require_output_file_matches_result`, media canonicalization callers of
`select_local_source` (keep byte-free `select_location`),
`dispatch_control_plane_*` adapters + runtime dispatchers, backup bundled
dispatcher (gate re-minted as ticket per C5), bundled verify dispatchers,
staged-result ffprobe launches, orphaned `worker_process.rs`/
`artifact/worker.rs` parts (verify scan pump sharing first).
Rewrite affected validators data-only (spec C4) and backup/verify flows onto
tickets (C5). Update all sibling tests; e2e moves to agent-driven execution
(spec test strategy) — chaos overrides move agent-side.
Verify: `cargo test -p voom-control-plane -p voom-cli`.

## T7 — Commit-intent source handle (C6)

Files: `crates/voom-control-plane/src/artifact/commit/{intent,prepare,recovery}.rs`,
`crates/voom-node-agent/src/client.rs` (`OpenCommitIntent` fields),
`commit.rs` (applying does source→staging materialization then promote),
`crates/voom-test-support/src/commit_node.rs`,
`crates/voom-artifact` if receipt enums extend (additive only),
`artifact/stage.rs` deletion, orphaned `artifact/fs.rs` helpers.
Extend receipts additively; recovery stays receipt-only.
Verify: `cargo test -p voom-control-plane artifact::commit -p voom-node-agent -p voom-test-support` consumers.

## T8 — Full suite + docs

Runbook touch-ups only where commands change (`docs/runbooks/operator-real-media-execution.md`);
AGENTS.md crate map unchanged (no new crates). `just ci` green.

Ordering: T1 → T2/T4 parallel → T3/T5 → T6 → T7 → T8. T6 depends on T3
(agent must execute before adapters die); T7 is independent of T6 except for
shared fs.rs/stage.rs deletions — land after T6.

## Verification mapping (acceptance criteria)

- Owner-node execution: e2e through in-process AgentRuntime (T6).
- Cross-root isolation: binding-miss tests (T3).
- Mismatch before lease execution: schema/decode rejection tests (T1/T3);
  PROTOCOL_VERSION bump (T1).
- Durable truth under crash/retry/cancel: existing lease/intent suites +
  new executor classification tests (T3/T7).
- Unified contracts: no separate control-plane adapter path remains (T6).
- No control-plane byte work: grep gate in review — no stat/hash/copy/probe
  calls on the named pipeline post-T6/T7 (inspect.rs and
  workflow/coordinator/promotion.rs excluded per charter/#436).
