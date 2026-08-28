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
//! **Opening a `BEGIN IMMEDIATE` transaction is cancellation-safe**: the open runs
//! to completion whether or not its caller is still waiting, so a cancelled caller
//! can no longer strand the write lock on a pooled connection. The two openers that
//! issue a custom statement get that from the private `begin_detached` helper; the
//! two deferred-`BEGIN` openers need nothing, because a deferred `BEGIN` takes no
//! lock and the worker's rendezvous acknowledgement already rolls back a cancelled
//! open. See [ADR 0087].
//!
//! [ADR 0083]: https://github.com/randomparity/voom-v2/blob/main/docs/adr/0083-read-then-write-transactions-begin-immediate.md
//! [ADR 0086]: https://github.com/randomparity/voom-v2/blob/main/docs/adr/0086-transaction-openers-are-named-helpers.md
//! [ADR 0087]: https://github.com/randomparity/voom-v2/blob/main/docs/adr/0087-cancellation-safe-begin-immediate.md

use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::sync::oneshot;
use tracing::Instrument;
use voom_core::VoomError;

/// Open a transaction on a detached task and take its result over a channel.
///
/// This is the fix for issue #592. `sqlx` 0.8.6's `SqliteTransactionManager::begin`
/// with a *custom* statement (`sqlx-sqlite-0.8.6/src/transaction.rs:19-30`) runs the
/// statement on the worker thread — taking `SQLite`'s write lock — and only *then*
/// awaits `conn.lock_handle()` to confirm `in_transaction()`. A caller dropped in
/// that window leaves no `Transaction` value, so no `ROLLBACK` is ever queued;
/// `return_to_pool` (`sqlx-core-0.8.6/src/pool/connection.rs:275-325`) only pings,
/// so the connection returns to the idle pool still holding the lock and every
/// subsequent writer blocks until the process exits.
///
/// Spawning breaks that: a spawned task outlives the future that spawned it, so a
/// cancelled caller leaves a task that still constructs the `Transaction` — and then
/// drops it, which queues the `ROLLBACK` the leak was missing.
///
/// The `oneshot` rather than the `JoinHandle` is what lets the detached side notice
/// it was orphaned and say so. Dropping a `JoinHandle` also detaches and would fix
/// the leak, but nothing there distinguishes a task whose caller is gone from one
/// whose caller is still waiting, so there would be no place to log and no
/// `send`-failure branch for a test to assert on. See ADR 0087.
async fn begin_detached(
    pool: &SqlitePool,
    statement: &'static str,
    context: &'static str,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    let pool = pool.clone();
    let (sender, receiver) = oneshot::channel();

    tokio::spawn(
        async move {
            // Timed because the orphan `warn` is this design's only observable for
            // the pool-slot residual it accepts, and a bare count cannot tell a
            // microsecond open under no contention from one parked in `SQLite`'s busy
            // handler for the full `busy_timeout`. Those are the benign and the
            // worrying case, and without a duration they log identically.
            let started = std::time::Instant::now();
            let opened = pool.begin_with(statement).await;
            let held_ms = started.elapsed().as_millis();
            // `send` returns the value back when the receiver is gone, which is
            // exactly the orphan case: the caller was cancelled while this open was
            // in flight. Dropping `unsent` here is what performs the rollback.
            if let Err(unsent) = sender.send(opened) {
                match &unsent {
                    Ok(_) => tracing::warn!(
                        context,
                        held_ms,
                        "transaction open completed after its caller was cancelled; rolling back"
                    ),
                    Err(error) => tracing::warn!(
                        context,
                        held_ms,
                        %error,
                        "transaction open failed for a caller that was already cancelled"
                    ),
                }
            }
        }
        // Carry the caller's span so the orphan warning is attributable to the
        // request that abandoned it rather than appearing at the root.
        .instrument(tracing::Span::current()),
    );

    receiver
        .await
        .map_err(|e| VoomError::database_context(context, e))?
        .map_err(|e| VoomError::database_context(context, e))
}

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
    begin_detached(pool, "BEGIN IMMEDIATE", context).await
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
    begin_detached(pool, "BEGIN IMMEDIATE", context).await
}
