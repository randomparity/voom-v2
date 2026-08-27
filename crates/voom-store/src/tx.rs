//! Transaction openers, named for the shape of the transaction they open.
//!
//! Every pool-level transaction in production code is opened by one of these
//! four functions. `scripts/check-transaction-openers.sh` enforces that: a bare
//! `pool.begin()` or `pool.begin_with(…)` anywhere else is a violation.
//!
//! The reason is [ADR 0083]. `SQLite` refuses a read→write lock upgrade with
//! `SQLITE_BUSY` *without* invoking the busy handler, so a transaction that
//! reads before it writes never consults the pool's `busy_timeout` and fails
//! immediately under contention. `BEGIN IMMEDIATE` takes the write lock up
//! front, so writers serialize instead.
//!
//! Which mode a transaction needs depends on what its *first* statement does —
//! a fact the author knows while writing it and nothing downstream can recover
//! cheaply. These names are how that fact is recorded. See [ADR 0086].
//!
//! [ADR 0083]: https://github.com/randomparity/voom-v2/blob/main/docs/adr/0083-read-then-write-transactions-begin-immediate.md
//! [ADR 0086]: https://github.com/randomparity/voom-v2/blob/main/docs/adr/0086-transaction-openers-are-named-helpers.md

use sqlx::{Sqlite, SqlitePool, Transaction};
use voom_core::VoomError;

/// Open a transaction that reads before it writes.
///
/// Takes `SQLite`'s write lock up front (`BEGIN IMMEDIATE`) so `busy_timeout`
/// serializes competing writers. A deferred `BEGIN` here is the #546 defect: the
/// lock upgrade at the first write is refused without the busy handler ever
/// running, so the caller sees `database is locked` instead of waiting.
///
/// "Reads before it writes" includes the reads inside `*_in_tx` helpers this
/// transaction passes its handle to — the first statement executed against the
/// handle is what counts, not the first statement in this function.
///
/// # Errors
/// Returns [`VoomError::Database`] if the transaction cannot be opened.
pub async fn begin_read_then_write(
    pool: &SqlitePool,
    context: &'static str,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| VoomError::database_context(context, e))
}

/// Open a transaction whose first statement writes.
///
/// A deferred `BEGIN` is correct here: the write lock is taken when that first
/// write executes, and there is no earlier read snapshot to upgrade from. Later
/// reads in the same transaction are fine.
///
/// `UPDATE … WHERE id IN (SELECT …)` is a write, not a read — the whole
/// statement takes the write lock.
///
/// # Errors
/// Returns [`VoomError::Database`] if the transaction cannot be opened.
pub async fn begin_write_first(
    pool: &SqlitePool,
    context: &'static str,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::database_context(context, e))
}

/// Open a transaction that only reads.
///
/// A read-only transaction never requests the write lock, so it never upgrades.
///
/// In WAL mode this reads the snapshot current when its first statement runs,
/// and does **not** wait for a writer that is mid-transaction. If the read is a
/// precondition that must observe that writer's outcome, it needs
/// [`begin_serialized_read`] instead.
///
/// # Errors
/// Returns [`VoomError::Database`] if the transaction cannot be opened.
pub async fn begin_read_only(
    pool: &SqlitePool,
    context: &'static str,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::database_context(context, e))
}

/// Open a read-only transaction that must order itself after any in-flight
/// writer.
///
/// Takes the write lock (`BEGIN IMMEDIATE`) despite never writing. In WAL mode
/// readers do not block on writers: a plain `BEGIN` would read the snapshot as
/// it stood *before* an uncommitted writer, which for a staleness or ownership
/// guard means passing on state the very next statement invalidates. Taking the
/// write lock makes this transaction queue behind that writer and read what it
/// committed.
///
/// Use it only where the read is a precondition on the latest committed state.
/// A guard that reads and then acts in a *separate* transaction still races —
/// this closes the stale-snapshot window, not the whole check-then-act one.
///
/// # Errors
/// Returns [`VoomError::Database`] if the transaction cannot be opened.
pub async fn begin_serialized_read(
    pool: &SqlitePool,
    context: &'static str,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| VoomError::database_context(context, e))
}
