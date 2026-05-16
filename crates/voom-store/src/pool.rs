use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, SqlitePool};
use voom_core::VoomError;

/// Open a `SQLite` pool against an existing database. **Never creates files or
/// directories.** Used by every read-side path; the explicit `connect_or_create`
/// is reserved for `init()`.
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
        .busy_timeout(std::time::Duration::from_millis(5000));

    if is_memory {
        options = options.shared_cache(true);
    }

    // Sprint 0 uses rollback-journal mode for all on-disk DBs. WAL would
    // create -wal/-shm sidecars that are visible even to readers, which
    // breaks the read-side no-filesystem-side-effects contract once a DB
    // has been initialized with WAL. Revisit when concurrent access pressure
    // is real (Sprint 6 daemon).

    let pool_size = if is_memory { 1 } else { 8 };

    options = options.disable_statement_logging();

    SqlitePoolOptions::new()
        .max_connections(pool_size)
        .min_connections(u32::from(is_memory))
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_in_memory_succeeds() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);
    }

    #[tokio::test]
    async fn connect_on_existing_disk_db_succeeds() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        connect_or_create(&url).await.unwrap();

        let pool = connect(&url).await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);
    }

    #[tokio::test]
    async fn connect_does_not_create_sqlx_migrations_table() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(
            exists.is_none(),
            "connect() must not create migration tracking table"
        );
    }

    #[tokio::test]
    async fn connect_refuses_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.db");
        let url = format!("sqlite://{}", missing.display());

        let err = connect(&url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
        assert!(
            !missing.exists(),
            "connect() must NOT create the database file"
        );
    }

    #[tokio::test]
    async fn connect_does_not_create_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("absent-dir/voom.db");
        assert!(!nested.parent().unwrap().exists());

        let url = format!("sqlite://{}", nested.display());
        let err = connect(&url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
        assert!(
            !nested.parent().unwrap().exists(),
            "connect() must NOT mkdir parents"
        );
        assert!(!nested.exists());
    }

    #[tokio::test]
    async fn connect_or_create_creates_missing_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c/voom.db");
        assert!(
            !nested.parent().unwrap().exists(),
            "parent must not exist yet"
        );

        let url = format!("sqlite://{}", nested.display());
        let pool = connect_or_create(&url).await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);

        assert!(
            nested.parent().unwrap().exists(),
            "connect_or_create() must mkdir -p the parent"
        );
        assert!(nested.exists(), "sqlite must have created the db file");
    }

    #[test]
    fn url_is_memory_recognizes_canonical_forms() {
        assert!(url_is_memory(":memory:"));
        assert!(url_is_memory("sqlite::memory:"));
        assert!(url_is_memory("sqlite::memory:?cache=shared"));
        assert!(url_is_memory("sqlite:///some.db?mode=memory"));
        assert!(url_is_memory("sqlite:///some.db?cache=shared&mode=memory"));
    }

    #[test]
    fn url_is_memory_rejects_adversarial_filenames() {
        // Substring matching used to misclassify these as memory DBs, which
        // would have flipped `create_if_missing` on for read-side `connect()`
        // and let it create files. The exact-match guard must reject them.
        assert!(!url_is_memory("sqlite:///tmp/foo:memory:bar.db"));
        assert!(!url_is_memory("sqlite:///srv/data/:memory:.sqlite"));
        assert!(!url_is_memory("sqlite:///:memory:trap.db"));
        assert!(!url_is_memory("sqlite:///some.db"));
        assert!(!url_is_memory("sqlite:///some.db?cache=shared"));
        // sqlx 0.8.6 treats `/:memory:` as an absolute file path; only the
        // bare `:memory:` form is in-memory. Misclassifying this would let
        // read-side connect() create a `/:memory:` file at the filesystem
        // root.
        assert!(!url_is_memory("sqlite:///:memory:"));
        assert!(!url_is_memory("/:memory:"));
    }

    #[tokio::test]
    async fn connect_refuses_adversarial_memory_lookalike_path() {
        // Regression for the previous substring-based is_memory check:
        // a path that contains ":memory:" as a literal filename fragment
        // is still on-disk, and read-side connect() must refuse to create
        // it.
        let tmp = tempfile::tempdir().unwrap();
        let trap = tmp.path().join("foo:memory:bar.db");
        let url = format!("sqlite://{}", trap.display());
        assert!(!trap.exists());

        let err = connect(&url).await.unwrap_err();
        assert_eq!(err.code(), "DB_UNREACHABLE");
        assert!(
            !trap.exists(),
            "connect() must NOT create files for paths containing ':memory:'"
        );
    }

    #[tokio::test]
    async fn neither_opener_creates_wal_or_shm_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("voom.db");
        let url = format!("sqlite://{}", db.display());

        {
            let pool = connect_or_create(&url).await.unwrap();
            sqlx::query("CREATE TABLE marker (id INTEGER)")
                .execute(&pool)
                .await
                .unwrap();
        }
        let wal = db.with_extension("db-wal");
        let shm = db.with_extension("db-shm");
        assert!(!wal.exists(), "connect_or_create() must not produce -wal");
        assert!(!shm.exists(), "connect_or_create() must not produce -shm");

        let pool = connect(&url).await.unwrap();
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM marker")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!wal.exists(), "connect() must not produce -wal");
        assert!(!shm.exists(), "connect() must not produce -shm");
    }
}
