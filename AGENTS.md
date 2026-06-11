# AGENTS.md

This file provides guidance to agentic coding tools when working with code in this repository.

## Development rules

These rules apply to every task in this project unless explicitly overridden.
Bias: caution over speed on non-trivial work.

## Rule 1 — Architecture Trumps All
Project is pre-release, prioritize architectural correctness in design choices.
Good design leads to long project life.

## Rule 2 — Think Before Coding
State assumptions explicitly. Ask rather than guess.
Push back when a simpler approach exists. Stop when confused.

## Rule 3 — Simplicity First
Minimum code that solves the problem. Nothing speculative.
No abstractions for single-use code.

## Rule 4 — Surgical Changes
Touch only what you must. Don't improve adjacent code.
Match existing style. Don't refactor what isn't broken.

## Rule 5 — Goal-Driven Execution
Define success criteria. Loop until verified.
Strong success criteria let agents loop independently.

## Rule 6 — Use the model only for judgment calls
Use for: classification, drafting, summarization, extraction.
Do NOT use for: routing, retries, deterministic transforms.
If code can answer, code answers.

## Rule 7 — Surface conflicts, don't average them
If two patterns contradict, pick one (more recent / more tested).
Explain why. Flag the other for cleanup.

## Rule 8 — Read before you write
Before adding code, read exports, immediate callers, shared utilities.
If unsure why existing code is structured a certain way, ask.

## Rule 9 — Tests verify intent, not just behavior
Tests must encode WHY behavior matters, not just WHAT it does.
A test that can't fail when business logic changes is wrong.

## Rule 10 — Checkpoint after every significant step
Summarize what was done, what's verified, what's left.
Don't continue from a state you can't describe back.

## Rule 11 — Match the codebase's conventions, even if you disagree
Conformance > taste inside the codebase.
If you think a convention is harmful, surface it. Don't fork silently.

## Rule 12 — Fail loud
"Completed" is wrong if anything was skipped silently.
"Tests pass" is wrong if any were skipped.
Default to surfacing uncertainty, not hiding it.

## Commands

All routine actions go through `just` (see `justfile`):

| Command | Purpose |
|---|---|
| `just setup` | One-shot bootstrap: toolchain, cargo tools, git hooks via `prek`. |
| `just ci` | Run the exact CI suite locally: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, `audit`. |
| `just fmt` / `just fmt-check` | `cargo fmt --all` (write / check). |
| `just lint` | `cargo clippy --workspace --all-targets --all-features -- -D warnings`. |
| `just test` | `cargo test --workspace --all-features`. |
| `just audit` / `just deny` | Supply-chain checks (`cargo-audit`, `cargo-deny`). |
| `just run -- <args>` | Invoke the `voom` CLI from source. |
| `just smoke` | End-to-end check of `version` / `health` / `init` against an ephemeral SQLite. |

Run a single test: `cargo test -p <crate> <test_name>` (e.g. `cargo test -p voom-cli version_envelope`).
Tests inside the `voom-cli` integration suite use `insta` snapshots — review with `cargo insta review` after a deliberate change.

Pre-commit hooks (installed by `just setup` via `prek install`) delegate to `just` recipes so they cannot drift from `just ci`: `fmt-check`, `check-test-layout`, `check-paused-time-db`(+selftest), `lint`, a light `cargo test --quiet`, `deny`, and `audit` on staged Rust / `Cargo.lock` / `Cargo.toml` / `deny.toml` changes. Two checks are deliberately CI-only because they are too slow per commit: `just doc` and the full `--all-features` test build (`just test`) — run `just ci` before pushing. Don't bypass the hooks; fix the underlying issue.

## Architecture

### Crate layering (one-way dependencies)

```
voom-core ── shared domain types (VoomError, VersionInfo, Config, IDs, Clock)
   ▲
   ├─ voom-store ── SqlitePool, MIGRATOR, schema probe, repositories
   ├─ voom-events ── durable event envelope and payload taxonomy
   ├─ voom-policy ── policy DSL AST, validation, compilation, fixtures
   │     ▲
   │     └─ voom-plan ── compliance reports and execution-plan generation
   ├─ voom-scheduler ── worker scoring and selection domain
   └─ voom-worker-protocol ── worker HTTP/NDJSON contracts and typed payloads
   ▲
voom-control-plane ── app-services layer and workflow orchestration
   ▲
voom-api (axum router, no binary yet)   voom-cli (`voom` binary)
```

Worker binaries (`voom-ffprobe-worker`, `voom-ffmpeg-worker`,
`voom-mkvtoolnix-worker`, `voom-verify-artifact-worker`) depend on
`voom-worker-protocol` for their external contract. Test and fake-support crates
live outside the production dependency path. `voom-artifact` holds
artifact-domain helpers shared outside the control-plane shell
(`commit_pipeline` — pending-commit record + event glue and
recovery-required commit data); keep filesystem promotion, worker
dispatch, and use-case assembly in `voom-control-plane`.

### Load-bearing invariants

Several behaviors are deliberate and documented in `docs/adr/` + `docs/specs/voom-control-plane-design.md`. Preserve them:

- **`connect()` vs `init()` are separate.** `voom_store::connect` opens an existing DB and **never creates files or directories**. Only `voom init` (the CLI command) calls `voom_store::init`, which is the sole path that creates databases and applies migrations. `ControlPlane::open` wraps `connect` — read-side code paths must never migrate. (`docs/adr/0003`.)
- **Tickets route work; events record facts.** Durable ticket/lease rows are the only mechanism that schedules execution. Events are append-only facts for audit/UI/metrics — they do not claim, lease, or trigger work directly. (`docs/adr/0001`.)
- **All providers are out-of-process workers.** No in-process fast path. `voom-worker-protocol` marks and enforces the HTTP/NDJSON contract boundary. (`docs/adr/0002`.)
- **Stack is tokio + sqlx + axum, async-first.** Blocking code is the exception. Migrations are embedded via `sqlx::migrate!` against `migrations/`.

### Durable payload schema-evolution contract (audit M4, ADR 0013)

A JSON column deserialized into a `Deserialize` type carries
`#[serde(deny_unknown_fields)]` on the real serde unit — a plain or newtype-wrapped
content struct. A tagged enum is not annotated (serde ignores it there); its
variants are newtype variants over annotated content structs, and serde's tag
discriminator rejects unknown variant names. Inline tagged struct-variants are a
silent no-op and are forbidden for durable enums. Payloads evolve **additive-only**
(new fields `Option`/`#[serde(default)]`); a rename/remove/retype is a deliberate,
coordinated change requiring binary-before-DB upgrade ordering, never a silent
default. New durable typed columns are added to `docs/payload-contract-inventory.md`
and `scripts/payload-contract-scope.txt`. Enforced by
`scripts/check-payload-deny-unknown.sh` in `just ci`.

### CLI output contract

The `voom` binary is agent-facing. Every invocation MUST emit exactly one JSON envelope on stdout (`schema_version`, `command`, `status`, `data` | `error`, optional `local`, `warnings`). Logs go to stderr. Even clap parse failures route through `envelope::emit_err` so stdout is always parseable — see `crates/voom-cli/src/main.rs`. Exit codes: `0` ok, `1` BAD_ARGS, `2` runtime error.

Error `code` strings are public contract — defined in `voom_core::VoomError::code()` (`DB_UNREACHABLE`, `DB_PARTIAL_SCHEMA`, `DB_DIRTY_MIGRATION`, `DB_SCHEMA_TOO_NEW`, `CONFIG_INVALID`, `NOT_FOUND`, `INTERNAL`) plus CLI-layer codes (`BAD_ARGS`). Don't rename or repurpose them; add new variants instead.

### Workspace / versioning

Single source of truth for the package version is `[workspace.package].version` in the root `Cargo.toml`. All member crates inherit via `version.workspace = true`. Internal path deps inherit via `[workspace.dependencies]` + `{ workspace = true }` — do not hardcode versions on internal deps. The release cadence is bump → tag → bump (`-dev` suffix on `main` between releases); full procedure in `docs/release-process.md`.

Adding a new crate: add it to `[workspace] members`, set `version.workspace = true` and the other inherited fields, and if it's an internal dep for other crates also add a `[workspace.dependencies]` entry pointing at its path.

### Where things live

- ADRs: `docs/adr/`
- Sprint specs: `docs/specs/` and `docs/superpowers/specs/`
- Migrations: `migrations/*.sql` (bundled into `voom-store::MIGRATOR`)
- Insta snapshots: `crates/voom-cli/tests/snapshots/`
- Clippy/lints config: `[workspace.lints]` in root `Cargo.toml` (pedantic on, panic/unwrap/expect denied)

## Testing layout

Unit tests live in a **sibling file** named `<source>_test.rs`, linked
from the parent source via `#[path]`:

```rust
// At the bottom of foo.rs
#[cfg(test)]
#[path = "foo_test.rs"]
mod tests;
```

```rust
// foo_test.rs
use super::*;

#[test]
fn something() { /* ... */ }
```

Integration tests stay in `crates/*/tests/` (no change). The
feature-gated helper `crates/voom-store/src/test_support.rs` stays
as-is and is classified as test code by SonarCloud.

`just check-test-layout` (also wired into `just ci`) enforces the
convention: no inline `#[cfg(test)] mod tests { ... }` in `src/`, and
every `*_test.rs` must have a matching `#[path]` declaration in its
sibling source file. See `docs/adr/0004-sibling-unit-tests.md`.

**Never pair `tokio::time::pause()`/`advance()` with a real `SqlitePool`.**
When tokio's clock is paused it auto-advances virtual time whenever the runtime
is idle — including while an `await` is parked on sqlx's blocking SQLite thread
— so the paused clock jumps past the pool's `acquire_timeout` and DB calls fail
spuriously with `DbUnreachable`. Drive DB-touching tests on real time and
control *domain* time through the injected `Clock` (`ManualClock`).
`just check-paused-time-db` (wired into `just ci`) enforces this: it fails when
one test file references `SqlitePool`/`ControlPlane` and also calls
`tokio::time::pause`/`advance`. See `docs/adr/0012-paused-time-db-pool-guard.md`.

`just coverage` produces `lcov.info` consumed by SonarCloud.
