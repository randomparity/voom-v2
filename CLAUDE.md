# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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

Pre-commit hooks (installed by `just setup` via `prek install`) run fmt-check, clippy, test, and `cargo audit` on staged Rust / `Cargo.lock` changes. Don't bypass them; fix the underlying issue.

## Architecture

### Crate layering (one-way dependencies)

```
voom-core ── shared domain types (VoomError, VersionInfo, Config, IDs, Clock)
   ▲
voom-store ── SqlitePool, MIGRATOR, schema probe, repositories
   ▲
voom-control-plane ── app-services layer (ControlPlane::open / health)
   ▲
voom-api (axum router, no binary yet)   voom-cli (`voom` binary)
```

Empty placeholder crates (`voom-events`, `voom-policy`, `voom-plan`, `voom-scheduler`, `voom-artifact`, `voom-worker-protocol`) exist so the boundary is visible from Sprint 0; they get real code in later sprints. Don't put logic in them without checking the spec.

### Load-bearing invariants

Several behaviors are deliberate and documented in `docs/adr/` + `docs/specs/voom-control-plane-design.md`. Preserve them:

- **`connect()` vs `init()` are separate.** `voom_store::connect` opens an existing DB and **never creates files or directories**. Only `voom init` (the CLI command) calls `voom_store::init`, which is the sole path that creates databases and applies migrations. `ControlPlane::open` wraps `connect` — read-side code paths must never migrate. (`docs/adr/0003`.)
- **Tickets route work; events record facts.** Durable ticket/lease rows are the only mechanism that schedules execution. Events are append-only facts for audit/UI/metrics — they do not claim, lease, or trigger work directly. (`docs/adr/0001`.)
- **All providers are out-of-process workers.** No in-process fast path. The empty `voom-worker-protocol` crate marks the boundary. (`docs/adr/0002`.)
- **Stack is tokio + sqlx + axum, async-first.** Blocking code is the exception. Migrations are embedded via `sqlx::migrate!` against `migrations/`.

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

`just coverage` produces `lcov.info` consumed by SonarCloud.
