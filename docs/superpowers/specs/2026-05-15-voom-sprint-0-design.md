---
name: voom-sprint-0-design
description: Sprint 0 (Spec & Skeleton) design for VOOM — empty-but-real Cargo workspace, SQLite migration runner, CLI/API skeletons emitting tagged JSON envelopes, and the engineering guardrails (lints, hooks, CI, ADRs, justfile, versioning policy) every later sprint inherits.
status: proposed
date: 2026-05-15
sprint: 0
references:
  - docs/specs/voom-control-plane-design.md
---

# VOOM Sprint 0 — Spec & Skeleton

## 1. Goal & Scope

Sprint 0 produces an **empty-but-real** VOOM: a Cargo workspace with all the
crate boundaries the top-level design names, a working SQLite migration runner
used by both disk and in-memory modes, a CLI that prints `version` and `health`
JSON through the envelope future commands will reuse, an axum API skeleton
serving `/health`, and the engineering guardrails (lints, hooks, CI, ADRs,
versioning policy, developer convenience) every later sprint inherits.

**Sprint 0 does NOT** implement any domain logic — no jobs, no leases, no
policies, no workers, no events. Those land in Sprint 1+. The crates that hold
those concepts exist as empty libraries so their boundary is enforced by the
compiler from day one.

The top-level design's Sprint 0 exit criteria are:

- Empty app starts.
- Database initializes on disk and in memory.
- CLI can print version and health JSON.
- CI-equivalent local checks pass.

This spec is how we get there.

## 2. Workspace Layout

Single Cargo workspace at the repo root. The `members` list is enumerated
explicitly (no globs) so adding a crate is a deliberate act.

| Crate | Kind | Sprint 0 contents | Owns (eventually) |
|---|---|---|---|
| `voom-core` | lib | Newtype IDs (`MediaId`, `TicketId`, `LeaseId`, `WorkerId`, `JobId`, `EventId`), `VoomError` enum, `VersionInfo`, `Config`, time abstraction | Domain types and traits referenced by every other crate |
| `voom-store` | lib | `sqlx::SqlitePool` builder for `:memory:` and on-disk URLs, embedded migration runner (`sqlx::migrate!`), one no-op migration (`0001_init.sql` creating `schema_meta`), `SchemaMetaRepo` trait + Sqlite impl | Repositories for jobs, leases, events, artifacts, etc. |
| `voom-events` | lib | Empty (placeholder `pub mod kind`) | Append-only event log writer and projections |
| `voom-policy` | lib | Empty | Policy grammar, parser, compiler |
| `voom-plan` | lib | Empty | Planner: snapshot → compliance report → ExecutionPlan DAG |
| `voom-scheduler` | lib | Empty | Lease selection, capability/grant matching, lookahead |
| `voom-artifact` | lib | Empty | ArtifactHandle, resolver, placement scoring |
| `voom-worker-protocol` | lib | Empty | HTTP/JSON + NDJSON wire types shared by host and workers |
| `voom-control-plane` | lib | Wires `voom-store`; exposes a `ControlPlane` handle consumed by API and CLI | App-services layer used by API/CLI/daemon |
| `voom-api` | lib | axum `Router` with `GET /health`; no server binary yet | REST surface |
| `voom-cli` | bin | clap-derive command tree with `version` and `health` subcommands, tagged-envelope writer | All operator commands |

**Naming convention.** Every crate is `voom-*`; the binary inside `voom-cli` is
named `voom`. Crates live under `crates/<name>/` (flat, no nesting). The repo
root holds: workspace `Cargo.toml`, `crates/`, `migrations/`, `docs/`,
`.github/`, `justfile`, `rustfmt.toml`, `deny.toml`, `audit.toml`,
`.pre-commit-config.yaml`, `README.md`.

Empty crates aren't dead weight — they make `cargo build` enforce "no upward
dependencies" and let Sprint 1+ land code without touching `Cargo.toml`.

## 3. Storage Foundation (`voom-store`)

### Connection

A single async function:

```rust
pub async fn connect(url: &str) -> Result<SqlitePool, VoomError>;
```

handles both `sqlite::memory:` (tests) and `sqlite:///path/to/voom.db` (disk).

For on-disk URLs it sets `journal_mode=WAL`, `synchronous=NORMAL`,
`foreign_keys=ON`, and `busy_timeout=5000`.

For `:memory:` it forces `SqliteConnectOptions::shared_cache(true)` and pool
size = 1 so test transactions see the same in-memory DB.

### Migrations

`migrations/` directory at the repo root, embedded via
`sqlx::migrate!("../../migrations")` from `voom-store`.

Sprint 0 ships exactly one migration: `0001_init.sql` creating
`schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)` and inserting
`('schema_init_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))` so the timestamp
is captured by SQLite itself at migration time — no host-clock involvement,
no separate post-migration write.

This proves migrations run end-to-end without committing to any domain tables.
Sprint 1 adds real schema.

### Run-on-open contract

`connect()` runs `MIGRATOR.run(&pool).await?` before returning. There is no
separate `voom migrate` command in Sprint 0 — opening the DB is migration.
Sprint 5 can split them when `voom init` lands.

### Repository trait stubs

```rust
#[async_trait]
pub trait SchemaMetaRepo: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>, VoomError>;
    async fn set(&self, key: &str, value: &str) -> Result<(), VoomError>;
}

pub struct SqliteSchemaMetaRepo(SqlitePool);
```

This proves the repository pattern works end-to-end (CLI → control plane →
repo → SQL) on a real table, without committing to job/event/lease schemas.
Sprint 1 adds the rest in this shape.

### Tests

- `voom-store/tests/migration.rs` — runs migrations against `:memory:` and
  against a `tempfile`-backed disk DB; asserts `schema_meta` has the init row
  in both.
- `voom-store/tests/repo_roundtrip.rs` — writes and reads through
  `SchemaMetaRepo` on both backends.

These two tests are the template Sprint 1's repository tests follow.

## 4. CLI Shape (`voom-cli`)

### Framework

`clap` v4 with derive macros. Single binary `voom`.

### Top-level args

- `--database-url <url>` — overrides env and default.
- `--format=json|plain` — default `json` (agent-friendly mandate); `plain` is an
  opt-in for humans.
- `--log-level <level>` — `error|warn|info|debug|trace`, default `info`.
- `--log-format=text|json` — default mirrors `--format`.
- `--no-color` — disable ANSI in plain mode.

### Sprint 0 subcommands

`voom version` payload (see §6 for full version semantics):

```json
{
  "schema_version": "0",
  "command": "version",
  "status": "ok",
  "data": {
    "version": "0.1.0-dev+abc1234",
    "semver": "0.1.0-dev",
    "git_sha": "abc1234",
    "dirty": false,
    "release": false,
    "build_profile": "debug"
  },
  "warnings": [],
  "error": null
}
```

`voom health` payload:

```json
{
  "schema_version": "0",
  "command": "health",
  "status": "ok",
  "data": {
    "db": {
      "url": "sqlite:///Users/dave/Library/Application Support/voom/voom.db",
      "schema_init_at": "2026-05-15T18:23:00Z",
      "migration_count": 1
    },
    "config_path": "/Users/dave/Library/Preferences/voom/config.toml",
    "runtime": { "tokio_workers": 8 }
  },
  "warnings": [],
  "error": null
}
```

On failure both commands return the same envelope with
`status: "error"`, `data: null`,
`error: { "code": "DB_UNREACHABLE", "message": "...", "hint": "..." }`.

### Envelope writer

Lives in `voom-cli` (private module). Two entry points:

```rust
fn emit_ok<T: Serialize>(command: &str, data: T, warnings: Vec<String>);
fn emit_err(command: &str, code: &'static str, message: String, hint: Option<String>);
```

Tests assert exact JSON shape against snapshots (`insta` crate).

### Exit codes (stable from day one)

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | User error (bad flag, invalid input) |
| 2 | System error (DB unreachable, IO failure) |
| 3 | Not found |

Agents can branch on these without parsing stderr.

### Git SHA at build

A `build.rs` in `voom-cli` runs `git rev-parse --short HEAD` and
`git diff --quiet HEAD`, emitting `cargo:rustc-env=VOOM_GIT_SHA=...` and
`VOOM_GIT_DIRTY=...`. Fallback values when `git` is unavailable (CI source
tarballs): `VOOM_GIT_SHA=unknown`, `VOOM_GIT_DIRTY=false`. See §6.

## 5. API Skeleton (`voom-api`)

### Shape

```rust
pub fn router(control_plane: ControlPlane) -> axum::Router;
```

One route: `GET /health` returns the same envelope shape as the CLI `health`
command, so the agent contract is one shape, not two.

### No server binary

Sprint 0 does not start a listening process. The router is exercised by
`voom-api/tests/health_route.rs` using `axum::Router::oneshot` via
`tower::ServiceExt`. The daemon binary that calls `axum::serve(...)` arrives
in Sprint 6.

### Why ship the router now?

Two reasons:

1. It proves `voom-control-plane` can be consumed by something other than the
   CLI, which is the whole point of the layered design.
2. It locks in the envelope shape across both surfaces before any second
   command exists, preventing CLI-only divergence later.

## 6. Versioning Policy

**Scheme.** SemVer 2.0 for the workspace and every published crate. All
`voom-*` crates share one version, bumped together. Workspace `Cargo.toml`
defines `[workspace.package] version = "..."`; members inherit via
`version.workspace = true`.

**Starting version.** `0.1.0-dev` in `Cargo.toml` on `main`.

### Display vs. crate version

Cargo's version field can't carry build metadata cleanly, so the SHA lives
outside `Cargo.toml`.

- `Cargo.toml` version: `0.1.0-dev` between releases, `0.1.0` on a tagged
  release commit, then bumped to `0.1.1-dev` (or `0.2.0-dev`) in the very next
  commit on `main`.
- `voom version` build-script env vars:
  - `VOOM_SEMVER` — from `CARGO_PKG_VERSION`.
  - `VOOM_GIT_SHA` — short SHA from `git rev-parse --short HEAD`.
  - `VOOM_GIT_DIRTY` — `true` if working tree has uncommitted changes at build.
- Canonical display string: `{semver}+{sha}` for clean builds,
  `{semver}+{sha}.dirty` if the tree was modified.

Examples:

- Dev build of clean main: `0.1.0-dev+abc1234`
- Dev build with local edits: `0.1.0-dev+abc1234.dirty`
- Tagged release: `0.1.0+def5678`

### Envelope fields

```json
{
  "version": "0.1.0-dev+abc1234",
  "semver": "0.1.0-dev",
  "git_sha": "abc1234",
  "dirty": false,
  "release": false,
  "build_profile": "debug"
}
```

`release` is `true` iff `semver` has no pre-release component. Agents branch
on `release` without parsing SemVer.

### Release workflow

On `main`:

1. Bump `Cargo.toml` from `0.1.0-dev` → `0.1.0` in a release-prep commit.
2. Tag `v0.1.0` on that commit.
3. Immediately follow with a `0.1.1-dev` (or `0.2.0-dev`) bump commit.

CI's release job builds from the tag; the binary self-reports
`0.1.0+<tag-commit-sha>`. We do not amend tags after creation.

### Sprint 0 deliverables for versioning

- `build.rs` in `voom-cli` emitting the three env vars with documented
  fallbacks.
- `VersionInfo` struct in `voom-core` so API and CLI report identical shapes.
- `tests/version_envelope.rs` snapshot-asserting JSON shape and that
  `release` is `false` when `semver` ends in `-dev`.
- One-page release runbook at `docs/release-process.md` describing
  bump-tag-bump cadence.

## 7. Cross-Cutting Concerns

### Errors

`voom-core` exposes `VoomError` (`thiserror`) with variants Sprint 0 actually
hits: `Database`, `Migration`, `Config`, `NotFound`, `Internal`. Each variant
carries a stable string code (matching the envelope's `error.code`) via
`pub fn code(&self) -> &'static str`.

Library crates return `Result<T, VoomError>`. The CLI binary additionally uses
`anyhow` only at the outermost `main` (per the global "thiserror for libraries,
anyhow for applications" rule). Sprint 1+ extends the enum as new error
classes appear — never a catch-all `Other(String)`.

### Logging

`tracing` everywhere; `tracing-subscriber` initialized once in `voom-cli`'s
`main`.

Two output modes wired from the start:

- Human format (`--log-format=text`, default for `--format=plain`).
- JSON-line format (`--log-format=json`, default for `--format=json`) so logs
  and command output are both machine-parseable.

Log level from `--log-level` (default `info`) or `RUST_LOG` if set. Logs go to
**stderr**; the JSON envelope goes to **stdout**. Agents can stream both
without mixing.

### Config loading

`voom-core::config` exposes:

```rust
pub struct Config {
    pub database_url: String,
    pub log_level: String,
    pub log_format: LogFormat,
}
```

Resolution order, highest priority first:

1. CLI flag.
2. Environment variable (`VOOM_DATABASE_URL`, `VOOM_LOG_LEVEL`,
   `VOOM_LOG_FORMAT`).
3. Compiled-in default.

The XDG path resolver (using the `directories` crate) computes the default
`database_url` lazily so tests can override without touching the filesystem.

**No config file is read in Sprint 0** — its path is computed and reported by
`voom health` so users see where it would live, but parsing is a Sprint 5
deliverable. This avoids inventing a config format before any setting needs
one.

### Async runtime

`tokio` with `#[tokio::main(flavor = "multi_thread")]` in the CLI; tests use
`#[tokio::test]`. No custom runtime configuration in Sprint 0.

### Shared dependencies

`[workspace.dependencies]` in the root `Cargo.toml` pins exact versions of
every shared crate (`tokio`, `sqlx`, `axum`, `serde`, `serde_json`,
`thiserror`, `anyhow`, `tracing`, `tracing-subscriber`, `clap`, `directories`,
`time`, `async-trait`, `insta`, `tower`, `tempfile`).

Member crates reference them as `tokio = { workspace = true, features = [...] }`.
One place to bump versions; no version drift.

## 8. Engineering Guardrails

### Lints

Root `[workspace.lints]` block with the full clippy ruleset from the global
standards (pedantic warn, `unwrap_used`/`panic`/`todo`/`dbg_macro` denied,
etc.). Members inherit via `lints.workspace = true`. Zero-warnings policy
enforced by `-D warnings` in CI's clippy step.

### Format

`rustfmt.toml` at the root pinning `edition = "2024"`, `max_width = 100`
(matches the 100-char line limit), `imports_granularity = "Crate"`. Formatting
is checked in CI and via the prek hook.

### `cargo deny`

`deny.toml` at the root. Owns **licenses** (permissive allowlist:
MIT/Apache-2.0/BSD-*/ISC/Unicode-DFS-2016) and **bans** (deny duplicates of
expensive transitives where practical). The `advisories` section is set to
`version = 2` with `yanked = "deny"` so yanked crates fail the build, but
vulnerability scanning is delegated to `cargo audit` (below). Run in CI on
every push.

### `cargo audit`

Owns **vulnerability scanning** against the RustSec advisory DB. Configured
via `audit.toml` at the root:

- `[advisories] vulnerability = "deny"`, `unmaintained = "warn"`,
  `unsound = "deny"`, `notice = "warn"`.
- `ignore = []` (every ignore must be added with a comment justifying it and an
  expiry date).

Wired in three places:

- **`just ci` (local + CI):** `audit` is one of the recipes `just ci`
  invokes, so any local or CI run that calls `just ci` exercises it.
- **Dedicated `audit.yml` workflow:** a standalone workflow whose only job
  runs `rustsec/audit-check` (SHA-pinned). Triggers on push, PR, and a daily
  `schedule:` cron so new advisories against unchanged code still page us.
  Failures appear as a distinct GitHub check named "audit", not buried inside
  the `ci` job's logs.
- **Pre-commit:** a `prek` hook running `cargo audit --deny warnings` against
  the lockfile. Gated to only run when `Cargo.lock` changes (prek's
  `files: '^Cargo\.lock$'`) so it doesn't slow down every commit.

Running the scanner in both `just ci` (via the `ci.yml` workflow) and the
dedicated `audit.yml` workflow is deliberate: `just ci` keeps the
contract that local and CI produce identical results, while `audit.yml`
exists primarily for the daily cron and the distinct named check.

### Pre-commit (`prek`)

`.pre-commit-config.yaml` runs:

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --quiet`
- `cargo audit --deny warnings` (only on `Cargo.lock` changes)
- generic hygiene hooks (trailing whitespace, large file guard, EOF newline)

`prek auto-update --cooldown-days 7` configured.

### CI (GitHub Actions)

Two workflows under `.github/workflows/`:

**`ci.yml`** on push and PR:

- Matrix over `ubuntu-latest` and `macos-latest`.
- Steps: checkout (SHA-pinned, `persist-credentials: false`) → install Rust
  stable with `clippy`/`rustfmt` → cache (`Swatinem/rust-cache`, SHA-pinned) →
  `just ci`.

**`audit.yml`** as a dedicated workflow (see `cargo audit` subsection above):

- Triggers: push, PR, daily `schedule:` cron.
- Runs `rustsec/audit-check` (SHA-pinned).
- Exists primarily for the daily cron and to surface a distinct GitHub check.

**`release.yml`** on tag push `v*.*.*`:

- Builds release binaries for `linux-x86_64`, `linux-aarch64`, `macos-aarch64`.
- Uploads to GitHub Release.
- Sprint 0 ships the workflow but doesn't cut a tag.

All actions pinned to full commit SHAs with version comments. `zizmor` run
locally before committing workflow changes (manual, not CI).

### Dependabot

`.github/dependabot.yml` for `cargo` and `github-actions` ecosystems, weekly
schedule, 7-day cooldown, grouped minor/patch updates per ecosystem.

When Dependabot opens a PR bumping a vulnerable dep, both the `audit` job (on
the new lockfile) and the `cargo deny check` job (on licenses/bans) run on
the PR. Green checks on both required before merge.

### ADRs

Lightweight MADR-style markdown under `docs/adr/`. Filename pattern:
`NNNN-kebab-title.md` starting at `0001`. Each ADR has frontmatter (`status`,
`date`, `deciders`) and four sections: Context, Decision, Consequences,
Alternatives Considered.

Sprint 0 ships three:

- `0001-durable-jobs-over-events.md` — captures the design doc's "durable jobs
  route work; events record facts" choice.
- `0002-out-of-process-workers-only.md` — captures "all providers are
  out-of-process from day one, no in-process fast path."
- `0003-sqlx-and-tokio-foundation.md` — captures Sprint 0's async-first
  storage choice and its downstream implications (axum, tokio runtime).

## 9. Developer Convenience (`justfile`)

A `justfile` at the repo root is the canonical entry point for everyday
tasks, so contributors don't memorize cargo invocations and `just ci`
produces bit-for-bit the same checks GitHub Actions runs.

```just
# Default action: list available recipes
default:
    @just --list

# Bootstrap a fresh checkout for development
setup:
    @echo "==> Verifying Rust toolchain"
    @command -v rustup >/dev/null || { echo "Install rustup: https://rustup.rs"; exit 1; }
    rustup show active-toolchain || rustup toolchain install stable
    rustup component add clippy rustfmt
    @echo "==> Installing cargo tools (idempotent)"
    cargo install --locked cargo-audit cargo-deny prek
    @echo "==> Verifying uv + Python 3.13"
    @command -v uv >/dev/null || { echo "Install uv: https://docs.astral.sh/uv/"; exit 1; }
    uv python install 3.13
    @echo "==> Installing git hooks"
    prek install
    prek auto-update --cooldown-days 7
    @echo "==> Warming cargo cache"
    cargo fetch
    @echo "==> Setup complete. Try: just ci"

# Run the exact set of checks GitHub Actions runs
ci: fmt-check lint test deny audit
    @echo "==> All CI checks passed"

# Individual checks (also called by `ci`)
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace --all-features

audit:
    cargo audit --deny warnings

deny:
    cargo deny check

# Run the CLI binary
run *ARGS:
    cargo run -p voom-cli -- {{ARGS}}

# Run version + health end-to-end against an ephemeral on-disk DB
smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    db=$(mktemp -t voom-smoke.XXXXXX.db)
    trap 'rm -f "$db"' EXIT
    cargo run -q -p voom-cli -- --database-url "sqlite://$db" version | jq -e '.status == "ok"'
    cargo run -q -p voom-cli -- --database-url "sqlite://$db" health  | jq -e '.status == "ok"'

# Remove build artifacts
clean:
    cargo clean
```

**Contract: `just ci` ≡ GitHub Actions `ci.yml`.** `ci.yml` calls `just ci`
rather than duplicating `cargo` invocations, so the two cannot drift. A check
added to one is automatically in the other.

**`setup` is idempotent.** Safe to re-run after pulling new commits;
`cargo install --locked` is a no-op when the binary is current, `prek install`
is idempotent, `uv python install 3.13` is idempotent.

**Python via `uv` is dev-environment convenience.** It is not a runtime
dependency of any `voom-*` crate. Python 3.13 is provided for ad-hoc scripts
(`zizmor`, one-off data tooling) without forcing each contributor to manage
Python versions manually.

**`smoke` recipe** exercises both Sprint 0 exit-criterion commands against a
real temp DB and validates the envelope shape with `jq`. CI's smoke step (and
humans) call it.

## 10. Exit Criteria (verification map)

| Exit criterion | How it's verified |
|---|---|
| Empty app starts | `cargo run -p voom-cli -- version` exits 0 with valid envelope JSON; CI runs this via `just smoke`. |
| DB initializes on disk and in memory | `voom-store/tests/migration.rs` covers both; `cargo run -p voom-cli -- health --database-url 'sqlite::memory:'` and `... 'sqlite:///tmp/voom-test.db'` both return `status: "ok"`. |
| CLI prints version and health JSON | Snapshot tests (`insta`) on both envelopes; `just smoke` validates JSON parse with `jq` in CI. |
| CI-equivalent local checks pass | `just ci` exits 0 from a fresh clone after `just setup`. `ci.yml` calls `just ci` so the two cannot drift. |

## 11. Out of Scope (Sprint 0 non-goals — explicit)

- No `voom init` command (Sprint 5).
- No daemon binary, no listening HTTP server (Sprint 6).
- No worker protocol implementation (Sprint 2).
- No job/lease/event/policy/plan tables — only `schema_meta` (Sprint 1+).
- No config file parsing (Sprint 5).
- No release artifacts cut; `release.yml` exists but isn't fired.
- No `xtask` runner; `cargo`-native commands + `just` suffice in Sprint 0.
- No metrics or Prometheus endpoint (Sprint 9).
- No `voom-worker-protocol` types beyond an empty module (Sprint 2).
- No real domain logic in any crate other than `voom-store`'s `schema_meta`.
