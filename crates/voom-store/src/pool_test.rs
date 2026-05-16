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
