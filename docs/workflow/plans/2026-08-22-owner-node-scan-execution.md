# Plan — owner-node scan execution (#421)

Goal: move scan discovery/hash/probe to owner-node bundled workers feeding durable scan
sessions; remove all control-plane-local byte access and direct path dispatch.

Architecture in three lines: `request_scan_run` mints session+ticket; the node-agent
scan-session pump pipelines ScanLibrary→HashFile→ProbeFile across declared child workers and
submits evidence-bearing observation batches; completion publishes identity from agreed
evidence then retires absent locations.

Tech stack: Rust workspace (tokio, sqlx/SQLite, axum, serde strict JSON), `just ci`
guardrails (fmt-check, lint, check-test-layout, check-paused-time-db+selftest,
test --all-features, doc, deny, audit) plus gated `scripts/check-adr-index.sh`.

## Global Constraints

- Sibling-test layout: unit tests in `<source>_test.rs` wired via `#[cfg(test)] #[path]`;
  no inline test mods (`just check-test-layout`).
- Durable payloads: `#[serde(deny_unknown_fields)]` on Deserialize structs; additive-only
  evolution; new typed roots registered in `docs/payload-contract-inventory.md` AND
  `scripts/payload-contract-scope.txt`.
- Public error codes are frozen (`voom_core::VoomError::code()`); add variants, never rename.
- Workspace-inherited versions; internal deps via `[workspace.dependencies]`.
- Never pair `tokio::time::pause()` with `SqlitePool`; drive domain time via injected Clock.
- Migration numbering: file logical number 0041 (0038–0040 reserved by parallel work);
  registered as next physical version in `crates/voom-store/src/migrator.rs` MIGRATOR +
  `expected_migrations()`.
- ADR index: exactly one row for 0077 in `docs/adr/README.md`, Status keyword matching the
  record (`Accepted`); touch no other row.
- Parallel work: sibling #422 owns `voom-node-agent/src/{client,runtime}.rs` bodies and
  voom-api commit-intent routes — keep edits there minimal (one wiring arm; client methods
  in a separate file).
- Clippy pedantic, panic/unwrap/expect denied; zero warnings.

## Task 1 — Evidence field end-to-end (migration 0041)

Files:
- create `migrations/0041_scan_observation_evidence.sql`
- edit `crates/voom-store/src/migrator.rs` (register physical version 3)
- edit `crates/voom-store/src/repo/scan/sessions.rs` (+`sessions_test.rs`): add
  `evidence: Option<ScanObservationEvidence>` to `ScanObservation`,
  `NewScanObservationBatch` persistence (write/read `evidence_json`)
- create `crates/voom-core/src/taxonomy/scan_evidence.rs` (+ sibling test): types per spec C1

Interfaces consumed later: `ScanObservation.evidence`,
`ScanObservationEvidence { content_hash, size_bytes, modified_at, file_key, sidecars,
probe_snapshot }`, `FileKeyFacts { dev, ino, nlink }`, `ScanSidecarEvidence`.

Steps: failing store test (round-trip batch with/without evidence) → migration + struct →
pass → register inventory rows → `cargo test -p voom-store -p voom-core scan`.

## Task 2 — Worker protocol contracts

Files: create `crates/voom-worker-protocol/src/operations/scan_library.rs`,
`operations/hash_file.rs`; edit `operations/mod.rs` registry; re-export from `lib.rs`.
Shapes per spec C2, all `deny_unknown_fields`. Unit tests: round-trip decode rejects unknown
fields; progress payload bound ≤256 candidates enforced by decode helper.
Verify: `cargo test -p voom-worker-protocol`.

## Task 3 — voom-scan-worker crate

Files: create crate `crates/voom-scan-worker` (workspace member, inherited version fields):
`main.rs` (ffprobe-worker pattern), `handler.rs`, `discover.rs` (pure logic moved from
`voom-control-plane/src/scan/discovery.rs`: extension tables, sidecar classification,
longest-stem matching — moved verbatim where possible), `walk.rs` (+ tests each).
Behavior: canonicalize root → walk metadata-only → skip symlinks (counted) → escape guard on
joined paths → emit candidate frames (≤256) → Result summary. Locator validation via
`ProviderRelativeLocator::new` before emission (malformed ⇒ skipped+counted, never emitted).
Leading-dash filenames pass through untouched.
Tests: ordering, symlink skip, out-of-root rejection, allowlist, empty root summary,
non-UTF-8 filename handling, frame batching bound.
Verify: `cargo test -p voom-scan-worker`.

## Task 4 — voom-hash-worker crate

Files: create crate `crates/voom-hash-worker`: `main.rs`, `handler.rs`, `hash.rs` (+tests).
Behavior: component-wise O_NOFOLLOW descent from canonical root to the relative locator;
BLAKE3 stream (8 KiB chunks); stat before/after (size, mtime, dev/ino/nlink); mismatch ⇒
terminal Error frame `FailureClass::ContentDrift`; sidecars hashed SHA-256 with same descent;
timestamps recorded as stability_started_at (pre-stat) / stability_confirmed_at (post-stat).
Tests: happy path hash matches blake3 of fixture; drift between stats fails closed; symlink
component rejected; hardlink reports shared dev/ino; missing file error class.
Verify: `cargo test -p voom-hash-worker`.

## Task 5 — CP request_scan_run

Files: create `crates/voom-control-plane/src/scan/run.rs` (+test); wire into
`scan/mod.rs` exports and `lib.rs`.
Behavior per spec C5: availability fail-close (reuse `RootBlockReason::from_availability`),
session insert + ticket creation + ready-marking in one transaction; payload encode via
`WorkflowTicketPayload` with byte-touching declaration; returns ids.
Tests: happy path creates requested session + ready ticket carrying session id and root-read
declaration; blocked root creates nothing; duplicate active session conflicts without ticket.

## Task 6 — Agent pump + client methods

Files: create `crates/voom-node-agent/src/scan_client.rs` (inherent impl on
`ControlPlaneClient`: start/batch/complete/fail over existing `send` transport),
`src/scan_session.rs` (pump per spec C4) + tests; edit `src/runtime.rs` **only** to route
`scan_library` dispatches to the pump; edit `src/main.rs` module decls if needed.
Batch idempotency key format: `{incarnation_id}-scan-{session_id}-{sequence}`.
Tests (mock CP server via local axum or existing test doubles used by runtime tests):
ordered batches, retry replays accepted sequence, drift candidate yields evidence-less
observation, fatal worker crash ⇒ fail_scan_session + lease Fail, empty enumeration ⇒
complete with null last_sequence, >1000 candidates split flushes.
Verify: `cargo test -p voom-node-agent`.

## Task 7 — Completion publication

Files: edit `crates/voom-control-plane/src/scan/sessions.rs` (`complete_scan_session`) to
invoke new `crates/voom-control-plane/src/scan/publish.rs` inside the completion tx before
retirement; relocate DB-only logic from `scan/persist.rs` (same-address replay, hardlink
attach, ingest + events, snapshot record, sidecar bundles, inode facts); delete byte-reading
parts. Tests (store-level + case-level): evidence published once per locator; second session
re-publishing same content hits same-address replay; hardlink attach; no evidence ⇒ no
publication but location protected from retirement; commit-lock conflict path unchanged.
Verify: `cargo test -p voom-control-plane scan`.

## Task 8 — CLI rewiring + removal sweep

Files: rewrite `crates/voom-cli/src/commands/media/scan.rs` (+snapshot updates) to call
`request_scan_run` then poll `cp.scan_session` until terminal (bounded by deadline;
`--no-wait` flag exits after request); delete old pipeline modules and their tests
(`discovery.rs`, `hash.rs`, `mod.rs` pipeline sections, `persist.rs` byte paths, old
`library.rs` checks). KEEP `worker.rs` and `bootstrap.rs`: audio/remux/transcode commit
probing, policy tool preflight, and artifact verification still consume them (#423/#424
surfaces); keep `local_node_id` (transform/commit consumers remain, owned by #423+); update
any insta snapshots.
Grep gate: no references to removed symbols remain in workspace.
Verify: `cargo build --workspace && just fmt && just lint && just check-test-layout`.

## Task 9 — Docs + debt close

Files: add README row `| [0077](0077-owner-node-scan-execution.md) | Owner-node scan
execution |` to `docs/adr/README.md`; set `docs/debt/0004` Status to Delivered with pointer
to ADR 0077 (record format rules respected); note agent-config worker declarations in the
operator runbook section touched by this change only if a runbook already documents scan
setup (check `docs/runbooks/`).
Verify: `bash scripts/check-adr-index.sh` passes; full `just ci` green; push branch.
