# Plan: `voom worker run-local` two-line stdout contract (#222)

Date: 2026-06-11
Spec: `docs/specs/run-local-stdout-contract.md`
Branch: `feat/audit-l1-run-local-stdout-contract`

Derived from the approved spec. The work is one integration test plus two
documentation edits; there is no production-code change. Tasks are sequential and
small enough for direct in-session execution (TDD), not subagent fan-out.

## Guardrails (run before every commit)

- `just ci` (`fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`,
  `audit`). Must be green.
- Run the single new test focused: `cargo test -p voom-cli --test run_local_stdout_contract`.
- Conventional Commits, imperative subject ≤72 chars, one logical change per
  commit, ending with the `Co-Authored-By: Claude Opus 4.8 (1M context)` trailer.

## Task 1 — Failing integration test (TDD red)

**Where it fits:** delivers acceptance criterion (a) of #222 — an end-to-end test
of the two-line `run-local` stdout protocol.

**File:** create `crates/voom-cli/tests/run_local_stdout_contract.rs`.

**Conventions:** integration tests live in `crates/*/tests/`. Spawn the shipped
binary via `env!("CARGO_BIN_EXE_voom")`. Build the bundled worker first with
`voom_test_support::worker::cargo_build_package("voom-ffmpeg-worker")` so
`run-local` resolves it as a sibling of `CARGO_BIN_EXE_voom`. Pass the DB via
`VOOM_DATABASE_URL`. Mirror `operator_execution_e2e.rs` for child management:
off-thread stdout line collection, off-thread stderr drain into a buffer, and
fail loudly (`panic!` with captured stderr) on early child exit, timeout, or a
nonzero exit. Use the same `#![expect(clippy::unwrap_used, clippy::panic, ...)]`
crate attribute the existing e2e test uses.

**Test body (`#[tokio::test(flavor = "multi_thread")]`):**

1. `tempfile::TempDir` root; a `NamedTempFile` SQLite DB inside it;
   `url = sqlite://<path>`.
2. `voom init` against the DB; assert exit 0 + `status:"ok"`.
3. Spawn `voom worker run-local --kind ffmpeg` with piped stdin/stdout/stderr,
   `VOOM_DATABASE_URL` set. Collect **every** stdout line through an mpsc channel
   on a reader thread (do not discard intermediate lines); drain stderr on another
   thread into an `Arc<Mutex<String>>`.
4. Read the first stdout line within a readiness timeout (reuse a 1-minute bound
   like the existing e2e). If the child exits first or the timeout elapses,
   `panic!` including the captured stderr. Parse it as JSON and assert:
   - `status == "ready"`, `kind == "ffmpeg"`, `worker_id` is a positive `u64`,
     `endpoint` parses as a `SocketAddr`.
   - it is a bare readiness line, not an envelope: `schema_version` and `command`
     are both absent (`Value::is_null` after `get`).
5. Close stdin (drop it) to trigger graceful shutdown. Collect remaining stdout
   lines until the channel disconnects, with a shutdown timeout. `child.wait()`
   and assert `status.success()`; on nonzero exit `panic!` with stderr.
6. Assert the **full** captured stdout is exactly two lines: line 1 is the
   readiness line from step 4 (same `worker_id`); line 2 is the retirement
   envelope with `command == "worker"`, `status == "ok"`,
   `data.status == "retired"`, and `data.worker_id` equal to the readiness
   `worker_id`. Assert no third line exists.
7. Run `voom worker list` against the same DB; assert the worker id from step 4
   is not in a live (`registered`/`active`) state.

**Acceptance criteria a reviewer can check:**
- Test file exists under `crates/voom-cli/tests/`, compiles, and is not behind any
  `#[ignore]` or env gate.
- It collects all stdout lines and asserts the count is exactly 2 (not "scan for
  the last JSON line").
- On a missing-ffmpeg / early-exit path it panics with the child's stderr, not a
  bare timeout.
- Before the implementation/doc step, this test passes already because the
  production behavior already emits the two-line contract — so this is a
  characterization/contract test. To satisfy TDD "confirm it fails for the right
  reason", first introduce a deliberate, reverted local break (see Task 2) to
  prove the assertion bites, then keep the unmodified production code.

## Task 2 — TDD red/green proof (no production change shipped)

**Where it fits:** the spec describes a *contract* test over already-correct
behavior. TDD still requires proving the test can fail. Production `run-local`
already emits exactly two lines, so:

1. Temporarily add a stray `println!`/second emit in `run_local_supervise`
   (`crates/voom-cli/src/commands/execution/worker.rs`) **in the working tree
   only**, run the new test, and confirm it fails on the "exactly two lines"
   assertion for the expected reason (3 lines / wrong line 2).
2. Revert the deliberate break (no production change is committed). Re-run the
   test green.

**Acceptance criteria:** the transcript shows the test failing with the stray
line present and passing once reverted; `git diff` on production source is empty
after this task.

**Rollback:** `git checkout -- crates/voom-cli/src/commands/execution/worker.rs`
to drop the temporary break.

## Task 3 — Document the contract

**Where it fits:** acceptance criterion (b) of #222.

**Files:**
1. `docs/runbooks/operator-real-media-execution.md` — add a short "stdout
   contract" note near the existing readiness-line block (around the step-2
   worker section and/or the existing "All commands emit a single JSON envelope"
   line) stating the explicit two-line shape and ordering: line 1 the bare
   readiness line, line 2 the standard retirement envelope on shutdown, with
   nothing else on stdout (logs go to stderr).
2. `AGENTS.md` — in the "CLI output contract" section, add one sentence naming
   `voom worker run-local` as the documented streaming exception to the
   one-envelope-per-invocation rule, linking the runbook and the spec.

**Conventions:** match existing doc tone; keep lines within the repo's wrap.
`just doc` (and prek end-of-file/trailing-whitespace hooks) must stay green.

**Acceptance criteria:** both docs describe the two-line contract; the AGENTS.md
CLI-output-contract section no longer reads as if `run-local` violates an
otherwise-absolute rule.

## Task 4 — Adversarial branch review + ship

1. `just ci` green.
2. `/challenge --json --base main` review loop (≤5 iterations); fix defensible
   findings, commit each.
3. Push; `gh pr create` against `main`, plain factual body ending `Closes #222`.
4. Drive CI green and `mergeStateStatus=CLEAN`/`mergeable=MERGEABLE`; rebase onto
   `origin/main` if `BEHIND`.

## Out of scope

- No production-code behavior change. If the test surfaces a real two-line
  violation in production, that is a separate bug to flag, not silently fix here.
- `--kind mkvtoolnix` is not separately tested (identical contract; label covered
  by unit tests).
- No `insta` snapshot (nondeterministic `worker_id`/`endpoint`).
