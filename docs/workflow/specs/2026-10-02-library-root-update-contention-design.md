# Library-root update writer contention (#664)

## Problem and scope
An update supplying a default root reads its membership inside a deferred transaction
before updating settings. SQLite refuses that read-to-write upgrade under another writer,
bypassing the busy timeout. The repository owns this transaction; control-plane and CLI
callers keep their existing interfaces. The approved exclusion is a broad opener audit,
owned by repository maintenance triage.

## Design
Use the existing cancellation-safe `begin_read_then_write` opener in
`update_library_root`, as ADR 0083 and ADR 0086 require. Remove the unused
`begin_write_first` import. Retain current pre-transaction lookup, validation,
explicit rollback, commit, and post-update lookup ordering.
A retry loop adds no value over SQLite's existing busy handler; moving validation
outside the transaction would weaken the membership check. No new decision or ADR is needed.

## Success
For library-root updates, writer contention is handled at the opener. A held writer
with zero busy timeout fails with the opener's context and leaves the row unchanged.
After release the same update succeeds. With the production busy timeout, a pending update
can survive contention and succeed after release. Existing partial-update and rollback
behavior remains covered by the sibling tests.

## Failure model
- Actors and deployments: local control-plane/CLI library-root updates using SQLite WAL pools.
- Invariants and assets: atomic settings/default updates; bounded waiting instead of failed lock upgrade.
- Accepted failure classes: a writer exceeding the configured busy timeout remains a database error;
  the normal-timeout regression's short pending observation alone cannot prove SQL execution began.
- Covered elsewhere: cancellation-safe opener by ADR 0087 and its tests; broader opener audit by
  repository maintenance triage; existing invalid-default and rollback behavior by library_roots tests.

## Global Constraints
Rust, tokio and sqlx remain the existing stack; add no dependency or target floor.
Use sibling unit tests, real Tokio time, pinned repository-local temporary databases,
and named production transaction helpers. Public APIs, data schema and error codes stay unchanged.

## Validation
Add two sibling regressions: a timing-independent zero-timeout test discriminating the
opener error from the old UPDATE error, and a bounded normal-timeout wait/release test.
Both supply a non-null same-library default so the relevant SELECT executes.
Run them against old code first, then with the corrected opener; assert stored settings
after success and no mutation after failure. Existing library_roots tests cover errors,
partial updates and rollback. Run focused tests at serial/default parallelism, then
repository formatting, source guards, lint, hooks and final `just ci`.
