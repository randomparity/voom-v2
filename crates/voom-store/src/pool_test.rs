use super::*;

#[tokio::test]
async fn connect_in_memory_succeeds() {
    let pool = connect("sqlite::memory:").await.unwrap();
    let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
    assert_eq!(row.0, 1);
    assert_eq!(journal_mode(&pool).await, "memory");
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

#[tokio::test]
async fn connect_configures_busy_timeout_that_survives_cpu_starvation() {
    // Regression: under a CPU-starved parallel test suite a lock holder can be
    // descheduled past the lock-wait budget, so a waiting writer gets
    // SQLITE_BUSY -> DB_UNREACHABLE. The budget must be generous enough that
    // transient starvation does not surface as a spurious failure.
    let pool = connect("sqlite::memory:").await.unwrap();
    let busy_timeout_ms: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        busy_timeout_ms >= 30_000,
        "busy_timeout must survive CI CPU starvation; was {busy_timeout_ms}ms"
    );
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
async fn on_disk_openers_use_wal_journal_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("voom.db");
    let url = format!("sqlite://{}", db.display());

    {
        let pool = connect_or_create(&url).await.unwrap();
        assert_eq!(journal_mode(&pool).await, "wal");
        sqlx::query("CREATE TABLE marker (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
    }

    let pool = connect(&url).await.unwrap();
    assert_eq!(journal_mode(&pool).await, "wal");
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM marker")
        .fetch_one(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn on_disk_wal_allows_writer_commit_while_reader_transaction_is_open() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("voom.db");
    let url = format!("sqlite://{}", db.display());
    let setup = connect_or_create(&url).await.unwrap();
    sqlx::query("CREATE TABLE marker (id INTEGER PRIMARY KEY)")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("INSERT INTO marker (id) VALUES (1)")
        .execute(&setup)
        .await
        .unwrap();

    let reader = connect(&url).await.unwrap();
    let writer = connect(&url).await.unwrap();
    let mut reader_conn = reader.acquire().await.unwrap();
    sqlx::query("BEGIN")
        .execute(&mut *reader_conn)
        .await
        .unwrap();
    let visible_to_reader: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM marker")
        .fetch_one(&mut *reader_conn)
        .await
        .unwrap();
    assert_eq!(visible_to_reader, 1);

    let mut writer_conn = writer.acquire().await.unwrap();
    sqlx::query("PRAGMA busy_timeout = 0")
        .execute(&mut *writer_conn)
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *writer_conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO marker (id) VALUES (2)")
        .execute(&mut *writer_conn)
        .await
        .unwrap();
    sqlx::query("COMMIT")
        .execute(&mut *writer_conn)
        .await
        .unwrap();

    let still_reader_snapshot: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM marker")
        .fetch_one(&mut *reader_conn)
        .await
        .unwrap();
    assert_eq!(still_reader_snapshot, 1);
    sqlx::query("COMMIT")
        .execute(&mut *reader_conn)
        .await
        .unwrap();

    let visible_after_reader_commit: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM marker")
        .fetch_one(&reader)
        .await
        .unwrap();
    assert_eq!(visible_after_reader_commit, 2);
}

async fn journal_mode(pool: &sqlx::SqlitePool) -> String {
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .unwrap();
    mode.to_ascii_lowercase()
}
