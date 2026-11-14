# 0008 — The bundled SQLite advisory awaits an sqlx release

## Status

Open
review-by: 2027-02-14

## Concern

RUSTSEC-2026-0459 (CVE-2026-119375): SQLite before 3.54.0 can return uninitialized
page-cache memory when it opens a database file together with a `-wal` sidecar whose page
size differs from the database's. A crafted `.db` + `-wal` pair can make ordinary queries
return stale bytes from the process's memory.

VOOM links SQLite through `libsqlite3-sys` 0.30.1 with the `bundled` feature
(`crates/voom-store/Cargo.toml`), which builds SQLite 3.46.0 and is affected.
The fix is `libsqlite3-sys` >= 0.39, and no released sqlx accepts it: `sqlx-sqlite` 0.8.6
requires `^0.30.1` and 0.9.0 requires `>=0.30.1, <0.38.0` (crates.io, 2026-11-14).
`libsqlite3-sys` must stay link-compatible with the copy sqlx resolves, so it cannot move
on its own.

Both advisory checks therefore ignore this one ID: `deny.toml` `[advisories] ignore` and
`.cargo/audit.toml` `ignore`. The `audit` workflow (`rustsec/audit-check`) runs
`cargo audit` from the repository root and reads the same `.cargo/audit.toml`.

## Exposure

Bounded to the code paths at `818cc91a`. The only production opener of a SQLite file is
`voom_store::pool::connect` / `connect_or_create` (`crates/voom-store/src/pool.rs`), which
opens the configured `database_url` in WAL mode. That URL comes from the operator: the CLI
`--database-url` flag, `VOOM_DATABASE_URL`, or the default `<data_dir>/voom.db`
(`default_database_url`, `crates/voom-core/src/runtime/config.rs`). The `voom-api` server
takes the same `--database-url` / `VOOM_DATABASE_URL` setting at process start
(`crates/voom-api/src/config.rs`); it is the one process with network clients, and no
request reaches the URL. No production path imports, attaches, restores, or opens a
database file received from another party.

Exploitation therefore needs a party less trusted than the operator to write a crafted
`.db` + `-wal` pair at the path the operator configured. Shared or network filesystems
where another user can write the data directory are outside the supported deployment and
are the owner's to assess against this record.

## Why deferred

The remedies that do not wait for sqlx are each owned elsewhere: replacing sqlx with
`rusqlite` (ADR 0003's owner), bumping sqlx for its own sake (a separate issue, and 0.9
does not reach the fix either), and linking a system SQLite instead of the bundled source
(the deployment owner). Issue #857's remediation, chosen by the operator, is a narrow,
time-boxed ignore owned by this record.

## Non-regression boundary

Until this record closes:

- The ignore names RUSTSEC-2026-0459 only. Every other advisory still fails `cargo deny
  check` and `cargo audit --deny warnings`; no other ID is added under this record.
- A change that adds a code path opening a SQLite file this process did not create from
  operator configuration — an import, restore, attach, upload, or fixture loaded from
  outside the repository — invalidates the exposure above. It must reopen this record (or
  resolve it) before landing.

At `818cc91a` this predicate matches exactly one file, `crates/voom-store/src/pool.rs`;
a match in any other file is such a change:

```
rg -l -i -e 'SqlitePool::connect|SqliteConnectOptions|SqlitePoolOptions' \
  -e 'SqliteConnection::connect|ATTACH DATABASE|VACUUM INTO' \
  crates --type rust -g '!**/tests/**' -g '!**/*_test.rs' -g '!crates/voom-test-support/**' \
  -g '!crates/voom-fake*/**' -g '!crates/voom-conformance/**'
```

Callers reach that opener through the store's public `voom_store::connect` and
`voom_store::init`. At `818cc91a` this second predicate matches exactly three files —
`crates/voom-control-plane/src/lib.rs`, `crates/voom-control-plane/src/health.rs`, and
`crates/voom-cli/src/commands/system/init.rs` — each passing the configured
`database_url`; a new file in its output must be checked against the rule above:

```
rg -l -e 'voom_store::(connect|init)\b' -e 'use voom_store::\{[^}]*\b(connect|init)\b' \
  crates --type rust -g '!**/tests/**' -g '!**/*_test.rs' -g '!crates/voom-test-support/**' \
  -g '!crates/voom-fake*/**' -g '!crates/voom-conformance/**' -g '!crates/voom-store/**'
```

The predicates see direct openers and their first callers only. A new path that hands a
non-operator URL to an existing caller (for example `ControlPlane`'s constructor) is not
visible to them; the rule above still governs it, and review is what holds it.

The excluded paths are test-only crates and files; they open throwaway databases the test
itself creates.

## What would resolve it

An sqlx release whose `sqlx-sqlite` permits `libsqlite3-sys` >= 0.39. Then bump sqlx and
`libsqlite3-sys` together, delete both ignores, and close this record. Done when
`cargo deny check` and `cargo audit --deny warnings` pass with no RUSTSEC-2026-0459 entry
in `deny.toml` or `.cargo/audit.toml`.

## Provenance

target: deny.toml
target: .cargo/audit.toml
target: crates/voom-store/Cargo.toml
target: crates/voom-store/src/pool.rs
target: crates/voom-core/src/runtime/config.rs
Recorded by `$quest` for issue #857 (scope token `q857-59bddab4`), 2026-11-14, under the
operator's remediation decision relayed by the campaign. The sqlx requirements were read
from crates.io on that date.
tracker: #857
