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
