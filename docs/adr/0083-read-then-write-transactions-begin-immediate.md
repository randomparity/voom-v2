# 0083 — Read-then-write transactions begin immediate

## Status

Accepted (2026-08-25)

## Context

`crates/voom-store/src/pool.rs` sets a 30s `busy_timeout` on every connection,
and `pool_test.rs` asserts it survives CPU starvation. Despite that,
`delayed_acquire_replay_never_dispatches` failed on `main` with
`lease expire: ... (code: 5) database is locked` — twice in CI, and locally on
an idle 48-core workstation at roughly one run in eight (#546).

The busy handler was never consulted. `SQLite` skips it deliberately on a
**lock upgrade**: a transaction opened with a deferred `BEGIN` takes a read
snapshot at its first `SELECT` and only asks for the write lock at its first
write. Making the caller wait there could deadlock two transactions that each
hold a read lock and want to upgrade, so `SQLite` returns `SQLITE_BUSY`
immediately instead. Waiting is only safe when the write lock is taken before
any read, which is what `BEGIN IMMEDIATE` does.

The consequence is that a read-then-write transaction fails on the first
contended instant regardless of how generous `busy_timeout` is, while a
write-first transaction waits the full budget. Those two shapes look identical
at the call site — both are `pool.begin()` — and the distinction was written
down in exactly one place: the doc comment on `begin_immediate_tx`
(`crates/voom-control-plane/src/cases/mod.rs`). Several call sites had already
been converted one at a time as individual flakes surfaced
(`acquire_lease`, `heartbeat_lease`, `mark_ready_if_unblocked`,
`recover_expired_scan_sessions`), with no rule recorded to say which of the
remaining ~90 deferred sites were also wrong.

`ControlPlane::expire_due` is a production path, not only a test one:
`remote_recover` calls it on the recovery loop.

## Decision

A transaction whose **first** statement reads and whose **later** statements
write opens with `BEGIN IMMEDIATE`. Everything else keeps the deferred `BEGIN`.

The criterion is the lock upgrade, not the presence of a write. A transaction
that writes first — including `UPDATE ... WHERE id IN (SELECT ...)`, which is
one statement and takes the write lock before its subquery reads — never
upgrades, so `busy_timeout` already applies to it and `BEGIN IMMEDIATE` would
change nothing.

`voom-control-plane` uses the existing `begin_immediate_tx` helper.
`voom-store` cannot depend on `voom-control-plane`, so its repositories keep
the established in-crate form, `pool.begin_with("BEGIN IMMEDIATE")` with a
call-specific error context — the same form already used by `LeaseRepo::acquire`,
`PolicyRepo`, `workflow_progress`, and the audio-operation repositories.

Converted under this rule in the change that recorded it:

- `SqliteLeaseRepo::expire_due` — scans due leases, then updates them.
- `ControlPlane::expire_due` — the same scan plus paired event appends; the
  path `remote_recover` calls and the one that failed.
- `ControlPlane::heartbeat_node` — reads the auth record, then writes the
  heartbeat, on every agent's timer while `remote_recover` writes `nodes`.
- `ControlPlane::record_pre_lease_ticket_failure` — reads the ticket and checks
  for a held lease, then writes the failure.

Sites matching the shape but with no production caller (`SqliteLeaseRepo::fail`,
`SqliteLeaseRepo::force_release`, `SqliteTicketRepo::mark_ready_if_unblocked`,
`ControlPlane::force_release_lease`) are left deferred and tracked in #552
rather than converted speculatively.

## Consequences

- A contended read-then-write transaction now waits out the other writer
  instead of failing, so `busy_timeout` means what `pool.rs` says it means.
- These transactions hold the write lock from `BEGIN` rather than from their
  first write, so writers serialize slightly earlier. Readers are unaffected
  under WAL, and the affected transactions are short.
- The rule now has a home outside a single doc comment, and a criterion sharp
  enough to classify the remaining deferred sites without re-deriving the
  `SQLite` semantics each time.
- Nothing enforces it yet. #552 covers a `just` check that flags a repository
  method opening a deferred transaction and then writing.
- Two regression tests hold the line, one per crate
  (`expire_due_waits_out_a_concurrent_writer`): a competing `BEGIN IMMEDIATE`
  holds the write lock for 200ms while `expire_due` runs. Both were verified to
  redden against the deferred `BEGIN` with the exact error from #546.

## Considered & rejected

- **Raise `busy_timeout`.** verified: the busy handler is not invoked at all on
  a lock upgrade, so no value fixes this path. The 30s already configured was
  never consulted.
- **Retry the transaction on `SQLITE_BUSY`.** judgment: re-runs the read half
  against a newer snapshot, so it changes the outcome rather than the timing,
  and it reintroduces the deadlock `SQLite` refuses the wait to avoid.
- **Convert every deferred `pool.begin()` in the workspace.** judgment: ~90
  sites, most read-only or write-first, where the change is either a no-op or
  needless lock-hold. A rule that fires everywhere carries no information about
  where the hazard is.
- **Add a shared `begin_immediate` helper to `voom-store`.** judgment: the
  crate already has three spellings of this transaction (a private helper in
  `policies.rs` and inline `begin_with` elsewhere); a fourth entry point would
  have to replace the others to be worth adding, which is a refactor of its own
  and not this defect.
- **Make `expire_due` write-first.** judgment: it reports which leases it
  expired and which tickets it requeued, and those IDs come from the scan.
  Restructuring the query to derive them from `RETURNING` would rewrite working
  batch and retry-accounting logic to avoid a one-word change to `BEGIN`.
