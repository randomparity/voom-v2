use std::fs;

use super::*;

/// The repo pins `TMPDIR` to `.test-tmp/` in `.cargo/config.toml` so every host
/// runs these tests on real storage. Without it a workstation with a tmpfs
/// `/tmp` -- the Fedora and Ubuntu 24.04+ default -- gets a nearly free `fsync`
/// while CI and macOS pay milliseconds, and `fsync` duration is how long a
/// `SQLite` write lock is held. That divergence is invisible at the call site, so
/// it gets a test rather than a comment. See
/// `docs/adr/0079-deterministic-test-temp-root.md`.
#[test]
fn temp_databases_land_on_the_pinned_repo_local_root() -> Result<(), Box<dyn std::error::Error>> {
    let database = TempDatabase::new()?;
    let path = database.path().canonicalize()?;

    // Compare against the manifest dir rather than the current directory: cargo
    // sets CARGO_MANIFEST_DIR per crate, and a test's working directory is the
    // crate root, not the workspace root.
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .ok_or_else(|| std::io::Error::other("crate is not two levels below the workspace root"))?;
    let expected_root = workspace.join(".test-tmp").canonicalize()?;

    if !path.starts_with(&expected_root) {
        return Err(std::io::Error::other(format!(
            "TMPDIR is not pinned to the repo-local root: {} is outside {}. \
             Check the [env] table in .cargo/config.toml.",
            path.display(),
            expected_root.display()
        ))
        .into());
    }
    Ok(())
}

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
