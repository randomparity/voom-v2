use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{ConnectOptions, SqlitePool};
use voom_core::VoomError;

/// Open a `SQLite` pool against an existing database. **Never creates the main
/// database file or parent directories.** On-disk pools may create normal `SQLite`
/// WAL sidecars (`-wal`/`-shm`). Used by every read-side path; the explicit
/// `connect_or_create` is reserved for `init()`.
pub async fn connect(url: &str) -> Result<SqlitePool, VoomError> {
    connect_inner(url, /* create = */ false).await
}

/// Open a `SQLite` pool, creating the database file and any missing parent
/// directories. Only `init()` should call this.
pub async fn connect_or_create(url: &str) -> Result<SqlitePool, VoomError> {
    connect_inner(url, /* create = */ true).await
}

async fn connect_inner(url: &str, create: bool) -> Result<SqlitePool, VoomError> {
    let is_memory = url_is_memory(url);

    if create && !is_memory {
        ensure_parent_dir(url)?;
    }

    let mut options: SqliteConnectOptions = url
        .parse()
        .map_err(|e| VoomError::Database(format!("invalid sqlite url {url:?}: {e}")))?;

    options = options
        .create_if_missing(create || is_memory)
        .foreign_keys(true)
        // Lock-wait budget: wait out transient write contention instead of
        // surfacing a spurious SQLITE_BUSY -> DB_UNREACHABLE. 30s is deliberately
        // generous because a lock holder can be descheduled for several seconds
        // under load (first observed as flakes on a CPU-starved parallel test
        // suite). Genuine deadlocks are reported separately and do not consume
        // this budget, so the longer wait only affects real, transient contention.
        .busy_timeout(std::time::Duration::from_secs(30));

    if is_memory {
        options = options.shared_cache(true);
    } else {
        options = options.journal_mode(SqliteJournalMode::Wal);
    }

    // On-disk pools use WAL so operator processes can read committed snapshots
    // while another process is writing. This may create normal SQLite -wal/-shm
    // sidecars for existing DB files; connect() still never creates the main DB
    // file, parent directories, migration rows, or schema.

    let pool_size = if is_memory { 1 } else { 8 };

    options = options.disable_statement_logging();

    SqlitePoolOptions::new()
        .max_connections(pool_size)
        .min_connections(u32::from(is_memory))
        // Bound the wait for a free pooled connection. With a single SQLite
        // writer, write transactions can each hold their connection for up to
        // `busy_timeout` while waiting out lock contention (more so for the
        // `BEGIN IMMEDIATE` paths, which hold the connection across the whole
        // lock wait). Without this, a saturated pool makes new callers block
        // forever; this surfaces a clean error instead. Sized at `busy_timeout`
        // plus headroom so it only fires under genuine sustained saturation.
        .acquire_timeout(std::time::Duration::from_secs(45))
        .connect_with(options)
        .await
        .map_err(|e| {
            VoomError::Database(format!(
                "pool open failed for {url:?} (create={create}): {e}"
            ))
        })
}

/// Extract the filesystem path from a `sqlite:` URL and create any missing
/// parent directories. Accepts `sqlite:///abs/path`, `sqlite://relative/path`,
/// `sqlite:/abs/path`, and bare `path` forms.
/// Recognize `SQLite` memory-DB URLs exactly as sqlx does — never via substring
/// match. A legitimate on-disk path like `/tmp/foo:memory:bar.db` is NOT a
/// memory URL, and the read-side `connect()` must keep its no-create
/// invariant for such paths.
///
/// sqlx 0.8.6 only special-cases the raw database name `:memory:`. The
/// slash-prefixed `/:memory:` is an absolute path to a file named
/// `:memory:` and must remain on-disk; classifying it as memory would let
/// `create_if_missing` create that file from a read-side path.
///
/// Accepted memory forms:
///   * `:memory:`
///   * `sqlite::memory:`
///   * Either of the above with a `?cache=…` query string
///   * Any URL whose query string contains `mode=memory`
fn url_is_memory(url: &str) -> bool {
    let stripped = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);
    let (path, query) = stripped.split_once('?').unwrap_or((stripped, ""));
    let bare_memory = path == ":memory:";
    let mode_memory = query.split('&').any(|pair| pair == "mode=memory");
    bare_memory || mode_memory
}

fn ensure_parent_dir(url: &str) -> Result<(), VoomError> {
    let path_str = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);

    let path_str = path_str.split('?').next().unwrap_or(path_str);

    if path_str.is_empty() {
        return Ok(());
    }

    let path = Path::new(path_str);
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|e| {
        VoomError::Database(format!(
            "could not create database parent directory {}: {e}",
            parent.display()
        ))
    })
}

#[cfg(test)]
#[path = "pool_test.rs"]
mod tests;
