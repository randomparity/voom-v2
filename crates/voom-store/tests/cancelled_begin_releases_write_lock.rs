#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]
#![expect(
    clippy::expect_used,
    reason = "the poll helper's wake guard names the future that never woke; a bare \
              Elapsed(()) panic would report only a line number"
)]
#![expect(
    clippy::panic,
    reason = "the observer must abort on a non-SQLITE_BUSY error rather than fold it \
              into a blocked observation, which would report success for the wrong reason"
)]

//! Regression proof for issue #592: cancelling a `BEGIN IMMEDIATE` open must not
//! strand `SQLite`'s write lock on a pooled connection.
//!
//! `sqlx` 0.8.6's `SqliteTransactionManager::begin` with a *custom* statement
//! (`sqlx-sqlite-0.8.6/src/transaction.rs:19-30`) runs the statement on the worker
//! thread — taking the write lock — and only *then* awaits `conn.lock_handle()` to
//! verify `in_transaction()`. A caller dropped in that window leaves no
//! `Transaction` value, so no `ROLLBACK` is ever queued, and `return_to_pool` only
//! pings. The connection goes back to the idle pool still holding the lock.
//!
//! Two arms, and the second is what keeps the first honest:
//!
//! * **fixed arm** — the same sweep through `begin_read_then_write`. The lock must
//!   be takeable at every *N*, in every repeat.
//! * **positive control** — a bare `pool.begin_with("BEGIN IMMEDIATE")` written
//!   here rather than called through the opener. At least one *N* across the
//!   repeats must leave the lock held. Without this arm the fixed arm is green
//!   whether or not the sweep still straddles a vulnerable window, so a dependency
//!   bump that moved the window outside 1..=8 would leave an assertion that passes
//!   while proving nothing.
//!
//! A red *control* means "check whether upstream closed the window", not "the fixed
//! arm stopped covering" — the two arms count different clocks.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::task::{Context, Wake, Waker};
use std::time::{Duration, Instant};

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Executor, SqlitePool};

use voom_store::test_support::fresh_initialized_pool_at;
use voom_store::tx::begin_read_then_write;
use voom_test_support::TempDatabase;

/// Wakeup-driven poll counts to sweep. The leak reproduces inside this range on the
/// warm fixture below; the control arm is what detects it if upstream ever moves it.
const POLL_SWEEP: std::ops::RangeInclusive<usize> = 1..=8;

/// Sweeps per arm. A single sweep is not a reliable observation — in the cold
/// fixture measured during design, 11 of 40 sweeps contained no leaking *N* at all,
/// so asserting "fails at some *N*" on one sweep would go red about a quarter of the
/// time for a reason unrelated to upstream. At the worst measured per-sweep miss
/// rate, five repeats put that at 0.275^5 = 0.0016.
const REPEATS: usize = 5;

/// Gap between the cancellation and the observer's first attempt. It stops the
/// observer taking and releasing the lock *before* the cancelled `BEGIN IMMEDIATE`
/// ever asks for it, which would report "not blocked" for the wrong reason.
const SETTLE: Duration = Duration::from_millis(100);

/// Retry ceiling for the fixed arm. It only has to exceed the latency of a detached
/// rollback — one queued statement on a worker thread, not a lock wait.
const FIXED_CEILING: Duration = Duration::from_secs(5);

/// Retry ceiling for the control arm, deliberately shorter. At a leaking *N* the
/// observer necessarily burns its whole ceiling, and this test lands in the
/// serialized instrumented `coverage` job whose duration is issue #592's subject.
/// The bound is not what discriminates here; the repeated `SQLITE_BUSY` is.
const CONTROL_CEILING: Duration = Duration::from_secs(1);

/// Pause between observer attempts.
const OBSERVER_BACKOFF: Duration = Duration::from_millis(25);

/// Upper bound on waiting for one wakeup in the poll helper, so a future that never
/// wakes fails the test instead of hanging the suite.
const WAKE_GUARD: Duration = Duration::from_secs(10);

/// A waker that signals rather than self-waking.
///
/// Self-waking is the trap here: a `cx.waker().wake_by_ref()` on `Pending` busy-spins
/// the polling loop, the sqlx worker thread never gets scheduled, and the sweep
/// measures nothing while appearing to work. Signalling means each poll in
/// [`poll_n_then_drop`] corresponds to one genuine step of progress, which is what
/// makes the *N* at which the leak appears reproducible rather than a timing sweep.
struct SignalWaker(tokio::sync::Notify);

impl Wake for SignalWaker {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

/// Poll `future` for exactly `polls` wakeup-driven polls, then drop it.
///
/// Returns `true` if the future was still pending when dropped. A future that
/// resolves before the count is reached returns `false` — that is not a failure,
/// it just means this *N* did not land inside the window.
async fn poll_n_then_drop<F: Future>(future: F, polls: usize) -> bool {
    let signal = Arc::new(SignalWaker(tokio::sync::Notify::new()));
    let waker = Waker::from(Arc::clone(&signal));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);

    for poll in 0..polls {
        if future.as_mut().poll(&mut context).is_ready() {
            return false;
        }
        if poll + 1 < polls {
            // `notify_one` before `notified()` stores a permit, so a wakeup that
            // races ahead of this await is not lost.
            tokio::time::timeout(WAKE_GUARD, signal.0.notified())
                .await
                .expect("a pending BEGIN IMMEDIATE never woke its caller");
        }
    }

    drop(future);
    true
}

/// One completed transaction through the pool.
///
/// The measured *N* table only reproduces in this warm shape: a cold pool spends
/// its first polls on connection establishment, which shifts where the window lands.
async fn warm_up(pool: &SqlitePool) {
    let transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    transaction.commit().await.unwrap();
}

/// Try once to take the write lock on `path` from an independent connection.
///
/// Independent is what makes the observation honest: a write issued back through the
/// *same* pool can be handed the leaked connection and silently join its open
/// transaction, reporting success while the lock is still stranded.
///
/// `busy_timeout(0)` is the other half. `voom_store::connect` sets 30s, at which
/// `SQLite` does not return on a held lock — it sleeps — so "blocked" would be
/// observable only as a wall-clock expiry, indistinguishable from a slow host. At 0
/// a held lock returns `SQLITE_BUSY` immediately, so every attempt is an observable,
/// attributable error rather than elapsed time.
async fn try_take_write_lock(path: &Path) -> Result<(), sqlx::Error> {
    let mut connection = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .busy_timeout(Duration::ZERO)
        .disable_statement_logging()
        .connect()
        .await?;

    let taken = connection.execute("BEGIN IMMEDIATE").await.map(|_| ());
    if taken.is_ok() {
        connection.execute("ROLLBACK").await?;
    }
    let _ = connection.close().await;
    taken
}

/// Whether `error` is `SQLite` reporting the lock is held by someone else.
fn is_busy(error: &sqlx::Error) -> bool {
    let sqlx::Error::Database(database) = error else {
        return false;
    };
    // SQLITE_BUSY is 5; the extended codes SQLITE_BUSY_SNAPSHOT (517) and
    // SQLITE_BUSY_RECOVERY (261) share the primary code and are matched by prefix.
    database
        .code()
        .is_some_and(|code| code == "5" || code == "517" || code == "261")
}

/// Retry the write lock until it is takeable or `ceiling` elapses.
///
/// Returns `true` if the lock became takeable. An error that is not `SQLITE_BUSY`
/// panics rather than being folded into "blocked": a poisoned connection fails with
/// `InvalidSavePointStatement` *without taking any lock*, and silently counting that
/// as an observation is how this test would report success for the wrong reason.
async fn lock_becomes_takeable(path: &Path, ceiling: Duration) -> bool {
    let deadline = Instant::now() + ceiling;
    loop {
        match try_take_write_lock(path).await {
            Ok(()) => return true,
            Err(error) if is_busy(&error) => {
                if Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(OBSERVER_BACKOFF).await;
            }
            Err(error) => {
                panic!("observer failed for a reason other than SQLITE_BUSY: {error}")
            }
        }
    }
}

/// A fresh database, pool and warm-up, per *N*, per arm, per repeat.
///
/// Sharing is a correctness bug here, not untidiness. An abandoned lock is held
/// until the process exits, so within a shared file the control arm's first leaking
/// *N* poisons every later *N* — and the fixed arm too, if they share — turning the
/// regression proof red for a reason unrelated to the fix. The poisoned connection
/// also returns to the idle pool, where a later `BEGIN IMMEDIATE` handed that
/// connection fails immediately without taking any lock.
async fn fresh_fixture() -> (TempDatabase, SqlitePool) {
    let database = TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(database.path()).await.unwrap();
    warm_up(&pool).await;
    (database, pool)
}

/// The fix: a cancelled open through the opener always leaves the lock takeable.
///
/// This is a regression assertion and nothing more. It does not claim to cancel a
/// post-fix open mid-flight — it cannot: after the fix the caller's only await is on
/// the channel, whose only wakeup source fires *after* the open has returned. So at
/// every *N* in this sweep the transaction is fully open before the caller goes away.
/// Cancellation *during* an open is covered deterministically by the orphan arm.
#[tokio::test(flavor = "multi_thread")]
async fn cancelled_open_through_the_opener_leaves_the_write_lock_takeable() {
    for repeat in 1..=REPEATS {
        for polls in POLL_SWEEP {
            let (database, pool) = fresh_fixture().await;

            poll_n_then_drop(
                begin_read_then_write(&pool, "issue 592 regression sweep"),
                polls,
            )
            .await;

            tokio::time::sleep(SETTLE).await;

            assert!(
                lock_becomes_takeable(database.path(), FIXED_CEILING).await,
                "repeat {repeat}, N={polls}: the write lock was still held \
                 {FIXED_CEILING:?} after a cancelled begin_read_then_write; the \
                 cancelled open stranded it on a pooled connection"
            );

            drop(pool);
        }
    }
}

/// Positive control: the unfixed shape still leaks, so the sweep still straddles the
/// window the fixed arm claims to cover.
///
/// Gated on parallelism as a silent early return. The cancellation has to land
/// between the worker thread taking the lock and the caller's next poll, and on a
/// host with too few cores the worker does not get scheduled inside that window —
/// the arm would then fail for a reason that says nothing about the code. No skip
/// notice is printed: libtest hides a passing test's output unless `--nocapture` is
/// passed, which neither `just ci` nor the coverage job does, so the message would be
/// invisible to every CI reader while costing a `print_stderr` lint expectation.
#[tokio::test(flavor = "multi_thread")]
async fn bare_begin_immediate_still_leaks_the_write_lock_when_cancelled() {
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
    if cores < 4 {
        return;
    }

    let mut blocked_observations = 0_usize;

    for _ in 1..=REPEATS {
        for polls in POLL_SWEEP {
            let (database, pool) = fresh_fixture().await;

            poll_n_then_drop(pool.begin_with("BEGIN IMMEDIATE"), polls).await;

            tokio::time::sleep(SETTLE).await;

            if !lock_becomes_takeable(database.path(), CONTROL_CEILING).await {
                blocked_observations += 1;
            }

            drop(pool);
        }
    }

    assert!(
        blocked_observations > 0,
        "no cancelled bare BEGIN IMMEDIATE stranded the write lock across {REPEATS} \
         sweeps of N in 1..=8. The regression sweep is no longer straddling the \
         window it exists to cover — check whether sqlx closed it upstream, and \
         re-derive the poll range before trusting the fixed arm"
    );
}

// ---------------------------------------------------------------------------
// Orphan arm
// ---------------------------------------------------------------------------

/// `context` passed by the orphan arm, and the string its captured `warn` is matched
/// on. It is unique to this arm deliberately: the fixed arm above can also orphan an
/// open, and these tests share one process and one global subscriber, so matching on
/// the message alone would let a concurrent arm satisfy this arm's assertion.
const ORPHAN_CONTEXT: &str = "issue 592 orphan arm";

/// How long the orphan arm gives its caller before cancelling it. Any value below
/// the pool's 30s `busy_timeout` works: while the holder keeps the write lock the
/// detached `BEGIN IMMEDIATE` physically cannot return, so the timeout is guaranteed
/// to fire rather than merely likely to.
const ORPHAN_TIMEOUT: Duration = Duration::from_millis(200);

/// Poll interval while waiting for a captured log line.
const CAPTURE_POLL: Duration = Duration::from_millis(20);

/// A `tracing` writer that accumulates into a shared buffer the test can read.
#[derive(Clone, Default)]
struct CaptureBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CaptureBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for CaptureBuffer {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

/// Install the capture subscriber once and return its buffer.
///
/// Global because `tracing`'s dispatcher is, and `with_default` would not work here:
/// it is thread-local, and the event this arm waits for is emitted from a spawned
/// task on another worker thread.
fn capture() -> &'static CaptureBuffer {
    static CAPTURE: std::sync::OnceLock<CaptureBuffer> = std::sync::OnceLock::new();
    CAPTURE.get_or_init(|| {
        let buffer = CaptureBuffer::default();
        let _ = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_env_filter(tracing_subscriber::EnvFilter::new("voom_store=warn"))
            .with_ansi(false)
            .try_init();
        buffer
    })
}

/// Wait for `needle` to appear in the captured log, up to `ceiling`.
async fn wait_for_captured(needle: &str, ceiling: Duration) -> bool {
    let deadline = Instant::now() + ceiling;
    loop {
        {
            let bytes = capture().0.lock().unwrap();
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(CAPTURE_POLL).await;
    }
}

/// A caller cancelled *during* an open still has its transaction rolled back.
///
/// This arm exists because the poll sweep above cannot reach that case, and does not
/// claim to: after the fix the caller's only await is on the channel, whose only
/// wakeup fires *after* the open has returned, so every *N* in that sweep cancels a
/// caller whose transaction is already fully open.
///
/// Determinism comes from `SQLite`'s lock rather than from timing. While the holder
/// keeps the write lock the detached `BEGIN IMMEDIATE` physically cannot return, so
/// the short timeout is guaranteed to drop the caller mid-open.
///
/// Waiting for the `warn` before probing the lock is what makes the arm mean
/// anything, and it replaces an earlier claim that was measured false. The observer
/// *can* slip in front of the detached opener — it did 20 times out of 20 — because
/// the opener parks in `SQLite`'s busy handler, which sleeps. Probing first would
/// therefore report a takeable lock while the orphan was still queued behind it,
/// passing for a reason unrelated to the rollback.
#[tokio::test(flavor = "multi_thread")]
async fn an_orphaned_open_rolls_itself_back() {
    capture();

    let database = TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(database.path()).await.unwrap();
    warm_up(&pool).await;

    let holder = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();

    let cancelled =
        tokio::time::timeout(ORPHAN_TIMEOUT, begin_read_then_write(&pool, ORPHAN_CONTEXT)).await;
    assert!(
        cancelled.is_err(),
        "the open returned while the holder still had the write lock, so this arm \
         never orphaned anything and proves nothing"
    );

    holder.rollback().await.unwrap();

    assert!(
        wait_for_captured(ORPHAN_CONTEXT, FIXED_CEILING).await,
        "no orphan warning naming {ORPHAN_CONTEXT:?} was captured within \
         {FIXED_CEILING:?} of the holder releasing the lock; the detached open \
         never noticed it had been abandoned"
    );

    assert!(
        lock_becomes_takeable(database.path(), FIXED_CEILING).await,
        "the orphaned open logged that it was rolling back, but the write lock was \
         still held {FIXED_CEILING:?} later"
    );
}
