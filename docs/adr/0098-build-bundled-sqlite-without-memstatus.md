# 0098 — Build bundled SQLite without the global memory-statistics mutex

## Status

Accepted (2026-09-30)

## Context

`libsqlite3-sys` 0.30.1 compiles the bundled amalgamation with SQLite's default
`SQLITE_DEFAULT_MEMSTATUS=1`. With memory statistics on, every SQLite
`malloc` and `free` in the process takes one global mutex to update the
counters. Every pooled connection in the process shares that one lock.

The test suite made the cost visible. The `voom-store` unit binary (768 tests,
each opening a pool and running migrations) measured on an 8-core Linux host,
binary run directly:

| Build | `--test-threads` | Wall | User | Sys |
|---|---|---|---|---|
| default | 1 | 42.7s | 40.8s | 5.8s |
| default | 8 | 40.5s | 85.7s | 146.2s |
| `MEMSTATUS=0` | 8 | 5.8s | 36.8s | 3.4s |

Eight threads bought almost nothing over one: system time rose 25x and
voluntary context switches rose from 0.75M to 21M, with `futex` the dominant
syscall. Removing the statistics removed the contention.

The production control plane opens pools of up to 8 connections against one
database, so it contends on the same lock under concurrent load.

Nothing in the workspace reads SQLite's memory accounting:
`sqlite3_memory_used`, `sqlite3_memory_highwater`, `sqlite3_status` and the
soft/hard heap limits are unused (`rg -i 'memory_used|soft_heap|hard_heap|memory_highwater|sqlite3_status' crates`
at `9d7ea8e8`). SQLite's own "recommended compile-time options" list
`SQLITE_DEFAULT_MEMSTATUS=0` for this reason.

## Decision

Set `LIBSQLITE3_FLAGS = "-DSQLITE_DEFAULT_MEMSTATUS=0"` in the `[env]` table of
`.cargo/config.toml`, with `force = true`.

`libsqlite3-sys`'s build script appends that variable's `-D` flags to the
amalgamation compile and declares `rerun-if-env-changed` on it, so a change
rebuilds SQLite. Cargo applies `[env]` to every build in the workspace, so
test, dev and release binaries all get the same SQLite.

`bundled_sqlite_is_built_without_memory_statistics` asserts `PRAGMA
compile_options` reports `DEFAULT_MEMSTATUS=0`. SQLite reports that option only
when the value differs from 1, so the test fails if the flag is dropped,
overridden or not honored by a future `libsqlite3-sys`.

## Consequences

- Parallel tests and concurrent production connections no longer serialize on
  SQLite's allocator bookkeeping.
- `sqlite3_memory_used` and the heap-limit APIs stop working (they return 0 or
  have no effect). Anything that later needs them has to revisit this ADR.
- `force = true` means a developer's own `LIBSQLITE3_FLAGS` is ignored in this
  workspace. Adding SQLite compile flags means editing this entry.

## Considered & rejected

- **Call `sqlite3_config(SQLITE_CONFIG_MEMSTATUS, 0)` at startup.** judgment:
  it must run before SQLite initializes, which means a direct
  `libsqlite3-sys` dependency, `unsafe` FFI, and an ordering requirement for every
  binary and test harness. The build flag has none of these.
- **Apply the flag to tests only.** judgment: production has the same contention,
  and building production and tests against different SQLite builds is the kind
  of silent divergence ADR 0079 removed.
- **`[env]` without `force = true`.** judgment: an inherited `LIBSQLITE3_FLAGS`
  would replace the flag without any sign. The compile-options test would catch
  it, but only when tests run in that environment.
