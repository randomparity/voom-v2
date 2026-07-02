use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Successful backup facts: the byte length copied and the `blake3:` checksum
/// of the source contents (which, on success, equal the destination).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackUpOutcome {
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupIoError {
    /// The source could not be read (missing, not a regular file, I/O error).
    ArtifactUnavailable(String),
    /// The destination could not be written, or a matching destination could
    /// not be confirmed (write/fsync/rename error, or a pre-existing
    /// destination whose contents differ from the source).
    BackupFailure(String),
}

impl Display for BackupIoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArtifactUnavailable(message) => write!(f, "artifact unavailable: {message}"),
            Self::BackupFailure(message) => write!(f, "backup failed: {message}"),
        }
    }
}

impl std::error::Error for BackupIoError {}

/// Copy `source` to `destination`, computing its size and BLAKE3 checksum and
/// fsyncing the copy for durability.
///
/// Idempotent: if `destination` already exists with matching size+checksum, the
/// existing copy is accepted and no bytes are rewritten (so a retried dispatch
/// is a no-op). A pre-existing destination whose contents differ is a
/// [`BackupIoError::BackupFailure`] — the worker never overwrites a
/// non-matching file.
///
/// # Errors
/// [`BackupIoError::ArtifactUnavailable`] when the source is missing or
/// unreadable; [`BackupIoError::BackupFailure`] for destination write/rename/
/// fsync errors or a mismatched pre-existing destination.
pub async fn back_up_file(
    source: &Path,
    destination: &Path,
) -> Result<BackUpOutcome, BackupIoError> {
    ensure_regular_source(source).await?;

    let parent = destination.parent().ok_or_else(|| {
        BackupIoError::BackupFailure(format!(
            "destination path {} has no parent directory",
            destination.display()
        ))
    })?;
    tokio::fs::create_dir_all(parent).await.map_err(|err| {
        BackupIoError::BackupFailure(format!(
            "create destination dir {}: {err}",
            parent.display()
        ))
    })?;

    if tokio::fs::symlink_metadata(destination).await.is_ok() {
        return reconcile_existing_destination(source, destination).await;
    }

    let outcome = copy_into_temp_then_promote(source, destination, parent).await?;
    Ok(outcome)
}

async fn ensure_regular_source(source: &Path) -> Result<(), BackupIoError> {
    let metadata = tokio::fs::symlink_metadata(source)
        .await
        .map_err(|err| source_unavailable(source, err))?;
    if !metadata.is_file() {
        return Err(BackupIoError::ArtifactUnavailable(format!(
            "source path {} is not a regular file",
            source.display()
        )));
    }
    Ok(())
}

async fn reconcile_existing_destination(
    source: &Path,
    destination: &Path,
) -> Result<BackUpOutcome, BackupIoError> {
    let source_facts = hash_source(source).await?;
    let dest_facts = hash_destination(destination).await?;
    if source_facts == dest_facts {
        return Ok(source_facts);
    }
    Err(BackupIoError::BackupFailure(format!(
        "destination {} already exists with different content",
        destination.display()
    )))
}

async fn copy_into_temp_then_promote(
    source: &Path,
    destination: &Path,
    parent: &Path,
) -> Result<BackUpOutcome, BackupIoError> {
    let temp = temp_path_for(destination);
    let outcome = match copy_source_to_temp(source, &temp).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(err);
        }
    };

    if let Err(err) = tokio::fs::rename(&temp, destination).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(BackupIoError::BackupFailure(format!(
            "promote backup {} -> {}: {err}",
            temp.display(),
            destination.display()
        )));
    }
    fsync_dir(parent).await?;
    Ok(outcome)
}

async fn copy_source_to_temp(source: &Path, temp: &Path) -> Result<BackUpOutcome, BackupIoError> {
    let mut reader = open_regular_no_follow(source).await?;
    let mut writer = tokio::fs::File::create(temp).await.map_err(|err| {
        BackupIoError::BackupFailure(format!("create temp {}: {err}", temp.display()))
    })?;

    let mut hasher = blake3::Hasher::new();
    let mut size: u64 = 0;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|err| source_unavailable(source, err))?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).await.map_err(|err| {
            BackupIoError::BackupFailure(format!("write temp {}: {err}", temp.display()))
        })?;
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    writer.flush().await.map_err(|err| {
        BackupIoError::BackupFailure(format!("flush temp {}: {err}", temp.display()))
    })?;
    writer.sync_all().await.map_err(|err| {
        BackupIoError::BackupFailure(format!("fsync temp {}: {err}", temp.display()))
    })?;

    Ok(BackUpOutcome {
        size_bytes: size,
        checksum: format!("blake3:{}", hasher.finalize().to_hex()),
    })
}

async fn hash_source(source: &Path) -> Result<BackUpOutcome, BackupIoError> {
    hash_stream(source, source_unavailable).await
}

async fn hash_destination(destination: &Path) -> Result<BackUpOutcome, BackupIoError> {
    hash_stream(destination, |path, err| {
        BackupIoError::BackupFailure(format!("read destination {}: {err}", path.display()))
    })
    .await
}

async fn hash_stream(
    path: &Path,
    on_read_error: impl Fn(&Path, std::io::Error) -> BackupIoError,
) -> Result<BackUpOutcome, BackupIoError> {
    let mut file = open_regular_no_follow(path)
        .await
        .map_err(|err| map_open_error(err, path, &on_read_error))?;
    let mut hasher = blake3::Hasher::new();
    let mut size: u64 = 0;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|err| on_read_error(path, err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok(BackUpOutcome {
        size_bytes: size,
        checksum: format!("blake3:{}", hasher.finalize().to_hex()),
    })
}

fn map_open_error(
    err: BackupIoError,
    path: &Path,
    on_read_error: &impl Fn(&Path, std::io::Error) -> BackupIoError,
) -> BackupIoError {
    // `open_regular_no_follow` always yields an ArtifactUnavailable; re-tag it
    // with the caller's classification (source vs destination read).
    match err {
        BackupIoError::ArtifactUnavailable(message) | BackupIoError::BackupFailure(message) => {
            on_read_error(path, std::io::Error::other(message))
        }
    }
}

fn temp_path_for(destination: &Path) -> PathBuf {
    let mut name: OsString = destination.as_os_str().to_owned();
    name.push(".voom-backup-partial");
    PathBuf::from(name)
}

fn source_unavailable(path: &Path, err: impl Display) -> BackupIoError {
    BackupIoError::ArtifactUnavailable(format!("source path {}: {err}", path.display()))
}

#[cfg(unix)]
async fn open_regular_no_follow(path: &Path) -> Result<tokio::fs::File, BackupIoError> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = path.to_owned();
    let open_path = path.clone();
    let std_file = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(open_path)
    })
    .await
    .map_err(|err| source_unavailable(&path, err))?
    .map_err(|err| source_unavailable(&path, err))?;
    Ok(tokio::fs::File::from_std(std_file))
}

#[cfg(not(unix))]
async fn open_regular_no_follow(path: &Path) -> Result<tokio::fs::File, BackupIoError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|err| source_unavailable(path, err))
}

#[cfg(unix)]
async fn fsync_dir(dir: &Path) -> Result<(), BackupIoError> {
    let dir = dir.to_owned();
    let sync_dir = dir.clone();
    tokio::task::spawn_blocking(move || std::fs::File::open(&sync_dir).and_then(|f| f.sync_all()))
        .await
        .map_err(|err| {
            BackupIoError::BackupFailure(format!("fsync dir join {}: {err}", dir.display()))
        })?
        .map_err(|err| BackupIoError::BackupFailure(format!("fsync dir {}: {err}", dir.display())))
}

#[cfg(not(unix))]
async fn fsync_dir(_dir: &Path) -> Result<(), BackupIoError> {
    Ok(())
}

#[cfg(test)]
#[path = "backup_test.rs"]
mod tests;
