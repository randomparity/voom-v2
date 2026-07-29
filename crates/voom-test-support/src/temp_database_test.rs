use std::fs;

use super::*;

#[tokio::test]
async fn drop_removes_sqlite_database_and_every_sidecar() -> Result<(), Box<dyn std::error::Error>>
{
    let parent = tempfile::tempdir()?;
    let database = TempDatabase::new_in(parent.path())?;
    let url = format!("sqlite://{}", database.path().display());

    voom_store::init(&url).await?;
    fs::write(
        format!("{}-journal", database.path().display()),
        b"rollback",
    )?;
    fs::write(format!("{}-wal", database.path().display()), b"wal")?;
    fs::write(
        format!("{}-shm", database.path().display()),
        b"shared memory",
    )?;

    drop(database);

    if parent.path().read_dir()?.next().is_some() {
        return Err(std::io::Error::other(
            "dropping TempDatabase left SQLite files in its parent directory",
        )
        .into());
    }
    Ok(())
}
