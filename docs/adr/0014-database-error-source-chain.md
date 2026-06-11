# 0014 — Preserve the `sqlx::Error` source chain in `VoomError::Database`

## Status

Accepted (2026-06-11). Implements audit finding L2 (issue #223).

## Context

`VoomError::Database` is the catch-all database-layer error. It is a tuple
variant carrying a single human-readable `String`:

```rust
#[error("database error: {0}")]
Database(String),
```

The variant is constructed at ~644 sites across `voom-core`, `voom-events`,
`voom-store`, and `voom-control-plane`. The dominant idiom wraps a `sqlx::Error`
that surfaced from an `.await`ed query, formatting it into a context-prefixed
string:

```rust
.map_err(|e| VoomError::Database(format!("asset_use_leases insert: {e}")))?
```

This **discards the structured source chain**. `std::error::Error::source()`
returns `None`, so a triaging operator sees only the top-line message and cannot
walk down to the underlying `sqlx::Error` (e.g. the SQLite extended result code,
the offending constraint name, or a transport-level cause). The audit (FABLE_AUDIT
L2) flagged this as a low-severity diagnosability gap.

Not every `Database` site wraps a `sqlx::Error`. The same variant is also used
for:

- URL/`SqliteConnectOptions` parse failures (`pool.rs`),
- integer overflow on dimension/CRF conversions (`video_profiles.rs`),
- "missing field" decode failures (`commit_safety_gate/codecs.rs`),
- string-literal sentinels such as `"database is locked"` (matched by retry
  helpers in `voom-control-plane`).

There is no `From<sqlx::Error>` impl; every site maps explicitly. Several sites
match or destructure the variant:

- `matches!(err, VoomError::Database(_))` (test assertions, ~7 sites),
- `Database(message) if message.contains("database is locked")` (the
  SQLITE_BUSY retry classifier in `leases.rs`),
- `Database(_) => "finalize_failed"` (commit recovery-reason mapping).

### Public contract constraints (AGENTS.md "CLI output contract")

- `VoomError::code()` returns stable wire strings. `Database` maps to
  `ErrorCode::DbUnreachable` → `"DB_UNREACHABLE"`. This must not change.
- The `Display` string `"database error: {message}"` is observed by operators
  and is the body of the envelope `error.message`; it must not change.

## Decision

Convert `Database` from a tuple variant to a **struct variant** that keeps the
existing message and adds an optional structured source:

```rust
#[error("database error: {message}")]
Database {
    message: String,
    #[source]
    source: Option<Box<sqlx::Error>>,
},
```

- `#[error("database error: {message}")]` reproduces the current `Display`
  string byte-for-byte, so the envelope `error.message` is unchanged.
- `#[source]` on the `Option` makes `std::error::Error::source()` return the
  wrapped `sqlx::Error` when present, and `None` otherwise — the source chain is
  preserved exactly for the sites that have a `sqlx::Error` to give.
- `source` is `Box`ed because `sqlx::Error` is large; boxing keeps `VoomError`
  (and the many `Result<_, VoomError>` futures) small, satisfying clippy's
  `large_futures`/`result_large_err` posture.
- `code()`/`error_code()` match `Self::Database { .. }` → `DbUnreachable`,
  unchanged.

Two constructors express intent at call sites and keep the migration mechanical:

```rust
/// Database error with a human-readable message and no structured source.
pub fn database(message: impl Into<String>) -> Self { ... }      // source: None

/// Database error wrapping a `sqlx::Error`, preserving its source chain.
/// The Display message is `"{context}: {source}"`.
pub fn database_context(context: impl Display, source: sqlx::Error) -> Self { ... }
```

Migration buckets (counts at time of writing):

| Bucket | Count | New form |
|---|---|---|
| `map_err(\|e\| Database(format!("ctx: {e}")))` over an `.await`ed sqlx call | 465 | `map_err(\|e\| VoomError::database_context("ctx", e))` |
| `Database(format!(...))` over a non-sqlx value | ~112 | `VoomError::database(format!(...))` |
| `Database("literal".to_owned()/.into())` | ~15 | `VoomError::database("literal")` |
| `matches!(_, Database(_))` | ~7 | `matches!(_, Database { .. })` |
| `Database(msg)` / `Database(message) if …` binding | ~5 | `Database { message, .. }` |

`voom-core` adds `sqlx` as a dependency (sqlite feature only, no runtime) to
name the `sqlx::Error` type. This is the layer that already owns the error
taxonomy and is depended on by every storage crate, so the type is in scope.

## Consequences

- Operators and `tracing` source-chain formatters can now walk a database error
  down to the `sqlx::Error`, including the SQLite extended result code and
  constraint metadata, without parsing the message string.
- The `Display` string and `code()` are unchanged; no envelope, snapshot, or
  `error.code` consumer is affected.
- All 644 sites are migrated in one change (no parallel/deprecated variant), per
  "Replace, don't deprecate". The genuine sqlx sites populate the source; the
  rest pass `None` honestly.
- `voom-core` gains a `sqlx` dependency. It is compile-time only (no async
  runtime feature) and is already transitively present workspace-wide.
- The retry classifier that matches `message.contains("database is locked")`
  keeps working because the message text is preserved. It is unchanged by this
  ADR; tightening it to inspect the structured `sqlx::Error` source instead of
  the string is explicitly **out of scope** (see rejected alternatives).

## Considered & rejected

1. **Retype to `Database(#[source] sqlx::Error)` (tuple, mandatory source).**
   Rejected: ~130 sites have no `sqlx::Error` to supply (URL parse, overflow,
   decode-missing-field, literal sentinels). They would have to fabricate one,
   which is impossible or dishonest. The variant is a general database-layer
   error, not a sqlx newtype.

2. **Add a second variant (`DatabaseSource { … }`) and keep `Database(String)`.**
   Rejected by "Replace, don't deprecate": it leaves two variants for one
   concept and a permanent fork in every `match`/classifier. A single struct
   variant with an optional source covers both cases with one arm.

3. **`From<sqlx::Error> for VoomError` and lean on `?`.**
   Rejected as the primary mechanism: a bare `From` drops the per-site context
   prefix (`"asset_use_leases insert: …"`) that operators rely on to locate the
   failing query. `database_context` keeps the prefix *and* the source. A `From`
   impl could be added later for ergonomics but is not required by this change
   and would tempt context-free `?` usage.

4. **Box the whole `Database` payload or intern messages.**
   Rejected as unnecessary: `Box<sqlx::Error>` already bounds the variant size;
   the `String` message is small and owned at the call site.

5. **Switch the `"database is locked"` retry classifier to match the structured
   `sqlx::Error` source.** Rejected as scope creep. It is a correctness-neutral
   improvement orthogonal to L2, the string match still works, and touching the
   retry path risks the SQLITE_BUSY behavior. Left for a dedicated change.
