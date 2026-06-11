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

1. `VoomError::Database` carries an optional `#[source] Box<sqlx::Error>`; for a
   value built from a real `sqlx::Error`, `std::error::Error::source()` returns
   `Some` and downcasts to `sqlx::Error`. For a non-sqlx value it returns `None`.
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
#[error("database error: {message}")]
Database {
    message: String,
    #[source]
    source: Option<Box<sqlx::Error>>,
},
```

Constructors on `impl VoomError`:

```rust
/// Database error with a human-readable message and no structured source.
pub fn database(message: impl Into<String>) -> Self {
    Self::Database { message: message.into(), source: None }
}

/// Database error wrapping a `sqlx::Error`, preserving its source chain.
/// Display message is `"{context}: {source}"`.
pub fn database_context(context: impl std::fmt::Display, source: sqlx::Error) -> Self {
    Self::Database {
        message: format!("{context}: {source}"),
        source: Some(Box::new(source)),
    }
}
```

`code()`/`error_code()` arms become `Self::Database { .. } => ErrorCode::DbUnreachable`.

`voom-core/Cargo.toml` gains `sqlx = { workspace = true }` (sqlite feature, no
runtime feature pulled in beyond what the workspace dep already declares — it is
compile-time only here).

## Migration map

| Bucket | Old | New |
|---|---|---|
| sqlx `map_err` | `map_err(\|e\| VoomError::Database(format!("ctx: {e}")))` | `map_err(\|e\| VoomError::database_context("ctx", e))` |
| sqlx closure with named arg | `move \|e: sqlx::Error\| VoomError::Database(format!("ctx.{field}: {e}"))` | `move \|e: sqlx::Error\| VoomError::database_context(format!("ctx.{field}"), e)` |
| non-sqlx format | `VoomError::Database(format!(...))` | `VoomError::database(format!(...))` |
| literal | `VoomError::Database("x".to_owned())` / `.into()` | `VoomError::database("x")` |
| match (ignore) | `matches!(_, VoomError::Database(_))`, `Database(_) =>` | `Database { .. }` |
| match (bind msg) | `Database(message) if message.contains(..)`, `Database(msg)` | `Database { message, .. }` / rename binding |

A site is in the **sqlx** bucket iff its `map_err`/closure input is a
`sqlx::Error` (the `.await`ed query idiom, or an explicit `e: sqlx::Error`). All
other formatted/literal sites use `database(..)` honestly with `source: None`.

## Edge cases / failure modes

- `database_context` formats `source` into the message *before* boxing it, so the
  message keeps the exact text operators see today (`"ctx: <sqlx display>"`).
- `Option<Box<sqlx::Error>>` with `#[source]`: thiserror returns the inner error
  when `Some`, `None` when `None` — verified by a unit test that downcasts.
- The `"database is locked"` classifier matches on `message`; preserved because
  `database_context("...", e)` still embeds the sqlx display, and the literal
  `database("database is locked")` site keeps the exact text.
- Variant size: `Box` keeps `VoomError` small; confirm no new `large_futures` /
  `result_large_err` clippy warning appears.

## Test plan (TDD)

In `crates/voom-core/src/error_test.rs` (sibling-file convention):

1. `database_context` preserves `DB_UNREACHABLE` code + `error_code`.
2. `database_context` exposes the `sqlx::Error` via `Error::source()` and it
   downcasts to `sqlx::Error`.
3. `database` (no source) returns `source() == None` and still `DB_UNREACHABLE`.
4. `database_context` Display equals `"database error: {context}: {sqlx display}"`.
5. Existing `database_variant_has_db_unreachable_code` updated to the new
   constructor, still asserting the code.

Use a cheap real `sqlx::Error` value (e.g. `sqlx::Error::RowNotFound`) — no DB
connection, no async runtime, so the unit test stays pure.

## Rollout / risk

- Single PR, single logical change set (variant + constructors + mechanical
  call-site migration). `voom-control-plane` is touched (it has `Database`
  sites) — coordinate with #230 which is refactoring that crate; conflicts are
  mechanical (constructor rename), resolvable in one rebase pass.
- Rollback: revert the PR; no data/schema/migration involved.
