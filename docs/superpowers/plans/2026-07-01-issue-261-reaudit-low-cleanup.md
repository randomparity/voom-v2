# Plan: Re-audit low-severity cleanup (issue #261)

Derived from `docs/specs/reaudit-low-severity-cleanup-261.md`.
Date: 2026-07-01. Branch: `feat/reaudit-low-cleanup-261`.

## Execution mode

Direct implementation in one session, one feature branch, per-item TDD. The
five tasks are independent but small and share one expensive workspace build, so
sequential direct work is more efficient than subagent fan-out and equally
rigorous. Order is chosen to group by crate and land the most self-contained
items first.

## Guardrail commands (run before each commit)

- Focused test: `cargo test -p <crate> <test_name>`
- Per-crate: `cargo test -p <crate>`
- Lint (hard-gated in CI, individually): `just lint`
  (`cargo clippy --workspace --all-targets --all-features -- -D warnings`)
- Format: `just fmt-check`
- Test layout: `just check-test-layout`
- Full suite before first push: `just ci`

Repo conventions that bind every task: sibling `*_test.rs` linked via
`#[path]` (never inline `#[cfg(test)] mod tests { ... }`); newtypes and explicit
destructuring; `let...else` for early returns; no `unwrap`/`expect`/`panic` in
non-test code (workspace lints deny them); `tracing` not `println`; absolute
imports. Never pair `tokio::time::pause` with a real `SqlitePool`.

---

## Task 1 — Item 4: server malformed vs missing version header

**Where it fits:** Smallest, fully self-contained; a pure `enforce_version`
refinement in `voom-worker-protocol`.

**Files:** `crates/voom-worker-protocol/src/http/server.rs` (`enforce_version`),
`crates/voom-worker-protocol/src/http/server_test.rs`.

**TDD:**
1. Add `enforce_version_malformed_header_reports_malformed`: insert a
   `x-voom-protocol-version: "1.0"` (unparseable) header; assert the error is
   `InvalidPayload` whose `detail` contains `"malformed"` and the offending
   value, not `"missing"`. Run it; confirm it fails against current code
   (detail says "missing").
2. Rework `enforce_version` to branch on three cases: header absent / not
   `to_str`-able → `InvalidPayload { "missing <header>" }`; present but not a
   `u32` → `InvalidPayload { "malformed <header>: <value>" }`; parseable →
   `negotiate(n).map(|_| ())`.
3. Keep `enforce_version_missing_header_is_invalid_payload` and
   `enforce_version_wrong_version_rejects` green.

**Acceptance:** absent → "missing" detail; present-unparseable → "malformed"
detail; both `InvalidPayload`; wrong version still `UnsupportedProtocolVersion`.

**Rollback:** revert the single function + test; no state, no schema.

---

## Task 2 — Item 2: client validates handshake `agreed`

**Where it fits:** Same crate as Task 1; defense-in-depth on the client's
handshake decode (ADR-0016 exact match).

**Files:** `crates/voom-worker-protocol/src/http/client.rs` (`handshake`),
`crates/voom-worker-protocol/src/http_test.rs`.

**TDD:**
1. Add a `tokio::test` that binds a raw `TcpListener`, accepts one connection,
   drains the request, and writes an HTTP 200 with body
   `{"agreed": <offered+1>}` (mirroring the raw-server pattern already in
   `http_test.rs`). Call `client.handshake(offered)` and assert
   `Err(ProtocolError::UnsupportedProtocolVersion { offered, .. })`. Run it;
   confirm it fails (current code returns `Ok`).
2. In `handshake`, after decoding the 2xx `HandshakeResponse`, return
   `Err(UnsupportedProtocolVersion { offered, expected: resp.agreed })` when
   `resp.agreed != offered`; otherwise return the response.
3. Add/keep a positive test: a server echoing `agreed == offered` still yields
   `Ok`. The existing round-trip tests (real `HttpServer`) already cover the
   matching path.

**Acceptance:** mismatched echo rejected with `UnsupportedProtocolVersion`;
matching echo succeeds; malformed/non-2xx paths unchanged.

**Rollback:** revert the added check + test.

---

## Task 3 — Item 3: fake worker delegates version check to `negotiate`

**Where it fits:** `voom-fakes` chaos worker; de-duplicates the operations-path
version check against ADR-0016's single source of truth.

**Files:** `crates/voom-fakes/src/bin/chaos_worker.rs` (`enforce_version`),
`crates/voom-fakes/src/bin/chaos_worker_test.rs` (sibling test, `use super::*`
already gives access to the private `enforce_version`).

**Verification note:** The only end-to-end driver of this binary
(`chaos_librarian_e2e`) is `#[ignore]`-gated (`just chaos-e2e-ci`), so it does
**not** run in `just ci`. `chaos_worker_test.rs` currently has no coverage of
`enforce_version`. Do not claim conformance coverage that CI does not run — add
a real unit test instead. This is a behavior-preserving de-duplication, so the
test is a characterization test that passes both before and after the refactor.

**TDD:**
1. Add characterization tests in `chaos_worker_test.rs` for the current
   `enforce_version`: wrong version → `UnsupportedProtocolVersion { offered,
   expected }`; missing header → `InvalidPayload`; correct version → `Ok(())`.
   Run them; confirm they pass on the current hand-rolled implementation (they
   lock in the behavior the refactor must preserve).
2. Replace the `if offered == voom_core::PROTOCOL_VERSION { Ok } else { Err(...) }`
   tail of `enforce_version` with `voom_worker_protocol::negotiate(offered).map(|_| ())`,
   keeping the existing present-and-parseable header extraction unchanged.
3. Re-run the tests (still green), `cargo build -p voom-fakes`, `just lint`.
   Do not un-gate any `#[ignore]` E2E test.

**Acceptance:** `enforce_version` delegates to `negotiate`; the new unit tests
run under `just ci` and stay green; build + lint clean; wrong-version /
missing-header rejection behavior unchanged.

**Rollback:** revert the function tail.

---

## Task 4 — Item 5: builtin-worker ensure uses `begin_immediate_tx`

**Where it fits:** `voom-control-plane`; aligns five read-then-write bootstraps
with the remote-execution contention pattern.

**Files:** `crates/voom-control-plane/src/transcode/commit.rs`,
`.../remux/commit.rs`, `.../audio/commit.rs`, `.../scan/mod.rs`,
`.../artifact/verify.rs`. Import `crate::cases::begin_immediate_tx`.

**TDD:** A deterministic `SQLITE_BUSY` race test would be timing-dependent and
flaky, so this is a consistency/hardening change verified by existing tests plus
lint:
1. In each of the five sites replace the deferred begin
   (`cp.pool.begin()` / `self.pool.begin()` / `begin_tx(&cp.pool)`) with
   `begin_immediate_tx(&cp.pool)` (or `&self.pool`), keeping the
   `ensure_*_in_tx` call and commit unchanged.
2. Fix imports per site: `verify.rs` already imports
   `{append_event, begin_tx, commit_tx}` and uses `begin_tx` a second time in
   `persist_verification_outcome` (unchanged by this task), so `begin_tx` stays
   imported — add `begin_immediate_tx` alongside it, do **not** remove it. Sites
   that inline `pool.begin()` gain a `begin_immediate_tx` import. Let `just lint`
   (unused-import denial) be the backstop; do not pre-emptively delete imports.
3. Run `cargo test -p voom-control-plane` and `just lint`.

**Acceptance:** all five sites use `begin_immediate_tx`; existing
transcode/remux/audio/scan/verify tests pass; lint clean.

**Rollback:** revert each site to its prior begin; restore imports.

---

## Task 5 — Item 1: recovery stat NotFound vs unstattable

**Where it fits:** `voom-control-plane` commit recovery; distinguishes genuine
target absence from occupied/unstattable targets.

**Files:** `crates/voom-control-plane/src/artifact/commit/recovery.rs`
(`recover_commit_inner`, line ~84),
`crates/voom-control-plane/src/artifact/commit/mod_test.rs`.

**TDD:**
1. Add `recover_commit_errors_when_target_stat_fails_non_absent`:
   - stage + verify bytes, run `commit_artifact_with_hooks(..., &FailAfterPrepare)`
     with `target = dir/install/target.bin` (create `dir/install` first) so a
     recovery-required record exists and the target is absent;
   - replace the intermediate component: `remove_dir_all(dir/install)` then
     `write(dir/install, b"x")`, so `symlink_metadata(dir/install/target.bin)`
     fails with `ENOTDIR` (kind != `NotFound`). Use `remove_dir_all` in case a
     temp sibling was created under `install`;
   - assert `cp.recover_commit(handle).await.unwrap_err().error_code()
     == ErrorCode::CommitFailure` and that no fresh install occurred.
   Run it; confirm it fails against current code (the `.ok()` collapses the
   error to `None` and the fresh-install path yields a different error/outcome).
2. Replace `observe_regular_file(&target_path).await.ok()` with an explicit
   probe:
   - `Ok(_)` → `Some(observe_regular_file(&target_path).await?)`;
   - `Err(e) if e.kind() == std::io::ErrorKind::NotFound` → `None`;
   - `Err(e)` → `return Err(VoomError::CommitFailure(format!( ... target_path ... )))`.
   Keep the downstream `already_installed` match (matching facts → finalize;
   `Some(_)` mismatch → `Conflict`; `None` → repromote) unchanged.
3. Run `cargo test -p voom-control-plane` including
   `recover_commit_repromotes_when_target_absent` and
   `recover_commit_resumes_finalize_when_target_already_installed`.

**Acceptance:** ENOTDIR (non-`NotFound`) target stat → `CommitFailure`, no fresh
install; absent target still repromotes; installed matching file still
finalizes; occupied non-file (dir/symlink) surfaces `ArtifactUnavailable`.

**Rollback:** revert `recover_commit_inner`'s probe block + test.

---

## Cross-cutting verification

After all tasks: `just ci` (fmt-check, lint, check-test-layout,
check-paused-time-db{,-selftest}, check-payload-deny-unknown{,-selftest}, test,
doc, deny, audit) green before the first push. No snapshot/OpenAPI/migration
artifacts are touched by any task, so none need regeneration.
