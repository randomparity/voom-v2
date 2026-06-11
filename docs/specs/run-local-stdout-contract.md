# Spec: `voom worker run-local` two-line stdout contract

Date: 2026-06-11
Status: Approved
Issue: #222 (audit L1 follow-up from `FABLE_AUDIT.md`)

## Context

Every `voom` invocation emits exactly one JSON envelope on stdout
(`AGENTS.md` → "CLI output contract"). `voom worker run-local` is the one
documented streaming exception: it is a long-running foreground supervisor that
prints a readiness signal as soon as its bundled worker has bound an endpoint and
been registered, then blocks until shutdown, then prints the final envelope.

That makes its stdout a **two-line protocol**:

1. A bare readiness line — `{"status":"ready","worker_id":<u64>,"kind":"ffmpeg"|"mkvtoolnix","endpoint":"<addr>"}`
   — emitted by `emit_ready_line` (`crates/voom-cli/src/commands/execution/worker.rs`).
   This is **not** a standard envelope; it has no `schema_version`/`command`
   wrapper. It exists so an operator (or supervising agent) can gate on readiness
   before dispatching work to the worker.
2. The standard one-envelope-per-invocation result, emitted on shutdown:
   `emit_ok("worker", { worker_id, kind, status: "retired" }, ...)` on a clean
   retire, or an error envelope via `emit_voom_error` if retirement fails.

The readiness line's *shape* is unit-tested (`ready_line_json`,
`worker_test.rs`). The `operator_execution_e2e.rs` end-to-end test drives
`run-local` as part of a full media pipeline and asserts the readiness line and
the retirement envelope, but it tolerates extra interleaved stdout lines (it
scans for `status == "ready"` and keeps the *last* JSON line as the envelope).
Nothing asserts the **strict** contract: that a single `run-local` lifecycle
writes *exactly two* well-formed JSON lines to stdout, in order, and nothing
else.

## Decision

1. Add a focused integration test
   (`crates/voom-cli/tests/run_local_stdout_contract.rs`) that isolates the
   stdout protocol from the compliance pipeline:
   - `voom init` a fresh on-disk SQLite DB.
   - Spawn one `voom worker run-local --kind ffmpeg` child with piped
     stdin/stdout/stderr.
   - Collect **every** stdout line (not just "the last one").
   - Wait for the readiness line, assert its full shape (`status:"ready"`,
     `kind:"ffmpeg"`, a positive `worker_id`, a parseable `endpoint`), and that
     it carries no envelope wrapper (`schema_version`/`command` absent).
   - Close stdin to trigger shutdown; wait for the child to exit 0.
   - Assert stdout is **exactly two lines**, both valid JSON, line 1 the
     readiness line and line 2 the retirement envelope
     (`status:"ok"`, `command:"worker"`, `data.status:"retired"`,
     `data.worker_id` equal to the readiness line's `worker_id`).
   - Assert the same worker is no longer live via `voom worker list`.

2. Document the two-line contract where `run-local` lives: a "stdout contract"
   subsection in `docs/runbooks/operator-real-media-execution.md`, and a sentence
   in the `AGENTS.md` "CLI output contract" section naming `run-local` as the
   documented streaming exception. The runbook already shows the readiness line;
   this makes the two-line shape and ordering explicit.

## Test gating

`run-local` reaching the readiness line requires the real bundled worker binary
(`voom-ffmpeg-worker`) and a real `ffmpeg`/`ffprobe` on `PATH` (the worker runs a
dependency preflight before binding). This matches `operator_execution_e2e.rs`,
which is **not** env-gated: CI installs `ffmpeg`/`mkvtoolnix` and runs the full
`--all-features` workspace test suite (`.github/workflows/ci.yml`). The new test
follows the same pattern — it builds `voom-ffmpeg-worker` with `cargo_build_package`
so `run-local` resolves it as a sibling of `CARGO_BIN_EXE_voom`, and relies on the
same toolchain the existing e2e already requires. No existing gated test is
un-gated or widened.

`--kind ffmpeg` alone is sufficient to exercise the protocol; the contract is
identical for `mkvtoolnix`, and the per-kind label is already covered by the
`ready_line_json` unit tests.

## Consequences

- A regression that adds a stray stdout line to `run-local` (e.g. a debug
  `println!`, or emitting the envelope twice) now fails a test, where today it
  would silently break agents that parse the two-line stream.
- The test adds one more real-ffmpeg-dependent integration test to the CLI suite.
  It is lighter than `operator_execution_e2e.rs` (one worker, no media fixture,
  no scan/policy/execute), so the marginal CI cost is small.

## Considered & rejected

- **An `insta` snapshot of the two lines.** Rejected: `worker_id`, `endpoint`
  port, and the DB path are nondeterministic, so a snapshot would need so much
  redaction that it would assert almost nothing. Structural assertions are
  clearer and match `operator_execution_e2e.rs`.
- **Env-gating the test behind a flag.** Rejected: it would diverge from the
  existing, ungated `operator_execution_e2e.rs` and from the operator override
  not to weaken or add gating. CI already provides the tools.
- **Extending `operator_execution_e2e.rs` instead of a new file.** Rejected: that
  test deliberately tolerates extra stdout lines because it interleaves many
  commands; tightening it to "exactly two lines" would conflate the pipeline
  oracle with the stdout-protocol assertion. A separate, minimal test states the
  contract directly.
