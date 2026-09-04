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

## Refactoring guardrails

- Preserve domain newtypes across repository, orchestration, and event-construction
  boundaries. Do not flatten distinct IDs or durable vocabulary to primitives in an
  intermediate struct and reconstruct the types later; that removes compile-time swap
  protection while leaving the final API looking typed.
- Treat every value read from SQLite as untrusted persisted data. Perform checked numeric
  conversions and structural validation before applying business classification such as
  missing, conflict, or invalid configuration. Corrupt storage is a database error, not a
  domain absence or configuration error.
- Removing a serialized field removes the accepted input too. Put
  `#[serde(deny_unknown_fields)]` on the concrete deserialized struct and add a regression
  that rejects the former field; an output-shape assertion alone does not prove removal.
- Architecture prose is a summary, not an authority. Check workspace manifests or
  `cargo metadata` before changing dependency guidance, distinguish normal dependencies
  from dev-only edges, and describe transitional ownership as transitional rather than
  extending it by precedent.
- When a refactor moves behavior across crate boundaries, compare the old and new failure
  ordering as well as the happy-path result. Preserve transaction ownership, mutation/event
  ordering, deterministic query ordering, and corruption diagnostics unless the governing
  design explicitly changes them.

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

Pre-commit hooks (installed by `just setup` via `prek install`) delegate to `just` recipes so they cannot drift from `just ci`: `fmt-check`, `check-test-layout`, `check-paused-time-db`(+selftest), `check-transaction-openers`(+selftest), `lint`, a light `cargo test --quiet`, `deny`, and `audit` on staged Rust / `Cargo.lock` / `Cargo.toml` / `deny.toml` changes. Two checks are deliberately CI-only because they are too slow per commit: `just doc` and the full `--all-features` test build (`just test`) — run `just ci` before pushing. Don't bypass the hooks; fix the underlying issue.

## Architecture

### Crate architecture and dependency map

The map below describes responsibilities and internal **normal** Cargo dependencies.
`A → B` means `A` declares `B` under `[dependencies]`; it does not describe runtime
data flow. A responsibility names the crate that owns a concern, not the only crate
allowed to use its types. Manifests and `cargo metadata` are authoritative when this
conceptual map and the build graph disagree.

Production crate responsibilities:

- `voom-core`: shared errors, IDs, configuration, clocks, and domain value types.
- `voom-events`: durable event envelopes, subjects, assertions, and payload taxonomy.
- `voom-policy`: policy DSL parsing, validation, compilation, fixtures, and profiles.
- `voom-plan`: deterministic reports, execution plans, diagnostics, and plan hashing.
- `voom-scheduler`: worker scoring, candidate selection, reasons, and decisions.
- `voom-worker-protocol`: versioned worker contracts, payloads, progress, and transport.
- `voom-store`: SQLite connections, migrations, schema probes, and repositories.
- `voom-artifact`: pending-commit records, event glue, and commit-recovery data.
- `voom-control-plane`: durable authority, commit authorization, and orchestration.
- `voom-api`: axum router, application-state wiring, and bounded TLS server binary.
- `voom-node-agent`: authenticated pull execution and node-local worker supervision.
- `voom-cli`: agent-facing commands, JSON envelopes, initialization, and presentation.
- `voom-ffprobe-worker`: local media probing through ffprobe.
- `voom-ffmpeg-worker`: local video and audio transforms through ffmpeg.
- `voom-mkvtoolnix-worker`: local remux operations through mkvtoolnix.
- `voom-verify-artifact-worker`: local artifact verification.
- `voom-backup-worker`: local artifact backup and checksum verification.
- `voom-scan-worker`: storage-owner file discovery and media/sidecar classification.
- `voom-hash-worker`: storage-owner BLAKE3 hashing of root-relative files.

ADR 0050 defines target byte ownership: storage-owner node agents perform or supervise
every byte read and mutation. The control plane retains durable authority, commit
authorization, and orchestration. Its current filesystem-promotion path is transitional
pending issues #416 through #425; do not extend it as control-plane-owned behavior.

Internal normal edges that define the production layering and its exceptions:

```text
voom-events              → voom-core
voom-policy              → voom-core
voom-plan                → voom-core, voom-policy
voom-scheduler           → voom-core
voom-worker-protocol     → voom-core

voom-store               → voom-core, voom-events, voom-policy
voom-artifact            → voom-core, voom-events, voom-store
voom-control-plane       → voom-artifact, voom-core, voom-events, voom-plan,
                           voom-policy, voom-scheduler, voom-store,
                           voom-worker-protocol

voom-api                 → voom-control-plane, voom-core
voom-node-agent          → voom-core, voom-worker-protocol
voom-cli                 → voom-control-plane, voom-core, voom-events,
                           voom-plan, voom-policy, voom-store
real scan/hash/media/backup workers → voom-core, voom-worker-protocol
```

The direct lower-crate edges from `voom-cli` are intentional. The CLI presents
core IDs and errors, event and plan payloads, policy fixtures, and store inspection
records; these presentation and local-initialization concerns do not all route
through `voom-control-plane`.

Support crates are workspace members but are not production layers:

- `voom-conformance` owns the black-box protocol harness and echo worker. Its normal
  edges are to `voom-core` and `voom-worker-protocol`.
- `voom-test-support` owns shared integration-test fixtures. Its normal edges are to
  `voom-control-plane`, `voom-core`, and `voom-store`.
- `voom-fake-support` owns the fake-provider runtime. Its normal edges are to
  `voom-core` and `voom-worker-protocol`.
- `voom-fakes` owns fake, chaos, and benchmark worker binaries. Its normal edges are
  to `voom-core`, `voom-fake-support`, and `voom-worker-protocol`.

`[dev-dependencies]` are deliberately excluded from the production map. They can
point against the normal layering for tests: notably, `voom-store` has dev-only
edges to `voom-control-plane` and `voom-test-support` while `voom-control-plane`
normally depends on `voom-store`. Cargo permits this test-only relationship; it
is not a circular production dependency.

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

`voom worker run-local` is the one documented streaming exception: it is a long-running foreground supervisor whose stdout is a **two-line contract** — first a bare readiness line (`{"status":"ready","worker_id":…,"kind":…,"endpoint":…}`, no envelope wrapper) once the bundled worker has bound and registered, then, on shutdown, the standard single retirement envelope (`status:"ok"`, `data.status:"retired"`). Nothing else is written to stdout. See `docs/runbooks/operator-real-media-execution.md` and `docs/specs/run-local-stdout-contract.md`.

Error `code` strings are public contract — defined in `voom_core::VoomError::code()` (`DB_UNREACHABLE`, `DB_PARTIAL_SCHEMA`, `DB_DIRTY_MIGRATION`, `DB_SCHEMA_TOO_NEW`, `CONFIG_INVALID`, `NOT_FOUND`, `INTERNAL`) plus CLI-layer codes (`BAD_ARGS`). Don't rename or repurpose them; add new variants instead.

### Workspace / versioning

Single source of truth for the package version is `[workspace.package].version` in the root `Cargo.toml`. All member crates inherit via `version.workspace = true`. Internal path deps inherit via `[workspace.dependencies]` + `{ workspace = true }` — do not hardcode versions on internal deps. The release cadence is bump → tag → bump (`-dev` suffix on `main` between releases); full procedure in `docs/release-process.md`.

Adding a new crate: add it to `[workspace] members`, set `version.workspace = true` and the other inherited fields, and if it's an internal dep for other crates also add a `[workspace.dependencies]` entry pointing at its path.

### Where things live

- ADRs: `docs/adr/`
- Sprint specs: `docs/specs/` and `docs/superpowers/specs/`
- Migrations: `migrations/*.sql` (embedded privately by `voom-store`)
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

**Open every transaction through a `voom_store::tx` helper, never `pool.begin()`.**
Which mode a transaction needs depends on what its *first* statement does —
including statements inside the `*_in_tx` helpers it passes its handle to — and
that is a fact only the author has. The four helpers record it:

| helper | mode | when |
|---|---|---|
| `begin_read_then_write` | `BEGIN IMMEDIATE` | reads before it writes — ADR 0083's hazard |
| `begin_write_first` | `BEGIN` | the first statement writes |
| `begin_read_only` | `BEGIN` | never writes |
| `begin_serialized_read` | `BEGIN IMMEDIATE` | never writes, but must not read a stale snapshot |

A read-then-write transaction on a deferred `BEGIN` is refused at its first
write *without* consulting `busy_timeout`, because `SQLite` will not upgrade a
read lock — so it fails under contention instead of waiting. That is issue #546.
Picking `begin_read_only` where WAL's snapshot semantics matter is the mirror
defect: a plain `BEGIN` does not wait for an in-flight writer, so a guard can
pass on state that is already stale.

`just check-transaction-openers` (wired into `just ci` and the hooks) enforces
the boundary: a bare `pool.begin()` or `pool.begin_with(…)` in production code
is a violation. It proves deliberateness, not correctness — the name is a claim
a reviewer can check against the body, which `begin_tx` never was. Test files
may open raw; a competing writer that holds the lock without writing is not a
shape any production helper describes. See `docs/adr/0083-…` and
`docs/adr/0086-transaction-openers-are-named-helpers.md`.

**Tests run on a pinned temp root, not the host's `/tmp`.**
`.cargo/config.toml` forces `TMPDIR` to `.test-tmp/` in the workspace so every
host puts `SQLite` test databases on real storage. Without it a workstation with
a tmpfs `/tmp` (the Fedora and Ubuntu 24.04+ default) gets a nearly free
`fsync` while CI and macOS pay milliseconds, and `fsync` duration is how long a
`SQLite` write lock is held. `temp_databases_land_on_the_pinned_repo_local_root`
fails if the pin stops working. See
`docs/adr/0079-deterministic-test-temp-root.md`.

**A single green run does not mean a test is not flaky.** The races in this
suite are found by repetition and by changing parallelism, not by one run:

- `just test-repeat PKG FILTER [COUNT]` — loop one test, stop on first failure.
- `just test-serial` / `just test-parallel` — the two ends CI already runs (the
  `coverage` job serializes, the `test` job does not). Each end has found races
  the other missed.
- `just test-constrained` — 4 cpus and a 16G ceiling via cgroup v2, for what
  repetition cannot reach (memory pressure, slow storage). Linux only, and it
  is the last thing to reach for, not the first: constraint knobs did not
  separate the cells for the one race measured so far.

`just coverage` produces `lcov.info` consumed by SonarCloud.
