# Issue #223 — Preserve the `sqlx::Error` source chain in `VoomError`

**Status:** Design approved (autonomous run; assumptions stated below).
**ADR:** [docs/adr/0014-database-error-source-chain.md](../../adr/0014-database-error-source-chain.md)
**Severity:** Low (audit L2). **Scope:** `voom-core`, `voom-events`, `voom-store`, `voom-control-plane`.

## Problem

`VoomError::Database(String)` interpolates a `sqlx::Error` into a context string
and discards the structured error. `std::error::Error::source()` returns `None`,
so operators cannot walk down to the underlying `sqlx::Error` (SQLite extended
result code, constraint name, transport cause) for triage. ~644 construction and
match sites use the variant; ~465 of them wrap a genuine `sqlx::Error`, the rest
wrap non-sqlx values (URL parse, integer overflow, decode-missing-field, literal
sentinels).

## Goal / success criteria

A reviewer can verify all of:

1. `VoomError::Database` carries an optional `#[source] Box<dyn Error + Send +
   Sync>`; for a value built from a real `sqlx::Error`,
   `std::error::Error::source()` returns `Some` and `downcast_ref::<sqlx::Error>()`
   is `Some`. For a non-sqlx value it returns `None`.
2. `VoomError::Database { .. }.code()` is still `"DB_UNREACHABLE"` and
   `error_code()` is still `ErrorCode::DbUnreachable`.
3. The `Display` string is still exactly `"database error: {message}"` for both
   constructors (the `database_context` message is `"{context}: {source}"`).
4. No `Database(String)` tuple form remains; every construction goes through
   `VoomError::database(..)` or `VoomError::database_context(..)`.
5. `just ci` is green: `fmt-check`, `lint` (clippy `-D warnings`, pedantic),
   `check-test-layout`, `test --all-features`, `doc`, `deny`, `audit`.
6. Insta snapshots in `crates/voom-cli/tests/snapshots/` are unchanged (the
   envelope `error.code` and `error.message` are byte-identical), or any change
   is reviewed and justified.

## Non-goals

- Changing any other `VoomError` variant.
- Adding a `From<sqlx::Error>` impl (rejected in ADR §3).
- Re-implementing the `"database is locked"` retry classifier to match the
  structured source instead of the message string (rejected in ADR §5).
- Touching error `code` strings or the `ErrorCode` enum.

## Design

Struct variant on `voom_core::VoomError`:

```rust
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[error("database error: {message}")]
Database {
    message: String,
    #[source]
    source: Option<BoxError>,
},
```

The source is a **boxed trait object**, not a concrete `Box<sqlx::Error>`. This
keeps `voom-core` (the bottom of the one-way layer graph) free of a `sqlx`
production dependency that would otherwise propagate — with its
`runtime-tokio` + `sqlite` features — into all 19 consumer crates, including
the storage-agnostic `voom-policy`/`voom-plan`/`voom-scheduler`/
`voom-worker-protocol`. Consumers recover the concrete type via
`err.source().and_then(|s| s.downcast_ref::<sqlx::Error>())`. See ADR §3
rejected-alternative 4.

Constructors on `impl VoomError`:

```rust
/// Database error with a human-readable message and no structured source.
pub fn database(message: impl Into<String>) -> Self {
    Self::Database { message: message.into(), source: None }
}

/// Database error that preserves an underlying error's source chain.
/// `context` is the full prefix that previously preceded `: {e}`, so the
/// composed Display message `"{context}: {source}"` is byte-identical to the
/// pre-migration text.
pub fn database_context(
    context: impl std::fmt::Display,
    source: impl Into<BoxError>,
) -> Self {
    let source = source.into();
    Self::Database { message: format!("{context}: {source}"), source: Some(source) }
}
```

A caller passes its `sqlx::Error` by value; the blanket
`From<E: Error + Send + Sync> for Box<dyn Error + Send + Sync>` boxes it. The
message is composed from the boxed source's `Display` before the box is stored.

`code()`/`error_code()` arms become `Self::Database { .. } => ErrorCode::DbUnreachable`.

`voom-core/Cargo.toml` gains **no** production dependency. The sibling test file
`crates/voom-core/src/error_test.rs` requires a `sqlx::Error` value, so
`sqlx = { workspace = true }` is added under `[dev-dependencies]` only;
dev-dependencies do not propagate to downstream crates.

## Migration map

| Bucket | Old | New |
|---|---|---|
| sqlx `map_err` | `map_err(\|e\| VoomError::Database(format!("<prefix>: {e}")))` | `map_err(\|e\| VoomError::database_context("<prefix>", e))` |
| sqlx closure with interpolated prefix | `move \|e: sqlx::Error\| VoomError::Database(format!("ctx.{field}: {e}"))` | `move \|e: sqlx::Error\| VoomError::database_context(format!("ctx.{field}"), e)` |
| non-sqlx format | `VoomError::Database(format!(...))` | `VoomError::database(format!(...))` |
| literal | `VoomError::Database("x".to_owned())` / `.into()` | `VoomError::database("x")` |
| match (ignore) | `matches!(_, VoomError::Database(_))`, `Database(_) =>` | `Database { .. }` |
| match (bind msg) | `Database(message) if message.contains(..)`, `Database(msg)` | `Database { message, .. }` / rename binding |

**Message-preserving rule.** `context` passed to `database_context` MUST be the
entire prefix that preceded the final `: {e}` in the old format string,
**including interpolated fragments**. Examples from the tree:

- `"invalid sqlite url {url:?}: {e}"` → `database_context(format!("invalid sqlite url {url:?}"), e)`
- `"pool open failed for {url:?} (create={create}): {e}"` → `database_context(format!("pool open failed for {url:?} (create={create})"), e)`
- `"video_profiles.{field}: {e}"` → `database_context(format!("video_profiles.{field}"), e)`

The composed `"{context}: {source}"` is then byte-identical to the original. A
site is in the **sqlx** bucket iff the value being formatted is a `sqlx::Error`
(the `.await`ed query idiom, or an explicit `e: sqlx::Error`). All other
formatted/literal sites use `database(..)` honestly with `source: None`.

## Edge cases / failure modes

- `database_context` formats `source` into the message *before* storing the box,
  so the message keeps the exact text operators see today.
- `Option<BoxError>` with `#[source]`: thiserror returns the inner error when
  `Some`, `None` when `None`. Recovered concretely via `downcast_ref::<sqlx::Error>()`.
- The `"database is locked"` classifier matches on `message`; preserved because
  `database_context("...", e)` still embeds the sqlx display, and the literal
  `database("database is locked")` site keeps the exact text.
- Variant size: `Box` keeps `VoomError` small; confirm no new `large_futures` /
  `result_large_err` clippy warning appears.

## Test plan (TDD)

In `crates/voom-core/src/error_test.rs` (sibling-file convention):

1. `database_context` preserves `DB_UNREACHABLE` code + `error_code`.
2. `database_context(_, sqlx::Error::RowNotFound)` exposes the source via
   `std::error::Error::source()`, and
   `source().and_then(|s| s.downcast_ref::<sqlx::Error>())` is `Some`, matching
   `sqlx::Error::RowNotFound`.
3. `database` (no source) returns `source() == None` and still `DB_UNREACHABLE`.
4. `database_context("video_profiles.crf", sqlx::Error::RowNotFound)` Display
   equals `"database error: video_profiles.crf: {RowNotFound display}"` — pins
   the byte-identical message composition.
5. Existing `database_variant_has_db_unreachable_code` updated to the new
   constructor, still asserting the code.

`sqlx::Error::RowNotFound` is a unit variant — constructible with no DB
connection and no async runtime, so the unit test stays pure. After migration,
run `cargo insta test --package voom-cli` (or `cargo insta review`) and confirm
no snapshot under `crates/voom-cli/tests/snapshots/` changed; any diff must be
reviewed and justified, not blindly accepted.

## Rollout / risk

- Single PR, single logical change set (variant + constructors + mechanical
  call-site migration). `voom-control-plane` is touched (it has `Database`
  sites) — coordinate with #230 which is refactoring that crate; conflicts are
  mechanical (constructor rename), resolvable in one rebase pass.
- Rollback: revert the PR; no data/schema/migration involved.
