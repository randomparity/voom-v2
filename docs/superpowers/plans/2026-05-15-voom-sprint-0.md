# VOOM Sprint 0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the empty-but-real Sprint 0 skeleton specified in `docs/superpowers/specs/2026-05-15-voom-sprint-0-design.md`: an 11-crate Cargo workspace, SQLite `init`/`connect`/`probe` storage layer, CLI with `version`/`health`/`init` commands emitting the tagged JSON envelope, axum `/health` router with the `local` block redacted, build-time SemVer+SHA versioning, and the engineering guardrails (lints, hooks, CI workflows, ADRs, justfile, release runbook) every later sprint inherits.

**Architecture:** Strict layered workspace — `voom-core` at the bottom (zero deps on sibling crates), `voom-store` only depends on `voom-core`, `voom-control-plane` wraps `voom-store`, `voom-api` and `voom-cli` consume `voom-control-plane`. Six placeholder crates (`voom-events`, `voom-policy`, `voom-plan`, `voom-scheduler`, `voom-artifact`, `voom-worker-protocol`) ship empty so the compiler enforces boundaries before Sprint 1+ code lands. Storage is async (sqlx + tokio); migrations are gated behind an explicit `voom init`. The envelope writer is parametrized so the API code path is structurally unable to emit the host-only `local` block.

**Tech Stack:** Rust stable (edition 2024), tokio multi-thread runtime, sqlx 0.8+ with `runtime-tokio` and `sqlite` features, axum 0.7+ (or 0.8 if current), clap 4 with `derive`, `serde` + `serde_json`, `thiserror` (libraries) + `anyhow` (CLI main), `tracing` + `tracing-subscriber`, `insta` for envelope snapshots, `directories` for XDG paths, `time` with `serde` + `formatting`. Dev tooling: `prek`, `cargo-deny`, `cargo-audit`, `just`, `uv`. CI: GitHub Actions with `Swatinem/rust-cache` and `rustsec/audit-check`.

**Tooling version policy:** Every `cargo add` invocation in this plan resolves the current stable version at the time of execution. When the plan calls for a specific GitHub Action SHA, look it up at execution time with `gh api repos/<owner>/<repo>/git/refs/tags/<tag> --jq .object.sha`. Do not hardcode versions from this plan document.

---

## File Structure

This sprint produces ~60 files. Here is the responsibility map; refer back when a task says "Create".

**Workspace root:**
- `Cargo.toml` — workspace definition: `[workspace] members = [...]`, `[workspace.package]` (version 0.1.0-dev, edition 2024, license, authors), `[workspace.dependencies]` pinning shared crates, `[workspace.lints.clippy]` with the global ruleset.
- `Cargo.lock` — generated.
- `rustfmt.toml` — edition 2024, max_width 100, imports_granularity = Crate.
- `deny.toml` — license allowlist, ban rules, advisories yanked = deny.
- `audit.toml` — RustSec advisory policy.
- `.pre-commit-config.yaml` — fmt/clippy/test/audit hooks.
- `.gitignore` — Rust artifacts plus the existing superpowers/plans line.
- `justfile` — verbatim from spec §9.
- `README.md` — quickstart (`just setup` → `just smoke`).

**`crates/voom-core/`:**
- `Cargo.toml` — depends on `thiserror`, `serde`, `time`.
- `src/lib.rs` — module re-exports.
- `src/error.rs` — `VoomError` enum + `code()`.
- `src/version.rs` — `VersionInfo` struct, parsing, `release()` predicate.
- `src/config.rs` — `Config`, `LogFormat`, XDG path resolution.
- `src/ids.rs` — newtype IDs (`MediaId`, `TicketId`, `LeaseId`, `WorkerId`, `JobId`, `EventId`).
- `src/clock.rs` — `Clock` trait + `SystemClock` impl.

**`crates/voom-store/`:**
- `Cargo.toml` — depends on `voom-core`, `sqlx`, `tokio`, `async-trait`, `time`.
- `src/lib.rs` — module re-exports.
- `src/pool.rs` — `connect()`.
- `src/schema.rs` — `probe_schema()`, `SchemaState`.
- `src/init.rs` — `init()`, `InitReport`, `MIGRATOR`.
- `src/repo/mod.rs` — `Repository` marker trait + re-exports.
- `src/repo/schema_meta.rs` — `SchemaMetaRepo` trait + `SqliteSchemaMetaRepo`.
- `tests/init.rs` — init idempotency over `:memory:` and disk.
- `tests/health_no_migrate.rs` — connect never advances schema.
- `tests/repo_roundtrip.rs` — SchemaMetaRepo round-trip.

**`migrations/`:**
- `0001_init.sql` — creates `schema_meta` table + inserts `schema_init_at`.

**Empty placeholder crates** (each just `Cargo.toml` + `src/lib.rs` with a placeholder module):
- `crates/voom-events/`
- `crates/voom-policy/`
- `crates/voom-plan/`
- `crates/voom-scheduler/`
- `crates/voom-artifact/`
- `crates/voom-worker-protocol/`

**`crates/voom-control-plane/`:**
- `Cargo.toml` — depends on `voom-core`, `voom-store`.
- `src/lib.rs` — `ControlPlane` handle, `HealthSnapshot`, `init()`/`health()`/`version()` methods.

**`crates/voom-api/`:**
- `Cargo.toml` — depends on `voom-core`, `voom-control-plane`, `axum`, `serde`, `serde_json`, `tower`.
- `src/lib.rs` — `pub fn router()`, route handlers.
- `src/envelope.rs` — API-only envelope variants (no `local`).
- `tests/health_route.rs` — `/health` shape + `local` absence + uninitialized error case.

**`crates/voom-cli/`:**
- `Cargo.toml` — depends on `voom-core`, `voom-store`, `voom-control-plane`, `clap`, `tokio`, `anyhow`, `serde`, `serde_json`, `tracing`, `tracing-subscriber`, `insta` (dev), `tempfile` (dev).
- `build.rs` — emits `VOOM_GIT_SHA`, `VOOM_GIT_DIRTY`.
- `src/main.rs` — async main, top-level args, subcommand dispatch.
- `src/cli.rs` — clap `Cli` struct, subcommand enum.
- `src/envelope.rs` — CLI envelope writer with `local` block, `emit_ok`/`emit_err`.
- `src/logging.rs` — `tracing-subscriber` setup (stderr).
- `src/commands/mod.rs`
- `src/commands/version.rs`
- `src/commands/health.rs`
- `src/commands/init.rs`
- `tests/version_envelope.rs` — `insta` snapshots of the version envelope.
- `tests/health_envelope.rs` — snapshots: pre-init, post-init, error variants.
- `tests/init_envelope.rs` — snapshots: first-init and idempotent re-init.

**`.github/`:**
- `workflows/ci.yml` — push/PR, matrix, calls `just ci`.
- `workflows/audit.yml` — push/PR/cron, `rustsec/audit-check`.
- `workflows/release.yml` — tag-pushed binaries.
- `dependabot.yml` — weekly cargo + github-actions updates.

**`docs/`:**
- `release-process.md` — bump-tag-bump runbook.
- `adr/0001-durable-jobs-over-events.md`
- `adr/0002-out-of-process-workers-only.md`
- `adr/0003-sqlx-and-tokio-foundation.md`

---

## Task 1: Create implementation branch

**Files:**
- None (git only).

- [ ] **Step 1: Confirm we're on `main` with a clean tree**

Run: `git -C /Users/dave/src/voom-v2 status --short --branch`
Expected: `## main` and no working tree changes (or only the not-yet-committed plan file).

- [ ] **Step 2: Create the implementation branch**

Run: `git -C /Users/dave/src/voom-v2 switch -c sprint-0-skeleton`
Expected: `Switched to a new branch 'sprint-0-skeleton'`

All subsequent tasks happen on this branch.

- [ ] **Step 3: Commit any uncommitted plan file (if needed)**

```bash
cd /Users/dave/src/voom-v2
if ! git diff --quiet HEAD -- docs/superpowers/plans/; then
  git add docs/superpowers/plans/2026-05-15-voom-sprint-0.md
  git commit -m "Add Sprint 0 implementation plan"
fi
```

---

## Task 2: Workspace `Cargo.toml`

**Files:**
- Create: `Cargo.toml`
- Create: `rustfmt.toml`
- Modify: `.gitignore`

- [ ] **Step 1: Write the workspace manifest**

Create `/Users/dave/src/voom-v2/Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = [
    "crates/voom-core",
    "crates/voom-store",
    "crates/voom-events",
    "crates/voom-policy",
    "crates/voom-plan",
    "crates/voom-scheduler",
    "crates/voom-artifact",
    "crates/voom-worker-protocol",
    "crates/voom-control-plane",
    "crates/voom-api",
    "crates/voom-cli",
]

[workspace.package]
version = "0.1.0-dev"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"
authors = ["VOOM contributors"]
repository = "https://github.com/randomparity/voom-v2"

[workspace.dependencies]
# Pinned at the workspace level; member crates use `{ workspace = true }`.
# All versions resolved by `cargo add` at implementation time (see Task 3+).

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# Panic prevention
unwrap_used = "deny"
expect_used = "warn"
panic = "deny"
panic_in_result_fn = "deny"
unimplemented = "deny"
# No cheating
allow_attributes = "deny"
# Code hygiene
dbg_macro = "deny"
todo = "deny"
print_stdout = "deny"
print_stderr = "deny"
# Safety
await_holding_lock = "deny"
large_futures = "deny"
exit = "deny"
mem_forget = "deny"
# Pedantic relaxations (too noisy)
module_name_repetitions = "allow"
similar_names = "allow"
# Doc-completeness pedantic lints — Sprint 0 prioritizes shipping working
# code over exhaustive # Errors / # Panics docs on every fallible function.
# Re-enable in a documentation-hardening sprint.
missing_errors_doc = "allow"
missing_panics_doc = "allow"

[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"
```

Note: `[workspace.dependencies]` is intentionally empty for now. Task 3 populates it via `cargo add --package <member>` invocations that record the resolved versions into the workspace section.

**Test-time lint relaxations (convention).** The workspace lints deny `unwrap_used`, `panic`, and forbid `unsafe_code`. Production code never uses these. Test code conventionally does (clarity over plumbing `Result<()>` through assertions). Every voom-* crate root (`lib.rs` or `main.rs`) introduced in Tasks 4+ MUST start with:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
```

This is a `#[cfg(test)]`-only relaxation; production builds remain strict. `unsafe_code = "forbid"` is left *unrelaxed* — no test in this sprint mutates the process environment (Task 6's `EnvSource` abstraction makes this unnecessary). If a future task genuinely needs `unsafe`, it must justify downgrading `forbid` to `deny` in its own design discussion.

`allow_attributes = "deny"` denies `#[allow(...)]` but `#[expect(...)]` (Rust 1.81+) is the supported relaxation mechanism throughout the workspace.

- [ ] **Step 2: Write `rustfmt.toml`**

Create `/Users/dave/src/voom-v2/rustfmt.toml`:

```toml
edition = "2024"
max_width = 100
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
newline_style = "Unix"
use_field_init_shorthand = true
```

- [ ] **Step 3: Extend `.gitignore`**

Edit `/Users/dave/src/voom-v2/.gitignore`. Final contents:

```
# Superpowers Plugin
# Plans are transient, specs are permanent
docs/superpowers/plans/

# Rust
/target/
**/*.rs.bk
Cargo.lock.bak

# macOS
.DS_Store

# Editor scratch
*.swp
.idea/
.vscode/
```

Note: `Cargo.lock` is intentionally NOT ignored — this workspace ships a binary, so the lockfile is committed.

- [ ] **Step 4: Verify the workspace parses**

Run: `cargo metadata --no-deps --format-version=1 > /dev/null`
Expected: exits 0. (It will warn about missing member crates because we haven't created them yet; that's fine — Task 3 fixes it.)

Actually, this *will* fail because the member directories don't exist. Skip this verification until Task 3 is done.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rustfmt.toml .gitignore
git commit -m "Add workspace Cargo.toml, rustfmt config, and Rust .gitignore entries"
```

---

## Task 3: Scaffold all 11 empty crates

**Files:** Created per crate below.

This task creates the directory and minimal `Cargo.toml` + `src/lib.rs` for every workspace member. Later tasks fill in the real content.

- [ ] **Step 1: Create directory structure**

```bash
cd /Users/dave/src/voom-v2
mkdir -p crates/{voom-core,voom-store,voom-events,voom-policy,voom-plan,voom-scheduler,voom-artifact,voom-worker-protocol,voom-control-plane,voom-api,voom-cli}/src
mkdir -p crates/voom-cli/tests crates/voom-api/tests crates/voom-store/tests
mkdir -p crates/voom-store/src/repo crates/voom-cli/src/commands
mkdir -p migrations
```

- [ ] **Step 2: Write each empty crate's `Cargo.toml` and `src/lib.rs`**

For each library crate in `voom-core`, `voom-events`, `voom-policy`, `voom-plan`, `voom-scheduler`, `voom-artifact`, `voom-worker-protocol`, `voom-store`, `voom-control-plane`, `voom-api`, create:

`crates/<name>/Cargo.toml`:

```toml
[package]
name = "<name>"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true

[lints]
workspace = true
```

`crates/<name>/src/lib.rs`:

```rust
//! <name> — see workspace README for sprint scope.
```

For `voom-cli` (binary), create:

`crates/voom-cli/Cargo.toml`:

```toml
[package]
name = "voom-cli"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true

[[bin]]
name = "voom"
path = "src/main.rs"

[lints]
workspace = true
```

`crates/voom-cli/src/main.rs`:

```rust
fn main() {
    // Real entrypoint added in later tasks.
}
```

- [ ] **Step 3: Empty placeholder module markers**

For each of the six fully-empty placeholders (`voom-events`, `voom-policy`, `voom-plan`, `voom-scheduler`, `voom-artifact`, `voom-worker-protocol`), append a placeholder `pub mod` line so the crate isn't fully empty:

```rust
//! Reserved for Sprint N — see workspace spec for scope.

pub mod placeholder {
    //! Intentionally empty until the owning sprint lands.
}
```

(Replace `Sprint N` with the sprint number from the spec table: events/policy/plan/scheduler/artifact land Sprint 3+; worker-protocol lands Sprint 2.)

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: 11 crates compile with zero errors and zero warnings.

If clippy lints fire on the empty crates (e.g., `missing_docs_in_private_items`), the doc comments in step 2 should suppress them. If they don't, suppress per-crate with `#![expect(...)]` at the crate root with a reason — never `#[allow(...)]`.

- [ ] **Step 5: Run clippy to confirm zero warnings**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: exits 0.

- [ ] **Step 6: Commit**

```bash
git add crates/ migrations/
git commit -m "Scaffold all 11 workspace crates with empty placeholders"
```

---

## Task 4: `voom-core` — `VoomError`

**Files:**
- Modify: `crates/voom-core/Cargo.toml`
- Create: `crates/voom-core/src/error.rs`
- Modify: `crates/voom-core/src/lib.rs`

- [ ] **Step 1: Add dependencies**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-core thiserror
cargo add --package voom-core serde --features derive
```

This will populate `[workspace.dependencies]` if you set `cargo` defaults to prefer workspace deps. If not, edit `Cargo.toml` to move the resolved version into `[workspace.dependencies]` and change member entries to `{ workspace = true, features = [...] }`. Same pattern for every subsequent `cargo add` invocation in this plan.

- [ ] **Step 2: Write the failing test**

Create `crates/voom-core/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("database error: {0}")]
    Database(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("schema is newer than this binary: {0}")]
    SchemaTooNew(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl VoomError {
    /// Stable string code matching the JSON envelope's `error.code`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "DB_UNREACHABLE",
            Self::Migration(_) => "DB_PARTIAL_SCHEMA",
            Self::SchemaTooNew(_) => "DB_SCHEMA_TOO_NEW",
            Self::Config(_) => "CONFIG_INVALID",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Internal(_) => "INTERNAL",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_variant_has_db_unreachable_code() {
        let err = VoomError::Database("connection refused".into());
        assert_eq!(err.code(), "DB_UNREACHABLE");
    }

    #[test]
    fn migration_variant_has_partial_schema_code() {
        let err = VoomError::Migration("missing migration".into());
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[test]
    fn schema_too_new_variant_has_too_new_code() {
        let err = VoomError::SchemaTooNew("future migration applied".into());
        assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
    }

    #[test]
    fn internal_variant_has_internal_code() {
        let err = VoomError::Internal("unexpected".into());
        assert_eq!(err.code(), "INTERNAL");
    }
}
```

- [ ] **Step 3: Wire into lib.rs**

Replace `crates/voom-core/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod error;

pub use error::VoomError;
```

- [ ] **Step 4: Run tests**

Run: `cargo test --package voom-core --lib`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-core/ Cargo.toml Cargo.lock
git commit -m "Add VoomError enum with stable error codes"
```

---

## Task 5: `voom-core` — `VersionInfo`

**Files:**
- Create: `crates/voom-core/src/version.rs`
- Modify: `crates/voom-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/voom-core/src/version.rs`:

```rust
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VersionInfo {
    pub version: String,
    pub semver: String,
    pub git_sha: String,
    pub dirty: bool,
    pub release: bool,
    pub build_profile: String,
}

impl VersionInfo {
    /// Build a `VersionInfo` from raw build-script outputs.
    ///
    /// `semver` is `CARGO_PKG_VERSION` at compile time.
    /// `git_sha` is the short SHA (or "unknown" when git is unavailable).
    /// `dirty` is true when the working tree had uncommitted changes at build.
    /// `build_profile` is "debug" or "release".
    #[must_use]
    pub fn new(semver: &str, git_sha: &str, dirty: bool, build_profile: &str) -> Self {
        let release = !semver.contains('-');
        let mut version = format!("{semver}+{git_sha}");
        if dirty {
            version.push_str(".dirty");
        }
        Self {
            version,
            semver: semver.to_owned(),
            git_sha: git_sha.to_owned(),
            dirty,
            release,
            build_profile: build_profile.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_build_is_not_release() {
        let v = VersionInfo::new("0.1.0-dev", "abc1234", false, "debug");
        assert!(!v.release);
        assert_eq!(v.version, "0.1.0-dev+abc1234");
    }

    #[test]
    fn tagged_build_is_release() {
        let v = VersionInfo::new("0.1.0", "def5678", false, "release");
        assert!(v.release);
        assert_eq!(v.version, "0.1.0+def5678");
    }

    #[test]
    fn dirty_tree_appends_dirty_suffix() {
        let v = VersionInfo::new("0.1.0-dev", "abc1234", true, "debug");
        assert_eq!(v.version, "0.1.0-dev+abc1234.dirty");
    }

    #[test]
    fn unknown_sha_still_renders() {
        let v = VersionInfo::new("0.1.0-dev", "unknown", false, "debug");
        assert_eq!(v.version, "0.1.0-dev+unknown");
    }
}
```

- [ ] **Step 2: Wire into lib.rs**

Update `crates/voom-core/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod error;
pub mod version;

pub use error::VoomError;
pub use version::VersionInfo;
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package voom-core --lib version`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-core/
git commit -m "Add VersionInfo with release/dirty derivation"
```

---

## Task 6: `voom-core` — `Config` and path resolution

**Files:**
- Modify: `crates/voom-core/Cargo.toml`
- Create: `crates/voom-core/src/config.rs`
- Modify: `crates/voom-core/src/lib.rs`

- [ ] **Step 1: Add `directories` dependency**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-core directories
```

- [ ] **Step 2: Write the failing test**

Create `crates/voom-core/src/config.rs`. The `Config::resolve_from` entry point takes an `EnvSource` trait so tests inject a `HashMap`-backed env source instead of mutating the process env. `unsafe_code = "forbid"` stays untouched.

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::error::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    pub fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(VoomError::Config(format!(
                "log_format must be 'text' or 'json', got {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub database_url: String,
    pub log_level: String,
    pub log_format: LogFormat,
    pub config_path: PathBuf,
}

/// Source of environment variables. Production uses `ProcessEnv`; tests inject
/// `MapEnv` so they never touch `std::env`.
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

pub struct MapEnv {
    map: HashMap<String, String>,
}

impl MapEnv {
    #[must_use]
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    #[must_use]
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.map.insert(key.to_owned(), value.to_owned());
        self
    }
}

impl Default for MapEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
}

impl Config {
    /// Resolve config, reading any missing values from the supplied env source.
    ///
    /// Used by tests with `MapEnv` and by `resolve()` with `ProcessEnv`.
    pub fn resolve_from<E: EnvSource>(
        env: &E,
        database_url_override: Option<String>,
        log_level_override: Option<String>,
        log_format_override: Option<String>,
    ) -> Result<Self, VoomError> {
        let database_url = database_url_override
            .or_else(|| env.get("VOOM_DATABASE_URL"))
            .map_or_else(default_database_url, Ok)?;
        let log_level = log_level_override
            .or_else(|| env.get("VOOM_LOG_LEVEL"))
            .unwrap_or_else(|| "info".to_owned());
        let log_format_str = log_format_override
            .or_else(|| env.get("VOOM_LOG_FORMAT"))
            .unwrap_or_else(|| "json".to_owned());
        let log_format = LogFormat::parse(&log_format_str)?;
        let config_path = default_config_path()?;
        Ok(Self { database_url, log_level, log_format, config_path })
    }

    /// Production entry point — reads from the live process environment.
    pub fn resolve(
        database_url_override: Option<String>,
        log_level_override: Option<String>,
        log_format_override: Option<String>,
    ) -> Result<Self, VoomError> {
        Self::resolve_from(
            &ProcessEnv,
            database_url_override,
            log_level_override,
            log_format_override,
        )
    }
}

fn project_dirs() -> Result<directories::ProjectDirs, VoomError> {
    directories::ProjectDirs::from("", "", "voom")
        .ok_or_else(|| VoomError::Config("could not resolve user data directory".into()))
}

fn default_database_url() -> Result<String, VoomError> {
    let dirs = project_dirs()?;
    let path = dirs.data_dir().join("voom.db");
    Ok(format!("sqlite://{}", path.display()))
}

fn default_config_path() -> Result<PathBuf, VoomError> {
    let dirs = project_dirs()?;
    Ok(dirs.config_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_parses_text_and_json() {
        assert_eq!(LogFormat::parse("text").unwrap(), LogFormat::Text);
        assert_eq!(LogFormat::parse("json").unwrap(), LogFormat::Json);
    }

    #[test]
    fn log_format_rejects_unknown() {
        let err = LogFormat::parse("xml").unwrap_err();
        assert_eq!(err.code(), "CONFIG_INVALID");
    }

    #[test]
    fn override_takes_priority_over_env() {
        let env = MapEnv::new().with("VOOM_DATABASE_URL", "sqlite::env");
        let cfg = Config::resolve_from(
            &env,
            Some("sqlite::override".into()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(cfg.database_url, "sqlite::override");
    }

    #[test]
    fn env_used_when_no_override() {
        let env = MapEnv::new().with("VOOM_DATABASE_URL", "sqlite::env-value");
        let cfg = Config::resolve_from(&env, None, None, None).unwrap();
        assert_eq!(cfg.database_url, "sqlite::env-value");
    }

    #[test]
    fn defaults_yield_sqlite_url_when_env_empty() {
        let env = MapEnv::new();
        let cfg = Config::resolve_from(&env, None, None, None).unwrap();
        assert!(cfg.database_url.starts_with("sqlite://"));
    }

    #[test]
    fn log_format_env_parsed_into_enum() {
        let env = MapEnv::new().with("VOOM_LOG_FORMAT", "text");
        let cfg = Config::resolve_from(&env, None, None, None).unwrap();
        assert_eq!(cfg.log_format, LogFormat::Text);
    }
}
```

No `unsafe` block. No `std::env::set_var`. The tests run in parallel safely because nothing they touch is process-global.

- [ ] **Step 3: Wire into lib.rs**

Update `crates/voom-core/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod config;
pub mod error;
pub mod version;

pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use error::VoomError;
pub use version::VersionInfo;
```

The `#![cfg_attr(test, expect(...))]` preamble is convention for every voom-* crate root (lib.rs / main.rs) that has unit tests — see Task 2 note. It scopes the relaxation to `#[cfg(test)]` compilation only.

- [ ] **Step 4: Run tests**

Run: `cargo test --package voom-core --lib config`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-core/ Cargo.toml Cargo.lock
git commit -m "Add Config + LogFormat with XDG-based defaults"
```

---

## Task 7: `voom-core` — `Clock` abstraction and ID newtypes

**Files:**
- Modify: `crates/voom-core/Cargo.toml`
- Create: `crates/voom-core/src/clock.rs`
- Create: `crates/voom-core/src/ids.rs`
- Modify: `crates/voom-core/src/lib.rs`

- [ ] **Step 1: Add `time` dep**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-core time --features serde,formatting
```

- [ ] **Step 2: Write Clock**

Create `crates/voom-core/src/clock.rs`:

```rust
use time::OffsetDateTime;

/// Wall-clock abstraction; production uses `SystemClock`, tests inject fakes.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_recent_timestamp() {
        let before = OffsetDateTime::now_utc();
        let now = SystemClock.now();
        let after = OffsetDateTime::now_utc();
        assert!(now >= before && now <= after);
    }
}
```

- [ ] **Step 3: Write IDs**

Create `crates/voom-core/src/ids.rs`:

```rust
use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(MediaId);
define_id!(TicketId);
define_id!(LeaseId);
define_id!(WorkerId);
define_id!(JobId);
define_id!(EventId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_as_bare_numbers() {
        let id = JobId(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "42");
    }

    #[test]
    fn ids_round_trip_through_json() {
        let id = TicketId(7);
        let json = serde_json::to_string(&id).unwrap();
        let back: TicketId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
```

Add the `serde_json` dev-dep:

```bash
cargo add --package voom-core --dev serde_json
```

- [ ] **Step 4: Wire into lib.rs**

Update `crates/voom-core/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Core domain types shared by every voom-* crate.

pub mod clock;
pub mod config;
pub mod error;
pub mod ids;
pub mod version;

pub use clock::{Clock, SystemClock};
pub use config::{Config, EnvSource, LogFormat, MapEnv, ProcessEnv};
pub use error::VoomError;
pub use ids::{EventId, JobId, LeaseId, MediaId, TicketId, WorkerId};
pub use version::VersionInfo;
```

- [ ] **Step 5: Run all voom-core tests**

Run: `cargo test --package voom-core`
Expected: all green (~13 tests across error/version/config/clock/ids).

- [ ] **Step 6: Commit**

```bash
git add crates/voom-core/ Cargo.toml Cargo.lock
git commit -m "Add Clock trait, SystemClock, and ID newtypes"
```

---

## Task 8: Migration SQL

**Files:**
- Create: `migrations/0001_init.sql`

- [ ] **Step 1: Write the migration**

Create `/Users/dave/src/voom-v2/migrations/0001_init.sql`:

```sql
CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

INSERT INTO schema_meta (key, value)
VALUES ('schema_init_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
```

The `STRICT` table option (SQLite 3.37+) enforces declared column types — catches accidental type coercions that bite later. SQLite 3.37 ships everywhere we care about.

- [ ] **Step 2: Commit**

```bash
git add migrations/0001_init.sql
git commit -m "Add 0001_init.sql creating schema_meta with init timestamp"
```

---

## Task 9: `voom-store` — `connect()` (no migrations)

**Files:**
- Modify: `crates/voom-store/Cargo.toml`
- Create: `crates/voom-store/src/pool.rs`
- Modify: `crates/voom-store/src/lib.rs`

- [ ] **Step 1: Add dependencies**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-store --path crates/voom-core
cargo add --package voom-store sqlx --features runtime-tokio,sqlite,migrate,time
cargo add --package voom-store tokio --features rt-multi-thread,macros,sync
cargo add --package voom-store time --features serde,formatting,parsing
cargo add --package voom-store async-trait
cargo add --package voom-store --dev tempfile
cargo add --package voom-store --dev tokio --features rt-multi-thread,macros
```

Also append to `crates/voom-store/Cargo.toml` a `[features]` block — `init_on` (Task 11) is feature-gated and only visible when this feature is enabled:

```toml
[features]
# Test-only public surface: enables init_on(pool) and similar
# pool-injection helpers needed by tests that want to seed pre-init state.
# Production crates MUST NOT enable this.
test-support = []
```

- [ ] **Step 2: Write the failing test**

Create `crates/voom-store/src/pool.rs`. The module exposes **two** entry points, deliberately separated by mutation intent:

- `connect(url)` — read-side. `create_if_missing(false)`, no `mkdir`. Errors with `DB_UNREACHABLE` when the file or parent doesn't exist. This is what `health`, the API, and any other read path use, so a typo'd path fails loudly instead of silently creating a fresh empty DB.
- `connect_or_create(url)` — write-side. `create_if_missing(true)` + `ensure_parent_dir()`. **Only `init()` calls this.**

The split is the contract that makes `voom health` filesystem-safe.

```rust
use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, SqlitePool};
use voom_core::VoomError;

/// Open a SQLite pool against an existing database. **Never creates files or
/// directories.** Used by every read-side path; the explicit `connect_or_create`
/// is reserved for `init()`.
pub async fn connect(url: &str) -> Result<SqlitePool, VoomError> {
    connect_inner(url, /* create = */ false).await
}

/// Open a SQLite pool, creating the database file and any missing parent
/// directories. Only `init()` should call this.
pub async fn connect_or_create(url: &str) -> Result<SqlitePool, VoomError> {
    connect_inner(url, /* create = */ true).await
}

async fn connect_inner(url: &str, create: bool) -> Result<SqlitePool, VoomError> {
    let is_memory = url.contains(":memory:");

    if create && !is_memory {
        ensure_parent_dir(url)?;
    }

    let mut options: SqliteConnectOptions = url
        .parse()
        .map_err(|e| VoomError::Database(format!("invalid sqlite url {url:?}: {e}")))?;

    // Per-connection settings (safe on read-side; not persisted to the DB file):
    options = options
        .create_if_missing(create || is_memory)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_millis(5000));

    if is_memory {
        options = options.shared_cache(true);
    }

    // Sprint 0 uses rollback-journal mode for all on-disk DBs. WAL would
    // create -wal/-shm sidecars that are visible even to readers, which
    // breaks the read-side no-filesystem-side-effects contract once a DB
    // has been initialized with WAL. WAL is a performance optimization
    // (concurrent readers + writer) that Sprint 0 doesn't need; revisit in
    // Sprint 6 (daemon) when concurrent access pressure is real.

    let pool_size = if is_memory { 1 } else { 8 };

    let _ = options.disable_statement_logging();

    SqlitePoolOptions::new()
        .max_connections(pool_size)
        .min_connections(if is_memory { 1 } else { 0 })
        .connect_with(options)
        .await
        .map_err(|e| {
            VoomError::Database(format!(
                "pool open failed for {url:?} (create={create}): {e}"
            ))
        })
}

/// Extract the filesystem path from a `sqlite:` URL and create any missing
/// parent directories. Accepts `sqlite:///abs/path`, `sqlite://relative/path`,
/// `sqlite:/abs/path`, and bare `path` forms.
fn ensure_parent_dir(url: &str) -> Result<(), VoomError> {
    let path_str = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);

    // Strip any sqlx query string ("?mode=...").
    let path_str = path_str.split('?').next().unwrap_or(path_str);

    if path_str.is_empty() {
        return Ok(());
    }

    let path = Path::new(path_str);
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|e| {
        VoomError::Database(format!(
            "could not create database parent directory {}: {e}",
            parent.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_in_memory_succeeds() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);
    }

    #[tokio::test]
    async fn connect_on_existing_disk_db_succeeds() {
        // Seed an existing DB via the create-mode opener, then verify the
        // read-only opener can attach to it.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        connect_or_create(&url).await.unwrap();

        let pool = connect(&url).await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);
    }

    #[tokio::test]
    async fn connect_does_not_create_sqlx_migrations_table() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(exists.is_none(), "connect() must not create migration tracking table");
    }

    #[tokio::test]
    async fn connect_refuses_missing_file() {
        // Read-side opener must NOT create the file. Path is in a tempdir so
        // the parent exists; only the .db file is missing.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.db");
        let url = format!("sqlite://{}", missing.display());

        let err = connect(&url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
        assert!(!missing.exists(), "connect() must NOT create the database file");
    }

    #[tokio::test]
    async fn connect_does_not_create_parent_directory() {
        // Read-side opener must NOT mkdir parents. Pointing at a nested path
        // whose parent doesn't exist should fail without filesystem side
        // effects.
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("absent-dir/voom.db");
        assert!(!nested.parent().unwrap().exists());

        let url = format!("sqlite://{}", nested.display());
        let err = connect(&url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
        assert!(!nested.parent().unwrap().exists(), "connect() must NOT mkdir parents");
        assert!(!nested.exists());
    }

    #[tokio::test]
    async fn connect_or_create_creates_missing_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c/voom.db");
        assert!(!nested.parent().unwrap().exists(), "parent must not exist yet");

        let url = format!("sqlite://{}", nested.display());
        let pool = connect_or_create(&url).await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);

        assert!(nested.parent().unwrap().exists(), "connect_or_create() must mkdir -p the parent");
        assert!(nested.exists(), "sqlite must have created the db file");
    }

    #[tokio::test]
    async fn neither_opener_creates_wal_or_shm_sidecars() {
        // Sprint 0 uses rollback-journal mode exclusively. Verify both the
        // write-side opener AND the subsequent read-side open leave the
        // filesystem free of -wal/-shm sidecar files.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("voom.db");
        let url = format!("sqlite://{}", db.display());

        // Write-side: create the DB.
        {
            let pool = connect_or_create(&url).await.unwrap();
            sqlx::query("CREATE TABLE marker (id INTEGER)")
                .execute(&pool)
                .await
                .unwrap();
        }
        let wal = db.with_extension("db-wal");
        let shm = db.with_extension("db-shm");
        assert!(!wal.exists(), "connect_or_create() must not produce -wal");
        assert!(!shm.exists(), "connect_or_create() must not produce -shm");

        // Read-side: open and confirm sidecars still absent.
        let pool = connect(&url).await.unwrap();
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM marker")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!wal.exists(), "connect() must not produce -wal");
        assert!(!shm.exists(), "connect() must not produce -shm");
    }
}
```

- [ ] **Step 3: Wire into lib.rs**

Update `crates/voom-store/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod pool;

pub use pool::{connect, connect_or_create};
```

- [ ] **Step 4: Run tests**

Run: `cargo test --package voom-store --lib`
Expected: 7 passed (in-memory open, existing-disk open, no migration table, refuses missing file, refuses missing parent dir, connect_or_create mkdir, no sidecars on read-side).

- [ ] **Step 5: Commit**

```bash
git add crates/voom-store/ Cargo.toml Cargo.lock
git commit -m "voom-store: split connect (no-create) from connect_or_create (mkdir + create)"
```

---

## Task 10: `voom-store` — `MIGRATOR`, `probe_schema()`, and `SchemaState`

**Files:**
- Create: `crates/voom-store/src/migrator.rs`
- Create: `crates/voom-store/src/schema.rs`
- Modify: `crates/voom-store/src/lib.rs`

`MIGRATOR` lives in its own module so `schema.rs` can reference it without depending on Task 11's `init.rs`. Task 11 then imports the same `MIGRATOR` for `init()`.

- [ ] **Step 1: Write the MIGRATOR module**

Create `crates/voom-store/src/migrator.rs`:

```rust
use sqlx::migrate::Migrator;

/// Embedded migration set. The single source of truth for "what schema does
/// this binary expect" — both `init()` (Task 11) and `probe_schema()`
/// (this task) read from here.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");
```

- [ ] **Step 2: Write the failing test**

Create `crates/voom-store/src/schema.rs`. The expected migration set — *and*
the per-migration checksums — are derived from the embedded `MIGRATOR` rather
than any hand-maintained constant. Drift in *either* the version set or the
stored checksum is flagged as `TooNew`, which is the strongest guard against
divergent schemas that happen to share a version number.

```rust
use std::collections::HashMap;

use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;

use crate::migrator::MIGRATOR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// `_sqlx_migrations` table absent.
    Uninitialized,
    /// Fewer migrations applied than this binary ships.
    Partial { applied: u32, expected: u32 },
    /// Exactly as many migrations applied as this binary ships AND every
    /// applied version is known to the embedded MIGRATOR.
    Current { migration_count: u32, schema_init_at: OffsetDateTime },
    /// At least one applied migration version is not in the embedded MIGRATOR
    /// — either a newer binary touched this DB or migrations were renumbered.
    /// Either way the current binary cannot reason about the schema and must
    /// refuse to operate.
    TooNew { applied: u32, expected: u32 },
}

/// Number of migrations this build ships, derived from the embedded MIGRATOR
/// at runtime. No hand-maintained constant — adding a `migrations/000N_*.sql`
/// file automatically bumps this without code changes.
#[must_use]
pub fn expected_migrations() -> u32 {
    u32::try_from(MIGRATOR.iter().count()).unwrap_or(u32::MAX)
}

/// Map of `version → checksum` for every migration this build ships. Both
/// the version *and* the checksum are validated against `_sqlx_migrations`
/// rows so a row with a known version but mutated SQL (same number, different
/// content) is still surfaced as drift.
fn embedded_versions() -> HashMap<i64, Vec<u8>> {
    MIGRATOR
        .iter()
        .map(|m| (m.version, m.checksum.to_vec()))
        .collect()
}

/// Inspect the schema without modifying it.
pub async fn probe_schema(pool: &SqlitePool) -> Result<SchemaState, VoomError> {
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| VoomError::Database(format!("probing for _sqlx_migrations failed: {e}")))?;

    if migrations_table_exists == 0 {
        return Ok(SchemaState::Uninitialized);
    }

    // Read ALL rows (not just success=1). A failed-but-recorded migration
    // attempt leaves a success=0 row that must NOT be ignored — otherwise
    // health can mis-report a half-applied DB as Current.
    let all_rows: Vec<(i64, Vec<u8>, bool)> = sqlx::query_as(
        "SELECT version, checksum, success FROM _sqlx_migrations",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| VoomError::Database(format!("reading _sqlx_migrations failed: {e}")))?;

    let expected = expected_migrations();
    let known = embedded_versions();

    let unknown_version_present = all_rows.iter().any(|(v, _, _)| !known.contains_key(v));
    let any_failed = all_rows.iter().any(|(_, _, success)| !success);
    let successful_count =
        u32::try_from(all_rows.iter().filter(|(_, _, s)| *s).count()).unwrap_or(u32::MAX);

    // Order matters:
    //   1. Unknown-version rows (success or not) → TooNew. A newer binary
    //      touched the DB and we don't understand its schema.
    //   2. Any failed row with only known versions → Partial. A previous
    //      migration attempt left the schema mid-flight.
    //   3. Checksum drift on a successful known row → TooNew.
    //   4. successful_count < expected → Partial.
    //   5. Else Current.
    if unknown_version_present {
        return Ok(SchemaState::TooNew { applied: successful_count, expected });
    }
    if any_failed {
        return Ok(SchemaState::Partial { applied: successful_count, expected });
    }

    let any_drift = all_rows.iter().any(|(version, checksum, _)| {
        known.get(version).is_some_and(|known_checksum| known_checksum.as_slice() != checksum.as_slice())
    });
    if any_drift {
        return Ok(SchemaState::TooNew { applied: successful_count, expected });
    }

    if successful_count < expected {
        return Ok(SchemaState::Partial { applied: successful_count, expected });
    }

    // successful_count == expected AND every (version, checksum) matches
    // → genuinely up to date. Read the schema_meta marker; failures here mean
    // the migration table was applied but the metadata table is missing or
    // corrupted — surface as Migration (DB_PARTIAL_SCHEMA), NOT Database
    // (DB_UNREACHABLE). The DB is reachable; its content is wrong.
    let init_at: String = sqlx::query_scalar(
        "SELECT value FROM schema_meta WHERE key = 'schema_init_at'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| {
        VoomError::Migration(format!(
            "schema_meta.schema_init_at is missing or unreadable (DB is reachable \
             but schema is corrupted): {e}"
        ))
    })?;

    let schema_init_at = OffsetDateTime::parse(&init_at, &time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| {
            VoomError::Migration(format!(
                "schema_meta.schema_init_at is malformed ({init_at:?}): {e}"
            ))
        })?;

    Ok(SchemaState::Current { migration_count: successful_count, schema_init_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::connect;

    /// SQL that creates an empty `_sqlx_migrations` table matching sqlx's
    /// schema. Tests use this to simulate post-init states without depending
    /// on Task 11's `init_on` (which doesn't exist yet at this checkpoint).
    const CREATE_MIGRATIONS_TABLE: &str = "\
        CREATE TABLE _sqlx_migrations ( \
            version BIGINT PRIMARY KEY, \
            description TEXT NOT NULL, \
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
            success BOOLEAN NOT NULL, \
            checksum BLOB NOT NULL, \
            execution_time BIGINT NOT NULL \
        )";

    #[tokio::test]
    async fn probe_returns_uninitialized_on_fresh_db() {
        let pool = connect("sqlite::memory:").await.unwrap();
        assert_eq!(probe_schema(&pool).await.unwrap(), SchemaState::Uninitialized);
    }

    #[tokio::test]
    async fn expected_migrations_matches_embedded_count() {
        // Sprint 0 ships exactly one migration; this guards against the count
        // drifting from the migrations/ directory.
        assert_eq!(expected_migrations(), 1);
    }

    #[tokio::test]
    async fn probe_returns_too_new_on_renumbered_migration_at_same_count() {
        // Pathological case: count matches expectation but the *version* is
        // not in the embedded MIGRATOR. Seed the migrations table by hand —
        // no dependency on `init_on` (which lands in Task 11).
        let pool = connect("sqlite::memory:").await.unwrap();
        sqlx::query(CREATE_MIGRATIONS_TABLE).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (42, 'renumbered', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = probe_schema(&pool).await.unwrap();
        match state {
            SchemaState::TooNew { applied, expected } => {
                assert_eq!(applied, expected, "count matches but version is unknown");
            }
            other => panic!("expected TooNew (version not in MIGRATOR), got {other:?}"),
        }
    }

    // Two additional drift tests — `probe_returns_too_new_when_extra_migration_present`
    // and `probe_returns_too_new_on_checksum_drift_at_known_version` — live in
    // Task 11 because they need a *legitimate* initial migration (real
    // checksum from MIGRATOR) before injecting drift, which is easiest to
    // arrange via `init_on`.
}
```

- [ ] **Step 2: Wire into lib.rs**

Update `crates/voom-store/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod migrator;
pub mod pool;
pub mod schema;

pub use migrator::MIGRATOR;
pub use pool::{connect, connect_or_create};
pub use schema::{SchemaState, expected_migrations, probe_schema};
```

Note: `EXPECTED_MIGRATIONS` (constant) is gone; callers use the `expected_migrations()` function instead. There is **no** hand-maintained migration count anywhere in the codebase.

- [ ] **Step 3: Run tests**

Run: `cargo test --package voom-store --lib schema`
Expected: 3 passed (uninitialized, expected_count, renumbered). Two additional drift tests live in Task 11 because they need `init_on` to seed a legitimate Current state before injecting drift.

The crate compiles and tests independently at this checkpoint — no Task 11 dependency.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-store/
git commit -m "voom-store: probe_schema validates per-version + checksum from embedded MIGRATOR"
```

---

## Task 11: `voom-store` — `init()` and `InitReport`

**Files:**
- Create: `crates/voom-store/src/init.rs`
- Modify: `crates/voom-store/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/voom-store/src/init.rs`:

```rust
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;

use crate::migrator::MIGRATOR;
use crate::pool::{connect, connect_or_create};
use crate::schema::{SchemaState, probe_schema};

// `connect` is imported (not just `connect_or_create`) because the unit tests
// below open in-memory pools via the read-side opener; without this import the
// bare `connect("sqlite::memory:")` calls in the test module fail to resolve.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub migrations_applied: u32,
    pub schema_init_at: OffsetDateTime,
    pub already_initialized: bool,
}

/// Open the pool (creating the database file and parent dirs if necessary) and
/// apply any pending migrations. Idempotent. This is the **only** production
/// entry point allowed to create filesystem state or mutate schema.
pub async fn init(url: &str) -> Result<InitReport, VoomError> {
    let pool = connect_or_create(url).await?;
    run_migrations_on(&pool).await
}

/// Run migrations on an already-open pool. **Test-only public surface** —
/// gated behind the `test-support` feature so production crates cannot reach
/// it. Use `init(url)` in production code; this exists solely so tests can
/// seed pre-init state and then inspect the resulting pool.
#[cfg(any(test, feature = "test-support"))]
pub async fn init_on(pool: &SqlitePool) -> Result<InitReport, VoomError> {
    run_migrations_on(pool).await
}

async fn run_migrations_on(pool: &SqlitePool) -> Result<InitReport, VoomError> {
    let before = probe_schema(pool).await?;

    // Defensive: never run migrations against a DB whose schema is ahead of
    // this binary. sqlx's migrator silently ignores unknown-version rows in
    // `_sqlx_migrations`, so without this check init would no-op and then
    // post-probe would surface TooNew via the fallthrough below — but with a
    // confusing "post-init schema state is not Current" message. Bail early
    // with an actionable error instead.
    if let SchemaState::TooNew { applied, expected } = before {
        return Err(VoomError::SchemaTooNew(format!(
            "cannot init: database has {applied} migrations applied but this binary ships \
             {expected}; upgrade the voom binary or roll back the database"
        )));
    }

    // Capture the applied count from the pre-init state so we can report the
    // *delta* (after - before), not the total. Otherwise resuming a partial
    // migration would claim to have applied every migration including the
    // ones already present.
    let before_count: u32 = match &before {
        SchemaState::Uninitialized => 0,
        SchemaState::Partial { applied, .. } => *applied,
        SchemaState::Current { migration_count, .. } => *migration_count,
        // TooNew bailed above; pattern is here only for exhaustiveness.
        SchemaState::TooNew { applied, .. } => *applied,
    };
    let already_initialized = matches!(before, SchemaState::Current { .. });

    MIGRATOR.run(pool).await.map_err(|e| {
        VoomError::Migration(format!("running migrations failed: {e}"))
    })?;

    let after = probe_schema(pool).await?;
    let SchemaState::Current { migration_count, schema_init_at } = after else {
        return Err(VoomError::Migration(format!(
            "post-init schema state is not Current: {after:?}"
        )));
    };

    let migrations_applied = migration_count.saturating_sub(before_count);

    Ok(InitReport { migrations_applied, schema_init_at, already_initialized })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{expected_migrations, probe_schema};

    #[tokio::test]
    async fn init_in_memory_applies_one_migration() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let report = init_on(&pool).await.unwrap();
        assert!(!report.already_initialized);
        assert_eq!(report.migrations_applied, 1);
    }

    #[tokio::test]
    async fn init_is_idempotent_on_same_pool() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let first = init_on(&pool).await.unwrap();
        let second = init_on(&pool).await.unwrap();
        assert!(!first.already_initialized);
        assert!(second.already_initialized);
        assert_eq!(second.migrations_applied, 0);
    }

    #[tokio::test]
    async fn init_refuses_when_db_schema_is_too_new() {
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        // Make the DB look like a newer binary already migrated it.
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let err = init_on(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
        assert!(format!("{err}").contains("cannot init"));
    }

    #[tokio::test]
    async fn probe_after_init_then_extra_row_returns_too_new() {
        // Drift case 1: legitimate init produced the canonical row; an extra
        // future-version row pushes the schema past what this binary knows.
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::TooNew { applied, expected } => {
                assert_eq!(expected, expected_migrations());
                assert!(applied > expected);
            }
            other => panic!("expected TooNew, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_after_init_then_checksum_mutation_returns_too_new() {
        // Drift case 2: same version + same count, but the on-disk checksum
        // diverges from the embedded MIGRATOR's recorded checksum. probe
        // must surface this as TooNew (not Current).
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();
        assert!(matches!(probe_schema(&pool).await.unwrap(), SchemaState::Current { .. }));

        sqlx::query("UPDATE _sqlx_migrations SET checksum = X'DEADBEEF' WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::TooNew { applied, expected } => {
                assert_eq!(applied, expected, "count unchanged; only checksum differs");
            }
            other => panic!("expected TooNew (checksum drift), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_returns_partial_when_known_version_row_marked_failed() {
        // A prior migration attempt left a success=0 row for a known version.
        // probe must NOT skip it (filtering by success=1 would hide partial
        // schemas); surface as Partial.
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::Partial { applied, expected } => {
                assert_eq!(applied, 0, "no successful migrations remain");
                assert_eq!(expected, expected_migrations());
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_returns_too_new_when_failed_unknown_version_row_present() {
        // Failed attempt at a future migration: a newer binary tried to
        // migrate and aborted. The schema is in an unclear forward state →
        // TooNew rather than Partial, so we refuse to operate.
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'failed-future', strftime('%s','now'), 0, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        match probe_schema(&pool).await.unwrap() {
            SchemaState::TooNew { applied, .. } => {
                assert_eq!(applied, expected_migrations(), "only successful row counts");
            }
            other => panic!("expected TooNew, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_after_init_then_corrupt_schema_meta_returns_migration_error() {
        // Migration table looks current, but schema_meta is missing — the DB
        // is reachable yet the schema is corrupt. probe must surface this as
        // a Migration error (DB_PARTIAL_SCHEMA), not Database (DB_UNREACHABLE),
        // so operators get an actionable recovery hint instead of thinking
        // their filesystem is broken.
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query("DROP TABLE schema_meta")
            .execute(&pool)
            .await
            .unwrap();

        let err = probe_schema(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[tokio::test]
    async fn probe_after_init_then_corrupt_schema_init_at_value_returns_migration_error() {
        // Same idea, but the row exists with a non-ISO8601 value.
        let pool = connect("sqlite::memory:").await.unwrap();
        init_on(&pool).await.unwrap();

        sqlx::query("UPDATE schema_meta SET value = 'not-a-timestamp' WHERE key = 'schema_init_at'")
            .execute(&pool)
            .await
            .unwrap();

        let err = probe_schema(&pool).await.unwrap_err();
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[tokio::test]
    async fn init_from_partial_state_reports_delta_not_total() {
        // Synthesize a Partial state: _sqlx_migrations exists but has zero
        // success rows. (In Sprint 0 we ship only one migration, so this is
        // degenerate — the delta equals the total. But the test pins the
        // delta-counting code path so Sprint 1+ can add a real partial case
        // without rewriting init logic.)
        let pool = connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations ( \
             version BIGINT PRIMARY KEY, \
             description TEXT NOT NULL, \
             installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             success BOOLEAN NOT NULL, \
             checksum BLOB NOT NULL, \
             execution_time BIGINT NOT NULL \
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let report = init_on(&pool).await.unwrap();
        assert!(!report.already_initialized);
        // before_count = 0, after = 1, delta = 1.
        assert_eq!(report.migrations_applied, 1);
    }
}
```

- [ ] **Step 2: Wire into lib.rs**

Update `crates/voom-store/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod init;
pub mod migrator;
pub mod pool;
pub mod schema;

pub use init::{InitReport, init};
pub use migrator::MIGRATOR;
pub use pool::{connect, connect_or_create};
pub use schema::{SchemaState, expected_migrations, probe_schema};

// `init_on` is deliberately NOT re-exported. It lives at
// `voom_store::init::init_on` and is gated behind the `test-support` feature
// so production crates cannot reach the pool-injection migration path.
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package voom-store --lib --features test-support`
Expected: lib tests = 13 passed total — schema's 3 (Task 10) + init's 10 from this task (3 baseline + 2 drift + 2 failed-row + 2 corruption + 1 partial-delta). The `--features test-support` flag exposes `init_on` for the lib's own unit tests; `just test` in the justfile uses `--all-features` so this happens automatically.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-store/
git commit -m "voom-store: add idempotent init() with InitReport"
```

---

## Task 12: `voom-store` — Integration tests

**Files:**
- Create: `crates/voom-store/tests/init.rs`
- Create: `crates/voom-store/tests/health_no_migrate.rs`

- [ ] **Step 1: Write `init.rs` integration test**

Create `crates/voom-store/tests/init.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_store::{SchemaState, connect, init, probe_schema};

// Integration tests use the disk-backed public `init(url)` exclusively.
// The :memory: + init_on path is covered by Task 11's lib-internal unit tests.
// init_on is not re-exported from voom-store and is gated behind test-support.

#[tokio::test]
async fn init_on_disk_creates_schema_meta() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

    let report = init(&url).await.unwrap();
    assert!(!report.already_initialized);

    let pool = connect(&url).await.unwrap();
    let state = probe_schema(&pool).await.unwrap();
    let SchemaState::Current { migration_count, .. } = state else {
        panic!("expected Current, got {state:?}");
    };
    assert_eq!(migration_count, 1);
}

#[tokio::test]
async fn second_init_against_same_disk_db_is_noop() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

    let first = init(&url).await.unwrap();
    let second = init(&url).await.unwrap();

    assert!(!first.already_initialized);
    assert!(second.already_initialized);
    assert_eq!(second.migrations_applied, 0);
    assert_eq!(first.schema_init_at, second.schema_init_at);
}
```

- [ ] **Step 2: Write `health_no_migrate.rs` integration test**

Create `crates/voom-store/tests/health_no_migrate.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use voom_store::{SchemaState, connect, probe_schema};

/// `connect()` and `probe_schema()` must NEVER create the migration tracking
/// table. This is the contract that makes `voom health` safe to run against
/// a DB the operator hasn't yet initialized.
#[tokio::test]
async fn connect_then_probe_leaves_db_uninitialized() {
    let pool = connect("sqlite::memory:").await.unwrap();
    assert_eq!(probe_schema(&pool).await.unwrap(), SchemaState::Uninitialized);

    // Re-probe; still uninitialized.
    assert_eq!(probe_schema(&pool).await.unwrap(), SchemaState::Uninitialized);

    // Direct inspection: _sqlx_migrations table must not exist.
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(exists, 0, "read-side calls must not create migration table");
}
```

Add the `sqlx` dev-dep for this test:

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-store --dev sqlx --features runtime-tokio,sqlite
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test --package voom-store --test init --test health_no_migrate`
Expected: 3 passed (init_on_disk, second_init_noop, health_no_migrate; the dropped :memory: variant is covered by Task 11 unit tests).

- [ ] **Step 4: Commit**

```bash
git add crates/voom-store/ Cargo.toml Cargo.lock
git commit -m "voom-store: add init and health-no-migrate integration tests"
```

---

## Task 13: `voom-store` — `SchemaMetaRepo`

**Files:**
- Create: `crates/voom-store/src/repo/mod.rs`
- Create: `crates/voom-store/src/repo/schema_meta.rs`
- Create: `crates/voom-store/tests/repo_roundtrip.rs`
- Modify: `crates/voom-store/src/lib.rs`

- [ ] **Step 1: Write the repo module skeleton**

Create `crates/voom-store/src/repo/mod.rs`:

```rust
//! Repository pattern: trait per storage area, Sqlite impl per trait.

pub mod schema_meta;

pub use schema_meta::{SchemaMetaRepo, SqliteSchemaMetaRepo};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
```

- [ ] **Step 2: Write `SchemaMetaRepo`**

Create `crates/voom-store/src/repo/schema_meta.rs`:

```rust
use async_trait::async_trait;
use sqlx::SqlitePool;
use voom_core::VoomError;

use super::Repository;

#[async_trait]
pub trait SchemaMetaRepo: Repository {
    async fn get(&self, key: &str) -> Result<Option<String>, VoomError>;
    async fn set(&self, key: &str, value: &str) -> Result<(), VoomError>;
}

pub struct SqliteSchemaMetaRepo {
    pool: SqlitePool,
}

impl SqliteSchemaMetaRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSchemaMetaRepo {}

#[async_trait]
impl SchemaMetaRepo for SqliteSchemaMetaRepo {
    async fn get(&self, key: &str) -> Result<Option<String>, VoomError> {
        sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("schema_meta get({key:?}) failed: {e}")))
    }

    async fn set(&self, key: &str, value: &str) -> Result<(), VoomError> {
        sqlx::query(
            "INSERT INTO schema_meta (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|e| VoomError::Database(format!("schema_meta set({key:?}) failed: {e}")))
    }
}
```

- [ ] **Step 3: Write the round-trip integration test**

Create `crates/voom-store/tests/repo_roundtrip.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_store::repo::{SchemaMetaRepo, SqliteSchemaMetaRepo};
use voom_store::{connect, init};

async fn fresh_initialized_pool() -> (NamedTempFile, sqlx::SqlitePool) {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    init(&url).await.unwrap();
    let pool = connect(&url).await.unwrap();
    (tmp, pool)
}

#[tokio::test]
async fn set_then_get_returns_value() {
    let (_keep, pool) = fresh_initialized_pool().await;
    let repo = SqliteSchemaMetaRepo::new(pool);
    repo.set("hello", "world").await.unwrap();
    assert_eq!(repo.get("hello").await.unwrap().as_deref(), Some("world"));
}

#[tokio::test]
async fn get_missing_key_returns_none() {
    let (_keep, pool) = fresh_initialized_pool().await;
    let repo = SqliteSchemaMetaRepo::new(pool);
    assert!(repo.get("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn set_twice_overwrites() {
    let (_keep, pool) = fresh_initialized_pool().await;
    let repo = SqliteSchemaMetaRepo::new(pool);
    repo.set("k", "v1").await.unwrap();
    repo.set("k", "v2").await.unwrap();
    assert_eq!(repo.get("k").await.unwrap().as_deref(), Some("v2"));
}
```

The helper takes a `NamedTempFile` ownership and returns it alongside the pool so the temp file isn't dropped (and the on-disk DB deleted) before the test finishes.

Also add `sqlx` as a dev-dep so the helper's `sqlx::SqlitePool` return type resolves:

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-store --dev sqlx --features runtime-tokio,sqlite
```

- [ ] **Step 4: Wire into lib.rs**

Update `crates/voom-store/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Storage layer: SQLite pool, migrations, repositories.

pub mod init;
pub mod migrator;
pub mod pool;
pub mod repo;
pub mod schema;

pub use init::{InitReport, init};
pub use migrator::MIGRATOR;
pub use pool::{connect, connect_or_create};
pub use schema::{SchemaState, expected_migrations, probe_schema};
```

- [ ] **Step 5: Run all voom-store tests**

Run: `cargo test --package voom-store`
Expected: all green (lib + 3 integration suites, ~10 tests total).

- [ ] **Step 6: Commit**

```bash
git add crates/voom-store/
git commit -m "voom-store: add SchemaMetaRepo trait, SqliteSchemaMetaRepo, and round-trip tests"
```

---

## Task 14: `voom-control-plane` — `ControlPlane` handle

**Files:**
- Modify: `crates/voom-control-plane/Cargo.toml`
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Add dependencies**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-control-plane --path crates/voom-core
cargo add --package voom-control-plane --path crates/voom-store
cargo add --package voom-control-plane sqlx --features runtime-tokio,sqlite
cargo add --package voom-control-plane tokio --features rt-multi-thread,sync
cargo add --package voom-control-plane time --features serde,formatting
cargo add --package voom-control-plane serde --features derive
cargo add --package voom-control-plane --dev tokio --features rt-multi-thread,macros
cargo add --package voom-control-plane --dev tempfile
```

`sqlx` is a direct dep because `ControlPlane` exposes a `SqlitePool` in its tests and the `TooNew` test injects via `sqlx::query`. Cargo doesn't allow naming a transitive crate.

- [ ] **Step 2: Write the failing test**

Replace `crates/voom-control-plane/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! App-services layer: wraps voom-store and exposes commands consumed by API/CLI.

use serde::Serialize;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_store::{SchemaState, connect, probe_schema};

#[derive(Debug, Clone)]
pub struct ControlPlane {
    pool: SqlitePool,
    database_url: String,
}

impl ControlPlane {
    /// Open an existing database. **Never creates files or directories** — if
    /// the DB doesn't exist, returns `DB_UNREACHABLE`. The CLI's `init` command
    /// is the only path that creates databases, and it calls
    /// `voom_store::init(url)` directly without going through `ControlPlane`.
    pub async fn open(database_url: String) -> Result<Self, VoomError> {
        let pool = connect(&database_url).await?;
        Ok(Self { pool, database_url })
    }

    #[must_use]
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Read-only health snapshot.
    pub async fn health(&self) -> Result<HealthSnapshot, VoomError> {
        let schema = probe_schema(&self.pool).await?;
        let (db_status, schema_init_at, migration_count, expected) = match schema {
            SchemaState::Uninitialized => (DbStatus::Uninitialized, None, None, None),
            SchemaState::Partial { applied, expected } => {
                (DbStatus::Partial, None, Some(applied), Some(expected))
            }
            SchemaState::Current { migration_count, schema_init_at } => (
                DbStatus::Current,
                Some(schema_init_at),
                Some(migration_count),
                None,
            ),
            SchemaState::TooNew { applied, expected } => {
                (DbStatus::TooNew, None, Some(applied), Some(expected))
            }
        };
        Ok(HealthSnapshot { db_status, schema_init_at, migration_count, expected_migrations: expected })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DbStatus {
    Uninitialized,
    Partial,
    Current,
    TooNew,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthSnapshot {
    pub db_status: DbStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_init_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_count: Option<u32>,
    /// Present whenever `db_status` is Partial or TooNew; otherwise None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_migrations: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test uses a unique tempfile-backed disk DB so the no-create
    /// `ControlPlane::open` has something to attach to. `voom_store::init` is
    /// called explicitly from each test that needs an initialized DB; there is
    /// no `cp.init()` shortcut by design (separating read- and write-side
    /// pool opens).
    fn fresh_url() -> (tempfile::NamedTempFile, String) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        (tmp, url)
    }

    #[tokio::test]
    async fn open_refuses_missing_database() {
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", tmp.path().join("nope.db").display());
        let err = ControlPlane::open(url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
    }

    #[tokio::test]
    async fn health_on_existing_but_uninitialized_db_is_uninitialized() {
        let (_keep, url) = fresh_url();
        // Create the DB (empty schema) via connect_or_create, then open via the
        // read-side path so the no-create rule isn't violated.
        voom_store::connect_or_create(&url).await.unwrap();

        let cp = ControlPlane::open(url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap.db_status, DbStatus::Uninitialized);
        assert!(snap.schema_init_at.is_none());
        assert!(snap.migration_count.is_none());
    }

    #[tokio::test]
    async fn init_then_health_reports_current() {
        let (_keep, url) = fresh_url();
        let report = voom_store::init(&url).await.unwrap();
        assert!(!report.already_initialized);

        let cp = ControlPlane::open(url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap.db_status, DbStatus::Current);
        assert_eq!(snap.migration_count, Some(1));
        assert!(snap.schema_init_at.is_some());
    }

    #[tokio::test]
    async fn second_init_returns_already_initialized() {
        let (_keep, url) = fresh_url();
        voom_store::init(&url).await.unwrap();
        let second = voom_store::init(&url).await.unwrap();
        assert!(second.already_initialized);
        assert_eq!(second.migrations_applied, 0);
    }

    #[tokio::test]
    async fn health_maps_too_new_state() {
        let (_keep, url) = fresh_url();
        voom_store::init(&url).await.unwrap();

        // Inject a synthetic future migration row via a sibling no-create pool
        // — the on-disk DB already exists, so connect() suffices.
        {
            let pool = voom_store::connect(&url).await.unwrap();
            sqlx::query(
                "INSERT INTO _sqlx_migrations \
                 (version, description, installed_on, success, checksum, execution_time) \
                 VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let cp = ControlPlane::open(url).await.unwrap();
        let snap = cp.health().await.unwrap();
        assert_eq!(snap.db_status, DbStatus::TooNew);
        assert!(snap.migration_count.unwrap() > snap.expected_migrations.unwrap());
        assert!(snap.schema_init_at.is_none());
    }
}
```

The tests use disk-backed temp DBs because the no-create `ControlPlane::open` would fail against a fresh `:memory:` URL — `:memory:` is shared-cache only within a single pool, so a sibling no-create pool against the same `:memory:` URL sees an empty DB. Disk URLs sidestep that and exercise the real read-side contract.

- [ ] **Step 3: Run tests**

Run: `cargo test --package voom-control-plane`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-control-plane/ Cargo.toml Cargo.lock
git commit -m "voom-control-plane: add ControlPlane with open/init/health"
```

---

## Task 15: `voom-cli` — `build.rs` for git SHA

**Files:**
- Create: `crates/voom-cli/build.rs`

- [ ] **Step 1: Write the build script**

Create `crates/voom-cli/build.rs`. The provenance trail (SHA + dirty) must be
robust against cargo's incremental cache and against CI builds from source
tarballs. Three robustness measures vs. a naive impl:

1. **Prefer `GITHUB_SHA` when set** — CI sets it, and trusting the env var
   avoids cache-missed re-runs of `git rev-parse`.
2. **Watch the actual ref file, not just `.git/HEAD`** — `HEAD` is usually a
   symbolic-ref like `ref: refs/heads/main`; the SHA changes in
   `.git/refs/heads/main` (or `packed-refs` after `git gc`).
3. **`git status --porcelain` for dirty** — `git diff --quiet HEAD` misses
   untracked files, which absolutely count for provenance.

```rust
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_REF");

    let git_root = env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map(|p| p.join("../..").join(".git"))
        .unwrap_or_else(|_| PathBuf::from("../../.git"));

    // Watch HEAD and packed-refs unconditionally.
    println!("cargo:rerun-if-changed={}", git_root.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_root.join("packed-refs").display());

    // If HEAD is a symbolic ref, also watch the file backing the current branch.
    if let Ok(out) = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
    {
        if out.status.success() {
            let r = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            println!("cargo:rerun-if-changed={}", git_root.join(&r).display());
        }
    }

    // SHA: prefer CI-provided env, fall back to `git rev-parse`.
    let sha = env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.chars().take(7).collect::<String>())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());

    // Dirty: tracked-file mods AND untracked files both count.
    // On CI without a working git, default to clean.
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=VOOM_GIT_SHA={sha}");
    println!("cargo:rustc-env=VOOM_GIT_DIRTY={}", if dirty { "true" } else { "false" });
}
```

A documented smoke check (added to `docs/release-process.md` Step 4): build the
binary, commit an empty change (`git commit --allow-empty`), build again, run
`voom version`, and confirm the reported SHA advanced. Run this once per
release-candidate cut to catch any future build-script regression.

- [ ] **Step 2: Test by building**

Run: `cargo build --package voom-cli`
Expected: builds cleanly.

Confirm the env vars are present:

```bash
cargo build --package voom-cli 2>&1 | rg "VOOM_GIT_SHA" || echo "no leak"
```

Expected: "no leak" (cargo doesn't echo `cargo:` directives by default — that's correct).

- [ ] **Step 3: Commit**

```bash
git add crates/voom-cli/build.rs
git commit -m "voom-cli: build.rs emits VOOM_GIT_SHA and VOOM_GIT_DIRTY"
```

---

## Task 16: `voom-cli` — Envelope writer

**Files:**
- Modify: `crates/voom-cli/Cargo.toml`
- Create: `crates/voom-cli/src/envelope.rs`

- [ ] **Step 1: Add dependencies**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-cli --path crates/voom-core
cargo add --package voom-cli --path crates/voom-store
cargo add --package voom-cli --path crates/voom-control-plane
cargo add --package voom-cli clap --features derive,env
cargo add --package voom-cli tokio --features rt-multi-thread,macros,fs
cargo add --package voom-cli serde --features derive
cargo add --package voom-cli serde_json
cargo add --package voom-cli anyhow
cargo add --package voom-cli tracing
cargo add --package voom-cli tracing-subscriber --features env-filter,json,fmt
cargo add --package voom-cli time --features serde,formatting
cargo add --package voom-cli --dev insta --features json
cargo add --package voom-cli --dev tempfile
cargo add --package voom-cli --dev tokio --features rt-multi-thread,macros
cargo add --package voom-cli --dev voom-store --path crates/voom-store
```

- [ ] **Step 2: Write the failing test**

Create `crates/voom-cli/src/envelope.rs`:

```rust
use std::io::{self, Write};

use serde::Serialize;

pub const SCHEMA_VERSION: &str = "0";

/// Host-only diagnostics block; emitted by CLI, never by API.
#[derive(Debug, Clone, Serialize)]
pub struct Local {
    pub db_url: String,
    pub config_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub schema_version: &'static str,
    pub command: &'static str,
    pub status: Status,
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<Local>,
    pub warnings: Vec<String>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Error,
}

/// Emit a successful envelope as a single JSON object to stdout, followed by a newline.
pub fn emit_ok<T: Serialize>(
    command: &'static str,
    data: T,
    local: Option<Local>,
    warnings: Vec<String>,
) -> io::Result<()> {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Ok,
        data: Some(data),
        local,
        warnings,
        error: None,
    };
    write_json(&env)
}

/// Emit an error envelope to stdout. Returns the suggested process exit code.
pub fn emit_err(
    command: &'static str,
    code: &'static str,
    message: String,
    hint: Option<String>,
    local: Option<Local>,
) -> io::Result<()> {
    let env: Envelope<()> = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Error,
        data: None,
        local,
        warnings: Vec::new(),
        error: Some(ErrorBody { code, message, hint }),
    };
    write_json(&env)
}

#[expect(
    clippy::print_stdout,
    reason = "envelope writer is the one place CLI output is allowed to reach stdout"
)]
fn write_json<T: Serialize>(value: &T) -> io::Result<()> {
    let s = serde_json::to_string(value)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let mut out = io::stdout().lock();
    writeln!(out, "{s}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Hello {
        msg: &'static str,
    }

    #[test]
    fn ok_envelope_includes_status_ok() {
        let env = Envelope {
            schema_version: SCHEMA_VERSION,
            command: "test",
            status: Status::Ok,
            data: Some(Hello { msg: "hi" }),
            local: None,
            warnings: Vec::new(),
            error: None,
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["data"]["msg"], "hi");
        assert!(json.get("local").is_none());
    }

    #[test]
    fn local_block_serializes_when_present() {
        let env = Envelope::<()> {
            schema_version: SCHEMA_VERSION,
            command: "test",
            status: Status::Ok,
            data: None,
            local: Some(Local {
                db_url: "sqlite::memory:".into(),
                config_path: "/etc/voom".into(),
            }),
            warnings: Vec::new(),
            error: None,
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["local"]["db_url"], "sqlite::memory:");
    }

    #[test]
    fn error_envelope_omits_data() {
        let env: Envelope<()> = Envelope {
            schema_version: SCHEMA_VERSION,
            command: "test",
            status: Status::Error,
            data: None,
            local: None,
            warnings: Vec::new(),
            error: Some(ErrorBody {
                code: "DB_UNREACHABLE",
                message: "boom".into(),
                hint: None,
            }),
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["status"], "error");
        assert!(json["data"].is_null());
        assert_eq!(json["error"]["code"], "DB_UNREACHABLE");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package voom-cli --lib envelope`

Note: the test won't run yet because `lib.rs` doesn't exist on a binary crate. Convert the envelope module to be reachable via `main.rs`'s `mod envelope;` or expose a `lib.rs` alongside `main.rs`. The plan uses `lib.rs` + `main.rs` so tests can live in the lib target.

Adjust `crates/voom-cli/Cargo.toml`:

```toml
[lib]
name = "voom_cli"
path = "src/lib.rs"

[[bin]]
name = "voom"
path = "src/main.rs"
```

Create `crates/voom-cli/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Internal library exposing CLI plumbing to integration tests.

pub mod envelope;
```

Run: `cargo test --package voom-cli --lib envelope`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/ Cargo.toml Cargo.lock
git commit -m "voom-cli: add envelope writer with data/local split and status enum"
```

---

## Task 17: `voom-cli` — Logging setup

**Files:**
- Create: `crates/voom-cli/src/logging.rs`
- Modify: `crates/voom-cli/src/lib.rs`

- [ ] **Step 1: Write the logging module**

Create `crates/voom-cli/src/logging.rs`:

```rust
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::{EnvFilter, fmt};
use voom_core::LogFormat;

/// Install the global tracing subscriber. Writes to stderr so it never collides
/// with the envelope on stdout.
pub fn init(level: &str, format: LogFormat) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let writer = std::io::stderr.with_max_level(tracing::Level::TRACE);

    match format {
        LogFormat::Json => {
            fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .json()
                .with_current_span(false)
                .init();
        }
        LogFormat::Text => {
            fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_target(false)
                .init();
        }
    }
}
```

- [ ] **Step 2: Wire into lib.rs**

Update `crates/voom-cli/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Internal library exposing CLI plumbing to integration tests.

pub mod envelope;
pub mod logging;
```

- [ ] **Step 3: Build**

Run: `cargo build --package voom-cli`
Expected: builds cleanly.

(No unit test — `tracing_subscriber::init` mutates global state and is awkward to test in isolation; behavior is verified by the snapshot tests in later tasks.)

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/
git commit -m "voom-cli: add logging::init writing to stderr in text or json"
```

---

## Task 18: `voom-cli` — clap top-level

**Files:**
- Create: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/lib.rs`

- [ ] **Step 1: Write the CLI struct**

Create `crates/voom-cli/src/cli.rs`:

```rust
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "voom", about = "VOOM control plane CLI", long_about = None)]
pub struct Cli {
    /// Override the database URL (default: XDG data dir).
    #[arg(long, env = "VOOM_DATABASE_URL", global = true)]
    pub database_url: Option<String>,

    /// Log level (error|warn|info|debug|trace).
    #[arg(long, default_value = "info", global = true, env = "VOOM_LOG_LEVEL")]
    pub log_level: String,

    /// Log format on stderr (text|json). Defaults to json so logs and command
    /// output are both machine-parseable.
    #[arg(long, value_enum, default_value_t = LogFormatArg::Json, global = true, env = "VOOM_LOG_FORMAT")]
    pub log_format: LogFormatArg,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print build version, semver, git SHA, and dirty flag.
    Version,
    /// Report database health without applying migrations.
    Health,
    /// Apply pending migrations idempotently.
    Init,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum LogFormatArg {
    Text,
    Json,
}

impl LogFormatArg {
    #[must_use]
    pub fn to_core(self) -> voom_core::LogFormat {
        match self {
            Self::Text => voom_core::LogFormat::Text,
            Self::Json => voom_core::LogFormat::Json,
        }
    }
}
```

**No `--format` flag in Sprint 0.** Command output is always the JSON envelope (the spec's agent-friendly mandate). A future sprint that ships human-readable plain output will add `--format` and implement it in every command emitter — at that point it stops being phantom. Until then, removing the flag is honest.

- [ ] **Step 2: Wire into lib.rs**

Update `crates/voom-cli/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Internal library exposing CLI plumbing to integration tests.

pub mod cli;
pub mod envelope;
pub mod logging;
```

- [ ] **Step 3: Build**

Run: `cargo build --package voom-cli`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/
git commit -m "voom-cli: add clap Cli struct with global flags and subcommand enum"
```

---

## Task 19: `voom-cli` — `version` subcommand

**Files:**
- Create: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/version.rs`
- Modify: `crates/voom-cli/src/lib.rs`
- Create: `crates/voom-cli/tests/version_envelope.rs`

- [ ] **Step 1: Write the failing snapshot test**

Create `crates/voom-cli/tests/version_envelope.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use serde_json::Value;
use voom_cli::commands::version::build_version_info;

#[test]
fn version_envelope_shape() {
    let info = build_version_info("0.1.0-dev", "abc1234", false, "debug");

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!("version_dev", &info);
    });
}

#[test]
fn release_flag_is_true_only_when_no_prerelease() {
    let dev = build_version_info("0.1.0-dev", "abc1234", false, "debug");
    let rel = build_version_info("0.1.0", "def5678", false, "release");
    let dirty = build_version_info("0.1.0-dev", "abc1234", true, "debug");

    assert!(!dev.release);
    assert!(rel.release);
    assert!(dirty.version.ends_with(".dirty"));
}

#[test]
fn version_envelope_serializes_as_expected_keys() {
    let info = build_version_info("0.1.0-dev", "abc1234", false, "debug");
    let json: Value = serde_json::to_value(&info).unwrap();
    let obj = json.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["build_profile", "dirty", "git_sha", "release", "semver", "version"]
    );
}
```

- [ ] **Step 2: Write the command implementation**

Create `crates/voom-cli/src/commands/mod.rs`:

```rust
pub mod version;
```

Create `crates/voom-cli/src/commands/version.rs`:

```rust
use std::io;

use voom_core::VersionInfo;

use crate::envelope::emit_ok;

#[must_use]
pub fn build_version_info(
    semver: &str,
    git_sha: &str,
    dirty: bool,
    build_profile: &str,
) -> VersionInfo {
    VersionInfo::new(semver, git_sha, dirty, build_profile)
}

pub fn run() -> io::Result<()> {
    let semver = env!("CARGO_PKG_VERSION");
    let sha = env!("VOOM_GIT_SHA");
    let dirty = matches!(env!("VOOM_GIT_DIRTY"), "true");
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let info = build_version_info(semver, sha, dirty, profile);
    emit_ok("version", info, None, Vec::new())
}
```

Update `crates/voom-cli/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! Internal library exposing CLI plumbing to integration tests.

pub mod cli;
pub mod commands;
pub mod envelope;
pub mod logging;
```

- [ ] **Step 3: Run the snapshot test (accept on first run)**

Run: `cargo test --package voom-cli --test version_envelope`

The first run creates pending snapshots. Accept them:

```bash
cargo insta accept --workspace
```

Then re-run:

```bash
cargo test --package voom-cli --test version_envelope
```

Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/
git commit -m "voom-cli: add version subcommand and envelope snapshot tests"
```

---

## Task 20: `voom-cli` — `health` and `init` subcommands + `main`

**Files:**
- Create: `crates/voom-cli/src/commands/health.rs`
- Create: `crates/voom-cli/src/commands/init.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Modify: `crates/voom-cli/src/main.rs`

- [ ] **Step 1: Health command**

Create `crates/voom-cli/src/commands/health.rs`:

```rust
use std::io;

use serde::Serialize;
use serde_json::json;
use voom_control_plane::{ControlPlane, DbStatus, HealthSnapshot};

use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Serialize)]
pub struct HealthData {
    pub db: HealthDb,
    pub runtime: HealthRuntime,
}

#[derive(Serialize)]
pub struct HealthDb {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_init_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_count: Option<u32>,
}

#[derive(Serialize)]
pub struct HealthRuntime {
    pub tokio_workers: usize,
}

pub async fn run(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.health().await {
        Ok(snap) => emit_snapshot(&snap, local),
        Err(err) => {
            emit_err("health", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}

fn emit_snapshot(snap: &HealthSnapshot, local: Local) -> io::Result<i32> {
    // Read-side never advances schema; surface uninitialized/partial/too-new as errors.
    let status_str = match snap.db_status {
        DbStatus::Uninitialized => {
            emit_err(
                "health",
                "DB_UNINITIALIZED",
                "database has no migrations applied".into(),
                Some("Run: voom init".into()),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::Partial => {
            let detail = json!({
                "applied": snap.migration_count,
                "expected": snap.expected_migrations,
            });
            emit_err(
                "health",
                "DB_PARTIAL_SCHEMA",
                format!("database partially migrated: {detail}"),
                Some("Run: voom init against the current binary".into()),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::TooNew => {
            let detail = json!({
                "applied": snap.migration_count,
                "expected": snap.expected_migrations,
            });
            emit_err(
                "health",
                "DB_SCHEMA_TOO_NEW",
                format!(
                    "database has migrations this binary does not know about: {detail}; \
                     refusing to operate against unknown schema"
                ),
                Some("Use a newer voom binary or roll the database back to a known migration".into()),
                Some(local),
            )?;
            return Ok(2);
        }
        DbStatus::Current => "current",
    };

    let data = HealthData {
        db: HealthDb {
            status: status_str,
            schema_init_at: snap.schema_init_at.map(|t| {
                t.format(&time::format_description::well_known::Iso8601::DEFAULT)
                    .unwrap_or_else(|_| t.unix_timestamp().to_string())
            }),
            migration_count: snap.migration_count,
        },
        runtime: HealthRuntime {
            tokio_workers: std::thread::available_parallelism()
                .map_or(1, std::num::NonZero::get),
        },
    };
    emit_ok("health", data, Some(local), Vec::new())?;
    Ok(0)
}
```

- [ ] **Step 2: Init command**

The `init` command bypasses `ControlPlane` (which is read-only) and calls
`voom_store::init(url)` directly. This is the only voom-cli code path
authorized to mutate filesystem state.

Create `crates/voom-cli/src/commands/init.rs`:

```rust
use std::io;

use serde::Serialize;

use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Serialize)]
pub struct InitData {
    pub migrations_applied: u32,
    pub schema_init_at: String,
    pub already_initialized: bool,
}

pub async fn run(database_url: &str, local: Local) -> io::Result<i32> {
    match voom_store::init(database_url).await {
        Ok(report) => {
            let data = InitData {
                migrations_applied: report.migrations_applied,
                schema_init_at: report
                    .schema_init_at
                    .format(&time::format_description::well_known::Iso8601::DEFAULT)
                    .unwrap_or_else(|_| report.schema_init_at.unix_timestamp().to_string()),
                already_initialized: report.already_initialized,
            };
            emit_ok("init", data, Some(local), Vec::new()).map(|()| 0)
        }
        Err(err) => {
            emit_err("init", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}
```

- [ ] **Step 3: Update commands/mod.rs**

```rust
pub mod health;
pub mod init;
pub mod version;
```

- [ ] **Step 4: Rewrite `main.rs`**

Replace `crates/voom-cli/src/main.rs`:

```rust
//! `voom` CLI entrypoint. Tests live in the sibling `voom_cli` library crate.
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use voom_cli::cli::{Cli, Command};
use voom_cli::commands::{health, init, version};
use voom_cli::envelope::{Local, emit_err};
use voom_cli::logging;
use voom_control_plane::ControlPlane;
use voom_core::Config;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    // Use try_parse so clap errors flow through the JSON envelope writer
    // instead of clap's own stderr exit path — agents reading stdout must
    // never see a non-JSON line.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let kind = e.kind();
            // --help/--version use clap's success-exit path; let it through
            // verbatim because there's no JSON envelope yet for those.
            if matches!(
                kind,
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayVersion
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                e.print().ok();
                return ExitCode::from(0);
            }
            // Everything else is a user error — emit BAD_ARGS envelope.
            let _ = voom_cli::envelope::emit_err(
                "cli",
                "BAD_ARGS",
                e.to_string(),
                Some("Run `voom --help` for usage".into()),
                None,
            );
            return ExitCode::from(1);
        }
    };
    logging::init(&cli.log_level, cli.log_format.to_core());

    let code = match dispatch(cli).await {
        Ok(code) => code,
        Err(err) => {
            let _ = emit_err(
                "internal",
                "INTERNAL",
                err.to_string(),
                Some("Re-run with --log-level=debug and file a bug".into()),
                None,
            );
            2
        }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}

async fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Version => {
            version::run()?;
            Ok(0)
        }
        Command::Health => {
            // Build `Local` as soon as config resolves so any subsequent failure
            // (open, probe) emits a properly-attributed `health` envelope
            // rather than falling through to main's generic `INTERNAL` arm.
            let cfg = Config::resolve(cli.database_url, None, None)?;
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            match ControlPlane::open(cfg.database_url).await {
                Ok(cp) => Ok(health::run(&cp, local).await?),
                Err(err) => {
                    // Most common: DB_UNREACHABLE because the file doesn't
                    // exist. Surface the exact code instead of INTERNAL.
                    let hint = (err.code() == "DB_UNREACHABLE")
                        .then(|| "Run: voom init".to_owned());
                    voom_cli::envelope::emit_err(
                        "health",
                        err.code(),
                        err.to_string(),
                        hint,
                        Some(local),
                    )?;
                    Ok(2)
                }
            }
        }
        Command::Init => {
            let cfg = Config::resolve(cli.database_url, None, None)?;
            // No ControlPlane: init is the write-side path and goes straight
            // to voom_store::init (connect_or_create + migrations).
            let local = Local {
                db_url: cfg.database_url.clone(),
                config_path: cfg.config_path.display().to_string(),
            };
            Ok(init::run(&cfg.database_url, local).await?)
        }
    }
}
```

- [ ] **Step 5: Build the binary**

Run: `cargo build --package voom-cli`
Expected: builds cleanly with zero warnings.

- [ ] **Step 6: Manual smoke check**

```bash
cargo run --package voom-cli -- version | jq .
cargo run --package voom-cli -- --database-url 'sqlite::memory:' health | jq .
cargo run --package voom-cli -- --database-url 'sqlite::memory:' init | jq .
```

Expected: each command emits a JSON object on stdout matching the spec's envelope shape. `version` succeeds; `health` against `:memory:` returns `DB_UNINITIALIZED`; `init` returns `already_initialized: false`.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-cli/
git commit -m "voom-cli: wire health/init/version commands through ControlPlane in async main"
```

---

## Task 21: `voom-cli` — `health` and `init` snapshot tests

**Files:**
- Create: `crates/voom-cli/tests/health_envelope.rs`
- Create: `crates/voom-cli/tests/init_envelope.rs`

- [ ] **Step 1: Health envelope snapshot test**

Create `crates/voom-cli/tests/health_envelope.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_cli::commands::health::{HealthData, HealthDb, HealthRuntime};

#[test]
fn health_payload_current_state_shape() {
    let payload = HealthData {
        db: HealthDb {
            status: "current",
            schema_init_at: Some("2026-05-15T18:23:00.000Z".into()),
            migration_count: Some(1),
        },
        runtime: HealthRuntime { tokio_workers: 8 },
    };
    insta::assert_json_snapshot!("health_current", &payload);
}
```

Plus an integration test that drives `ControlPlane` directly and asserts the uninitialized-DB path exits 2. `ControlPlane::open` is read-only (no-create), so tests must seed the DB on disk via `voom_store::init` or `voom_store::connect_or_create` first.

```rust
use tempfile::NamedTempFile;
use voom_cli::commands::health;
use voom_cli::envelope::Local;
use voom_control_plane::ControlPlane;

fn local_for(url: &str) -> Local {
    Local {
        db_url: url.to_owned(),
        config_path: "/tmp/voom-test/config.toml".into(),
    }
}

#[tokio::test]
async fn health_against_uninitialized_db_returns_exit_code_2() {
    // Seed an empty DB file (no migrations applied) via the create-mode opener,
    // then attach via the no-create read path that ControlPlane uses.
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::connect_or_create(&url).await.unwrap();

    let cp = ControlPlane::open(url.clone()).await.unwrap();
    let code = health::run(&cp, local_for(&url)).await.unwrap();
    assert_eq!(code, 2, "uninitialized DB must surface as DB_UNINITIALIZED with exit code 2");
}

#[tokio::test]
async fn health_against_initialized_db_returns_exit_code_0() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();

    let cp = ControlPlane::open(url.clone()).await.unwrap();
    let code = health::run(&cp, local_for(&url)).await.unwrap();
    assert_eq!(code, 0);
}
```

- [ ] **Step 1.5: Bad-args envelope integration test**

Create `crates/voom-cli/tests/bad_args_envelope.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;

/// Invoking the compiled binary with a bogus flag must produce a parseable
/// JSON envelope on stdout (the agent-facing contract), not clap's default
/// stderr message.
#[test]
fn unknown_flag_produces_bad_args_envelope_on_stdout() {
    let bin = env!("CARGO_BIN_EXE_voom");
    let output = Command::new(bin)
        .args(["--nonsense-flag"])
        .output()
        .expect("failed to invoke binary");

    assert_eq!(output.status.code(), Some(1), "BAD_ARGS exit code is 1");

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON envelope; got {stdout:?}: {e}"));

    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
    assert_eq!(json["command"], "cli");
}
```

`CARGO_BIN_EXE_voom` is set automatically by Cargo for integration tests of binary crates. This is the canonical way to exercise the compiled CLI end-to-end.

- [ ] **Step 2: Init envelope snapshot test**

Create `crates/voom-cli/tests/init_envelope.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use voom_cli::commands::init::InitData;

#[test]
fn init_first_run_shape() {
    let data = InitData {
        migrations_applied: 1,
        schema_init_at: "2026-05-15T18:23:00.000Z".into(),
        already_initialized: false,
    };
    insta::assert_json_snapshot!("init_first", &data);
}

#[test]
fn init_already_initialized_shape() {
    let data = InitData {
        migrations_applied: 0,
        schema_init_at: "2026-05-15T18:23:00.000Z".into(),
        already_initialized: true,
    };
    insta::assert_json_snapshot!("init_idempotent", &data);
}
```

- [ ] **Step 3: Accept snapshots and run**

```bash
cargo test --package voom-cli --test health_envelope --test init_envelope
cargo insta accept --workspace
cargo test --package voom-cli --test health_envelope --test init_envelope
```

Expected: all green after acceptance.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/
git commit -m "voom-cli: snapshot tests for health and init envelope shapes"
```

---

## Task 22: `voom-api` — `/health` router

**Files:**
- Modify: `crates/voom-api/Cargo.toml`
- Modify: `crates/voom-api/src/lib.rs`
- Create: `crates/voom-api/tests/health_route.rs`

- [ ] **Step 1: Add dependencies**

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-api --path crates/voom-core
cargo add --package voom-api --path crates/voom-control-plane
cargo add --package voom-api axum
cargo add --package voom-api serde --features derive
cargo add --package voom-api serde_json
cargo add --package voom-api time --features serde,formatting
cargo add --package voom-api tower
cargo add --package voom-api --dev tokio --features rt-multi-thread,macros
cargo add --package voom-api --dev tower --features util  # ServiceExt::oneshot in tests
cargo add --package voom-api --dev http-body-util
```

- [ ] **Step 2: Write the API router**

Replace `crates/voom-api/src/lib.rs`:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! HTTP surface for the control plane. Shared envelope without the host-only
//! `local` block.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;
use voom_control_plane::{ControlPlane, DbStatus};

pub const SCHEMA_VERSION: &str = "0";

#[derive(Clone)]
pub struct AppState {
    pub control_plane: ControlPlane,
}

#[must_use]
pub fn router(control_plane: ControlPlane) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .with_state(AppState { control_plane })
}

#[derive(Serialize)]
struct Envelope<T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    status: &'static str,
    data: Option<T>,
    warnings: Vec<String>,
    error: Option<ErrorBody>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

#[derive(Serialize)]
struct HealthData {
    db: HealthDb,
    runtime: HealthRuntime,
}

#[derive(Serialize)]
struct HealthDb {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_init_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration_count: Option<u32>,
}

#[derive(Serialize)]
struct HealthRuntime {
    tokio_workers: usize,
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.control_plane.health().await {
        Ok(snap) => match snap.db_status {
            DbStatus::Uninitialized => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_UNINITIALIZED",
                "database has no migrations applied".into(),
                Some("Run `voom init` on the host that owns this database".into()),
            ),
            DbStatus::Partial => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_PARTIAL_SCHEMA",
                format!(
                    "database partially migrated (applied={:?}, expected={:?})",
                    snap.migration_count, snap.expected_migrations
                ),
                Some("Run `voom init` against the current binary".into()),
            ),
            DbStatus::TooNew => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_SCHEMA_TOO_NEW",
                format!(
                    "database has migrations this binary does not know about \
                     (applied={:?}, expected={:?})",
                    snap.migration_count, snap.expected_migrations
                ),
                Some("Upgrade the server binary or roll the database back".into()),
            ),
            DbStatus::Current => {
                let env = Envelope {
                    schema_version: SCHEMA_VERSION,
                    command: "health",
                    status: "ok",
                    data: Some(HealthData {
                        db: HealthDb {
                            status: "current",
                            schema_init_at: snap.schema_init_at.map(|t| {
                                t.format(&time::format_description::well_known::Iso8601::DEFAULT)
                                    .unwrap_or_default()
                            }),
                            migration_count: snap.migration_count,
                        },
                        runtime: HealthRuntime {
                            tokio_workers: std::thread::available_parallelism()
                                .map_or(1, std::num::NonZero::get),
                        },
                    }),
                    warnings: Vec::new(),
                    error: None,
                };
                (StatusCode::OK, Json(env)).into_response()
            }
        },
        Err(err) => err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
            None,
        ),
    }
}

fn err_response(
    status: StatusCode,
    code: &'static str,
    message: String,
    hint: Option<String>,
) -> axum::response::Response {
    let env: Envelope<()> = Envelope {
        schema_version: SCHEMA_VERSION,
        command: "health",
        status: "error",
        data: None,
        warnings: Vec::new(),
        error: Some(ErrorBody { code, message, hint }),
    };
    (status, Json(env)).into_response()
}
```

Critically, this module's `Envelope` struct has **no `local` field**, so there is no code path that can emit one over HTTP.

- [ ] **Step 3: Write the route test**

Create `crates/voom-api/tests/health_route.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use voom_api::router;
use voom_control_plane::ControlPlane;

/// Create an empty DB on disk and return a router bound to it via the
/// read-only `ControlPlane::open` path.
async fn fixture_uninit() -> (tempfile::NamedTempFile, axum::Router) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::connect_or_create(&url).await.unwrap();
    let cp = ControlPlane::open(url).await.unwrap();
    (tmp, router(cp))
}

/// Initialize a disk DB via the public write-side path, then return a router
/// bound to it via the read-only path. `cp.init()` deliberately does not
/// exist — see Task 14.
async fn fixture_initialized() -> (tempfile::NamedTempFile, axum::Router) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(url).await.unwrap();
    (tmp, router(cp))
}

#[tokio::test]
async fn health_on_uninitialized_returns_503_db_uninitialized() {
    let (_keep, app) = fixture_uninit().await;
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "DB_UNINITIALIZED");
    assert!(json.get("local").is_none(), "API must NEVER include local block");
}

#[tokio::test]
async fn health_on_initialized_returns_200_current() {
    let (_keep, app) = fixture_initialized().await;
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["db"]["status"], "current");
    assert_eq!(json["data"]["db"]["migration_count"], 1);
    assert!(json.get("local").is_none(), "API must NEVER include local block");
}

#[tokio::test]
async fn health_on_too_new_db_returns_503_db_schema_too_new() {
    // Drive a disk-backed DB through public APIs only: initialize via
    // voom_store::init, inject a synthetic future migration via a separate
    // pool against the same URL, then build a fresh ControlPlane and router.
    // This avoids needing any cross-crate test helpers.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());

    voom_store::init(&url).await.unwrap();
    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let cp = ControlPlane::open(url).await.unwrap();
    let app = router(cp);
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "DB_SCHEMA_TOO_NEW");
    assert!(json.get("local").is_none(), "API must NEVER include local block");
}
```

Add the dev-dependencies needed for this test:

```bash
cd /Users/dave/src/voom-v2
cargo add --package voom-api --dev voom-store --path crates/voom-store
cargo add --package voom-api --dev sqlx --features runtime-tokio,sqlite
cargo add --package voom-api --dev tempfile
```

- [ ] **Step 4: Run all voom-api tests**

Run: `cargo test --package voom-api`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-api/ Cargo.toml Cargo.lock
git commit -m "voom-api: add /health route with structurally local-free envelope"
```

---

## Task 23: Full workspace verification (`cargo test` + `cargo clippy`)

**Files:** None.

- [ ] **Step 1: Run all tests**

Run: `cargo test --workspace --all-features`
Expected: every test green.

- [ ] **Step 2: Run clippy with deny warnings**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: zero output beyond `Checking … Finished …`.

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 4: Commit any fmt fixes if needed**

If `cargo fmt --all -- --check` reports diffs, run `cargo fmt --all` and commit as `chore: cargo fmt`.

---

## Task 24: `deny.toml` and `audit.toml`

**Files:**
- Create: `deny.toml`
- Create: `audit.toml`

- [ ] **Step 1: Write `deny.toml`**

Create `/Users/dave/src/voom-v2/deny.toml`:

```toml
[graph]
all-features = true

[advisories]
version = 2
yanked = "deny"
ignore = []

[licenses]
version = 2
confidence-threshold = 0.93
allow = [
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "MIT",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-DFS-2016",
    "Unicode-3.0",
    "Zlib",
    "MPL-2.0",
    "CC0-1.0",
]

[bans]
multiple-versions = "warn"
wildcards = "deny"
deny = []
skip = []
skip-tree = []

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

- [ ] **Step 2: Write `audit.toml`**

Create `/Users/dave/src/voom-v2/audit.toml`:

```toml
[advisories]
ignore = []
informational_warnings = ["unmaintained", "notice"]
severity_threshold = "low"
```

- [ ] **Step 3: Verify both tools run**

```bash
cargo install --locked cargo-deny cargo-audit
cargo deny check
cargo audit
```

Expected: both succeed.

- [ ] **Step 4: Commit**

```bash
git add deny.toml audit.toml
git commit -m "Add deny.toml (licenses+bans) and audit.toml (vuln policy)"
```

---

## Task 25: `justfile`

**Files:**
- Create: `justfile`

- [ ] **Step 1: Write the justfile**

Create `/Users/dave/src/voom-v2/justfile` exactly as spec §9 defines it:

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

# Run version + init + health end-to-end against an ephemeral on-disk DB
smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    workdir=$(mktemp -d -t voom-smoke.XXXXXX)
    db="$workdir/voom.db"
    missing="$workdir/never-created.db"
    url="sqlite://$db"
    missing_url="sqlite://$missing"
    trap 'rm -rf "$workdir"' EXIT

    # Helper: run an expected-failing voom command, capturing stdout + exit code
    # separately so `set -o pipefail` doesn't trip the script on the deliberate
    # non-zero CLI exit code.
    expect_fail() {
        local expected_code="$1"; shift
        local expected_err_code="$1"; shift
        set +e
        local out
        out=$("$@")
        local rc=$?
        set -e
        if [[ "$rc" -ne "$expected_code" ]]; then
            echo "expected CLI exit code $expected_code, got $rc"
            echo "stdout: $out"
            return 1
        fi
        echo "$out" | jq -e --arg code "$expected_err_code" \
            '.status == "error" and .error.code == $code' >/dev/null
    }

    # version: no DB touch
    cargo run -q -p voom-cli -- --database-url "$url" version | jq -e '.status == "ok"'

    # health on missing file: must exit 2 with DB_UNREACHABLE AND leave the
    # filesystem untouched (no file, no parent dir creation).
    expect_fail 2 DB_UNREACHABLE \
        cargo run -q -p voom-cli -- --database-url "$missing_url" health
    test ! -e "$missing" || { echo "health created a file at $missing"; exit 1; }

    # init: creates the DB and applies migrations (idempotent)
    cargo run -q -p voom-cli -- --database-url "$url" init | \
        jq -e '.status == "ok" and .data.already_initialized == false' >/dev/null
    cargo run -q -p voom-cli -- --database-url "$url" init | \
        jq -e '.status == "ok" and .data.already_initialized == true' >/dev/null

    # health after init: ok
    cargo run -q -p voom-cli -- --database-url "$url" health | \
        jq -e '.status == "ok" and .data.db.status == "current"' >/dev/null

    echo "==> smoke OK"

# Remove build artifacts
clean:
    cargo clean
```

- [ ] **Step 2: Verify recipes parse**

Run: `just --list`
Expected: all recipes listed, no parse errors.

- [ ] **Step 3: Run `just smoke`**

Run: `just smoke`
Expected: all five `jq` assertions pass; recipe exits 0.

If `jq` is missing, install via `brew install jq` (macOS) or document the dependency in README.

- [ ] **Step 4: Commit**

```bash
git add justfile
git commit -m "Add justfile with setup, ci, smoke, and individual cargo recipes"
```

---

## Task 26: `.pre-commit-config.yaml`

**Files:**
- Create: `.pre-commit-config.yaml`

- [ ] **Step 1: Look up current `pre-commit-hooks` revision**

Pre-commit and prek both pin upstream revs by SHA or tag. Look up the latest tag:

```bash
gh api repos/pre-commit/pre-commit-hooks/releases/latest --jq .tag_name
```

Record the tag for use below (e.g. `v5.0.0`).

- [ ] **Step 2: Write the config**

Create `/Users/dave/src/voom-v2/.pre-commit-config.yaml`:

```yaml
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: <REPLACE WITH TAG FROM STEP 1>
    hooks:
      - id: trailing-whitespace
      - id: end-of-file-fixer
      - id: check-yaml
      - id: check-added-large-files

  - repo: local
    hooks:
      - id: cargo-fmt
        name: cargo fmt --check
        entry: cargo fmt --all -- --check
        language: system
        pass_filenames: false
        files: '\.rs$'

      - id: cargo-clippy
        name: cargo clippy
        entry: cargo clippy --workspace --all-targets -- -D warnings
        language: system
        pass_filenames: false
        files: '\.rs$'

      - id: cargo-test
        name: cargo test --quiet
        entry: cargo test --workspace --quiet
        language: system
        pass_filenames: false
        files: '\.rs$'

      - id: cargo-audit
        name: cargo audit
        entry: cargo audit --deny warnings
        language: system
        pass_filenames: false
        files: '^Cargo\.lock$'
```

- [ ] **Step 3: Verify install**

```bash
prek install
prek run --all-files
```

Expected: all hooks succeed (the cargo hooks are slow on first run).

- [ ] **Step 4: Commit**

```bash
git add .pre-commit-config.yaml
git commit -m "Add prek config running fmt/clippy/test/audit hooks"
```

---

## Task 27: GitHub Actions — `ci.yml`

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Look up pinned SHAs**

Resolve current SHAs for each action you'll use. Run each command and record the SHA + tag for the comment:

```bash
gh api repos/actions/checkout/git/refs/tags/v4 --jq .object.sha
gh api repos/dtolnay/rust-toolchain/git/refs/heads/stable --jq .object.sha
gh api repos/Swatinem/rust-cache/git/refs/tags/v2 --jq .object.sha
gh api repos/extractions/setup-just/git/refs/tags/v3 --jq .object.sha
```

For any tag that resolves to a `tag` object instead of a `commit` object, peel it:

```bash
gh api repos/<owner>/<repo>/git/tags/<sha-from-above> --jq .object.sha
```

(Use the latest stable tag for each — `v4`, `v2`, `v3`, etc.)

- [ ] **Step 2: Write the workflow**

Create `/Users/dave/src/voom-v2/.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

jobs:
  test:
    name: test (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - name: Checkout
        uses: actions/checkout@<SHA>  # v4.x.x  ← replace with Step 1 output
        with:
          persist-credentials: false

      - name: Install Rust stable
        uses: dtolnay/rust-toolchain@<SHA>  # stable
        with:
          toolchain: stable
          components: clippy,rustfmt

      - name: Cache cargo
        uses: Swatinem/rust-cache@<SHA>  # v2.x.x

      - name: Install just
        uses: extractions/setup-just@<SHA>  # v3.x.x

      - name: Install cargo-deny + cargo-audit
        run: cargo install --locked cargo-deny cargo-audit

      - name: just ci
        run: just ci
```

- [ ] **Step 3: Lint with `actionlint`**

```bash
brew install actionlint
actionlint .github/workflows/ci.yml
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "Add ci.yml: matrix (linux, macos) running just ci"
```

---

## Task 28: GitHub Actions — `audit.yml`

**Files:**
- Create: `.github/workflows/audit.yml`

- [ ] **Step 1: Look up the rustsec/audit-check SHA**

```bash
gh api repos/rustsec/audit-check/git/refs/tags/v2 --jq .object.sha
```

- [ ] **Step 2: Write the workflow**

Create `/Users/dave/src/voom-v2/.github/workflows/audit.yml`:

```yaml
name: audit

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    - cron: "0 13 * * *"   # daily at 13:00 UTC

permissions:
  contents: read
  issues: write   # rustsec/audit-check opens issues on new advisories

jobs:
  audit:
    name: cargo audit
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<SHA>  # v4.x.x
        with:
          persist-credentials: false
      - uses: rustsec/audit-check@<SHA>  # v2.x.x
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
```

- [ ] **Step 3: Lint**

```bash
actionlint .github/workflows/audit.yml
```

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/audit.yml
git commit -m "Add audit.yml: rustsec/audit-check on push, PR, daily cron"
```

---

## Task 29: GitHub Actions — `release.yml`

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Look up additional SHAs**

```bash
gh api repos/softprops/action-gh-release/git/refs/tags/v2 --jq .object.sha
gh api repos/houseabsolute/actions-rust-cross/git/refs/tags/v1 --jq .object.sha
```

If `actions-rust-cross` isn't preferred, use `cross` directly with `cargo install --locked cross` and `cross build --release --target <triple>`.

- [ ] **Step 2: Write the workflow**

Create `/Users/dave/src/voom-v2/.github/workflows/release.yml`:

```yaml
name: release

on:
  push:
    tags: ["v*.*.*"]

permissions:
  contents: write

jobs:
  build:
    name: build ${{ matrix.target }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest
          - target: aarch64-apple-darwin
            os: macos-latest
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@<SHA>  # v4.x.x
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<SHA>  # stable
        with:
          toolchain: stable
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@<SHA>  # v2.x.x
      - name: Install cross-compile deps (Linux aarch64)
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: |
          sudo apt-get update
          sudo apt-get install -y gcc-aarch64-linux-gnu
          mkdir -p .cargo
          cat <<EOF >> .cargo/config.toml
          [target.aarch64-unknown-linux-gnu]
          linker = "aarch64-linux-gnu-gcc"
          EOF
      - name: Build
        run: cargo build --release --package voom-cli --target ${{ matrix.target }}
      - name: Package
        run: |
          name="voom-${{ github.ref_name }}-${{ matrix.target }}"
          mkdir -p dist
          cp target/${{ matrix.target }}/release/voom dist/$name
          (cd dist && tar -czf $name.tar.gz $name)
      - uses: softprops/action-gh-release@<SHA>  # v2.x.x
        with:
          files: dist/*.tar.gz
          draft: true
```

- [ ] **Step 3: Lint**

```bash
actionlint .github/workflows/release.yml
```

- [ ] **Step 4: Commit**

```bash
mkdir -p .github/workflows
git add .github/workflows/release.yml
git commit -m "Add release.yml: tag-triggered binaries for linux-x64/arm64 and macos-arm64"
```

---

## Task 30: Dependabot

**Files:**
- Create: `.github/dependabot.yml`

- [ ] **Step 1: Write the config**

Create `/Users/dave/src/voom-v2/.github/dependabot.yml`:

```yaml
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
    schedule:
      interval: "weekly"
    open-pull-requests-limit: 5
    cooldown:
      default-days: 7
    groups:
      cargo-minor-patch:
        update-types:
          - "minor"
          - "patch"

  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
    open-pull-requests-limit: 3
    cooldown:
      default-days: 7
    groups:
      actions-minor-patch:
        update-types:
          - "minor"
          - "patch"
```

- [ ] **Step 2: Commit**

```bash
git add .github/dependabot.yml
git commit -m "Add Dependabot config for cargo + github-actions with 7-day cooldown"
```

---

## Task 31: Release runbook

**Files:**
- Create: `docs/release-process.md`

- [ ] **Step 1: Write the runbook**

Create `/Users/dave/src/voom-v2/docs/release-process.md`:

```markdown
# Release Process

VOOM follows the bump → tag → bump cadence so `main` always carries a `-dev`
SemVer suffix between releases. The release process is run from `main`.

## Steps

1. **Bump to the release version.** On `main`, edit the workspace
   `Cargo.toml`'s `[workspace.package] version` from `0.X.Y-dev` to `0.X.Y`.
   Run `cargo build` to refresh `Cargo.lock`, then commit:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "Release: 0.X.Y"
   ```

2. **Tag the release commit.**

   ```bash
   git tag -a v0.X.Y -m "voom 0.X.Y"
   git push origin v0.X.Y
   ```

   The `release.yml` workflow builds linux-x64, linux-arm64, and macos-arm64
   binaries on tag push and uploads them to a draft GitHub Release.

3. **Bump to the next dev version.** Immediately on `main`, bump
   `[workspace.package] version` from `0.X.Y` → `0.X.(Y+1)-dev` (patch) or
   `0.(X+1).0-dev` (minor). Run `cargo build`, then commit:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "Begin 0.X.(Y+1)-dev"
   ```

4. **Publish the GitHub Release.** Edit the draft, paste a changelog (or
   `git log v0.X.(Y-1)..v0.X.Y --oneline`), and publish. The release artifacts
   self-report version as `0.X.Y+<tag-sha>`.

## Never

- Amend tags after creation.
- Force-push to `main`.
- Skip the post-release bump commit (otherwise the next `main` build reports
  the released version, breaking `--release` provenance).
```

- [ ] **Step 2: Commit**

```bash
git add docs/release-process.md
git commit -m "Add docs/release-process.md: bump-tag-bump runbook"
```

---

## Task 32: ADR 0001 — Durable Jobs Over Events

**Files:**
- Create: `docs/adr/0001-durable-jobs-over-events.md`

- [ ] **Step 1: Write the ADR**

```bash
mkdir -p /Users/dave/src/voom-v2/docs/adr
```

Create `/Users/dave/src/voom-v2/docs/adr/0001-durable-jobs-over-events.md`:

```markdown
---
status: accepted
date: 2026-05-15
deciders: [VOOM core]
---

# 0001 — Durable jobs route work; events record facts

## Context

Both the legacy VOOM and several reference systems (Unmanic, FileFlows) rely
on an event bus to drive worker scheduling. This conflates two concerns:
recording that something happened (an immutable fact) and committing to do
something next (durable work). Event-bus claiming makes recovery, audit, and
idempotency harder because the system has no single source of truth for "what
must still happen."

## Decision

Sprint 0 of voom-v2 separates these concerns at the schema level:

- **Tickets and leases** are durable, transactional rows that route work.
  Workers claim tickets via the scheduler; leases expire on heartbeat
  timeout; the host commits final mutations.
- **Events** are append-only facts that record what occurred. They feed UI,
  audit, metrics, and optional reactive plugins. Events do not claim,
  lease, or schedule work.

Both surfaces exist; the architectural promise is that *only durable jobs
route work*.

## Consequences

- Recovery is simple: any node can resume by re-reading ticket/lease state.
- Reasoning about "what will the system do next" is local to the tickets
  table, not a distributed bus.
- Reactive behavior (triggering work on an event) is layered on top of
  durable job creation rather than being the primary mechanism, which costs
  a small amount of indirection but eliminates a class of double-execution
  bugs.
- Event-bus features (transient pub/sub) are not provided in v1; consumers
  read the append-only event log.

## Alternatives Considered

- **Event-bus claiming.** Rejected: history of double-execution and recovery
  pain in similar systems.
- **In-memory job queues.** Rejected: home deployments need crash-safe
  durable state by default.
```

- [ ] **Step 2: Commit**

```bash
git add docs/adr/0001-durable-jobs-over-events.md
git commit -m "ADR 0001: durable jobs route work; events record facts"
```

---

## Task 33: ADR 0002 — Out-of-process workers only

**Files:**
- Create: `docs/adr/0002-out-of-process-workers-only.md`

- [ ] **Step 1: Write the ADR**

Create `/Users/dave/src/voom-v2/docs/adr/0002-out-of-process-workers-only.md`:

```markdown
---
status: accepted
date: 2026-05-15
deciders: [VOOM core]
---

# 0002 — All providers are out-of-process workers from day one

## Context

Plugin systems that allow both in-process and out-of-process execution
develop two divergent code paths, two security models, and two failure
modes. Subtle bugs accumulate where in-process behavior diverges from
out-of-process behavior.

## Decision

Every provider — built-in or third-party — runs as an out-of-process worker
speaking the same versioned HTTP/JSON protocol from Sprint 2 onward. No
in-process fast path exists. Workers receive `ArtifactHandle`s rather than
raw paths; large bytes move via artifact backends, not through the control
protocol.

## Consequences

- Built-in providers face the same crash, timeout, malformed-result, and
  trust constraints as third-party providers, which means chaos-tested
  reliability is uniform.
- Sprint 0 ships the empty `voom-worker-protocol` crate so the boundary is
  visible from day one even before the wire format lands in Sprint 2.
- Same-machine workers pay a small IPC overhead. Acceptable.
- Capability grants are explicit and enforced by the host regardless of
  worker origin.

## Alternatives Considered

- **In-process built-ins, out-of-process plugins.** Rejected: two code
  paths, two security models, ongoing divergence risk.
- **Embedded Lua/WASM plugins.** Rejected for v1: increases attack surface
  and language complexity before the core protocol is proven.
```

- [ ] **Step 2: Commit**

```bash
git add docs/adr/0002-out-of-process-workers-only.md
git commit -m "ADR 0002: all providers are out-of-process workers from day one"
```

---

## Task 34: ADR 0003 — sqlx + tokio foundation

**Files:**
- Create: `docs/adr/0003-sqlx-and-tokio-foundation.md`

- [ ] **Step 1: Write the ADR**

Create `/Users/dave/src/voom-v2/docs/adr/0003-sqlx-and-tokio-foundation.md`:

```markdown
---
status: accepted
date: 2026-05-15
deciders: [VOOM core]
---

# 0003 — sqlx + tokio as the async storage foundation

## Context

Sprint 0 needs to lock in an async runtime and SQLite client because the
choice cascades into the HTTP framework (Sprint 0 axum skeleton), the daemon
(Sprint 6), and every repository in Sprint 1+.

## Decision

- **Runtime.** `tokio` with the multi-thread flavor, default everywhere.
- **SQLite client.** `sqlx` with `runtime-tokio` and `sqlite` features.
  Compile-time-checked queries via `query!` / `query_as!` are available but
  not required (offline mode set up later). Migrations via `sqlx::migrate!`
  with embedded SQL.
- **HTTP framework.** `axum` (tokio-native, tower-based) for `voom-api`.

## Consequences

- The whole stack is async-first. Synchronous code is the exception.
- Compile-time SQL checking needs an offline query cache (`sqlx prepare`)
  for CI builds without a live database — set up when the first compile-time
  query lands.
- `voom-store` exposes `connect()` and `init()` as separate functions so
  read-side operations never trigger migrations (see spec §3 for rationale).
- Switching runtimes later (e.g., to async-std) requires touching every crate.
  Acceptable: tokio is the de facto default.

## Alternatives Considered

- **rusqlite + refinery.** Rejected: forces blocking-pool wrappers for the
  API/daemon; loses compile-time SQL checking; nice in single-CLI cases but
  awkward at the daemon boundary.
- **sea-orm.** Rejected: ORM abstraction over sqlx adds layers we don't need
  for a from-first-principles design that wants explicit SQL and explicit
  transaction semantics.
- **diesel.** Rejected: synchronous, schema-first; doesn't fit async daemon.
```

- [ ] **Step 2: Commit**

```bash
git add docs/adr/0003-sqlx-and-tokio-foundation.md
git commit -m "ADR 0003: sqlx + tokio as the async storage foundation"
```

---

## Task 35: README quickstart

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rewrite README**

Replace `/Users/dave/src/voom-v2/README.md`:

```markdown
# VOOM — Video Orchestration Operations Manager

A control-plane-first Rust application for managing video libraries through
policy-driven planning, durable job execution, and out-of-process providers.

This is the Sprint 0 skeleton: an empty-but-real workspace with the
engineering guardrails every later sprint inherits. Domain logic lands in
Sprint 1+.

## Getting started

```bash
# One-shot bootstrap: verify toolchain, install hooks, warm cache.
just setup

# Run all checks identical to CI.
just ci

# Smoke-test the CLI end-to-end against an ephemeral database.
just smoke
```

## Workspace map

| Crate | Purpose |
|---|---|
| `voom-core` | Shared domain types: `VoomError`, `VersionInfo`, `Config`, IDs. |
| `voom-store` | SQLite pool, migrations, repositories. |
| `voom-control-plane` | App-services layer wrapping `voom-store`. |
| `voom-api` | axum HTTP router (no server binary yet). |
| `voom-cli` | `voom` binary with `version` / `health` / `init` subcommands. |
| `voom-events` / `voom-policy` / `voom-plan` / `voom-scheduler` / `voom-artifact` / `voom-worker-protocol` | Reserved for later sprints. |

## Design and decisions

- Spec: `docs/specs/voom-control-plane-design.md`
- Sprint 0 design: `docs/superpowers/specs/2026-05-15-voom-sprint-0-design.md`
- ADRs: `docs/adr/`
- Release runbook: `docs/release-process.md`

## License

Apache-2.0.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "Rewrite README with Sprint 0 quickstart and workspace map"
```

---

## Task 36: Final verification — `just ci` and `just smoke`

**Files:** None.

- [ ] **Step 1: Run the full local CI pipeline**

Run: `just ci`
Expected: exits 0; prints `==> All CI checks passed`.

If any check fails, fix the underlying issue (do not move on with a failing pipeline). Common likely failures:

- `cargo fmt --check` diff → run `cargo fmt --all`.
- clippy warning on a newly added file → fix the lint.
- `cargo audit` finds a transitive vuln → if there's no fix yet, add to `audit.toml`'s ignore list with a justifying comment and an expiry date.

- [ ] **Step 2: Run the smoke recipe**

Run: `just smoke`
Expected: all five `jq` assertions pass; exits 0.

- [ ] **Step 3: Build the release profile**

Run: `cargo build --release --package voom-cli`
Expected: release binary at `target/release/voom`.

```bash
./target/release/voom version | jq .
```

Expected: JSON envelope with `build_profile: "release"`.

- [ ] **Step 4: Commit nothing (verification only)**

No commit. If steps 1-3 all succeed, Sprint 0 implementation is done.

---

## Task 37: Push branch and open PR

**Files:** None.

- [ ] **Step 1: Push the branch**

```bash
git -C /Users/dave/src/voom-v2 push -u origin sprint-0-skeleton
```

If `origin` is not configured, ask the user before adding a remote.

- [ ] **Step 2: Open PR**

```bash
gh pr create --title "Sprint 0: workspace skeleton, CLI/API JSON envelope, engineering guardrails" --body "$(cat <<'EOF'
## Summary

Lands the Sprint 0 skeleton specified in `docs/superpowers/specs/2026-05-15-voom-sprint-0-design.md`:

- 11-crate Cargo workspace with strict layering and empty placeholders for Sprint 1+ owners.
- `voom-store` with `connect()` / `init()` / `probe_schema()` — read-side never migrates.
- `voom-cli` with `version`, `health`, `init` subcommands emitting the tagged JSON envelope.
- `voom-api` with `GET /health` that structurally cannot emit the host-only `local` block.
- `voom-control-plane` wiring `voom-store` for both surfaces.
- Build-script SemVer + git-SHA versioning, `0.1.0-dev` starting point.
- `justfile` (setup, ci, smoke), `prek` hooks, `cargo-deny`, `cargo-audit`.
- CI workflows (ci, audit, release) and Dependabot config.
- Three ADRs and a release runbook.

## Test plan

- [ ] `just setup` succeeds on a fresh checkout.
- [ ] `just ci` exits 0.
- [ ] `just smoke` exits 0.
- [ ] CI workflow passes on linux + macos.
- [ ] `audit` workflow surfaces a distinct check.
- [ ] Release dry-run: tag `v0.1.0-dev.preview` locally to confirm `release.yml` resolves; do not push.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-Review Notes

Before declaring the plan complete, the author ran the spec-coverage / placeholder / type-consistency self-review.

**Spec coverage check.** Every spec section maps to a task:

| Spec section | Task(s) |
|---|---|
| §1 Goal & Scope | Tasks 2–35 collectively |
| §2 Workspace Layout | Tasks 2, 3 |
| §3 Storage Foundation | Tasks 8, 9, 10, 11, 12, 13 |
| §4 CLI Shape (envelope, subcommands, exit codes, build.rs) | Tasks 15, 16, 17, 18, 19, 20, 21 |
| §5 API Skeleton | Task 22 |
| §6 Versioning Policy (VersionInfo, build.rs, release runbook) | Tasks 5, 15, 31 |
| §7 Cross-Cutting (errors, logging, config, runtime, deps) | Tasks 4, 6, 17, 18, 20 |
| §8 Engineering Guardrails (lints, fmt, deny, audit, prek, CI, Dependabot, ADRs) | Tasks 2, 24, 26, 27, 28, 29, 30, 32, 33, 34 |
| §9 justfile | Task 25 |
| §10 Exit criteria | Task 36 |
| §11 Out of scope | Honored throughout (no daemon binary, no init command in Sprint 5 — wait, init IS in Sprint 0 per the spec revision; non-goal removed) |

**Type consistency check.** Cross-checked names across tasks:

- `VoomError` (Task 4) used in Tasks 5, 6, 9, 10, 11, 13, 14, 16, 22.
- `VersionInfo` (Task 5) used in Task 19.
- `Config` / `LogFormat` (Task 6) used in Task 18 (`LogFormatArg::to_core`), Task 20 (`Config::resolve`).
- `connect()` / `connect_or_create()` / `init()` / `probe_schema()` / `expected_migrations()` / `SchemaState` / `InitReport` / `MIGRATOR` (Tasks 9–11) used in Tasks 12, 13, 14. `init_on` is gated behind the `test-support` feature and not re-exported.
- `ControlPlane` / `DbStatus` / `HealthSnapshot` (Task 14) used in Tasks 20, 22.
- `Envelope` / `Local` / `emit_ok` / `emit_err` / `Status` / `ErrorBody` (Task 16) used in Tasks 19, 20, 21.

**Placeholder check.** Searched for "TBD", "TODO", "implement later", "add appropriate", "similar to" — none found in the implementation tasks. The two `<SHA>` and `<REPLACE WITH TAG FROM STEP 1>` placeholders in Tasks 26–29 are intentional and immediately followed by the exact `gh api` command that resolves them at execution time, not author memory.

---
