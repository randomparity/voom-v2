use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use voom_core::VoomError;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFileFacts {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<SystemTime>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionReport {
    pub staging: ArtifactFileFacts,
    pub target: ArtifactFileFacts,
    pub temp_path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct PromotionFailpointContext<'a> {
    pub temp_path: &'a Path,
    pub target_path: &'a Path,
}

pub trait PromotionFailpoint: Send + Sync {
    fn before_install(&self, _context: PromotionFailpointContext<'_>) -> Result<(), VoomError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NoPromotionFailpoint;

impl PromotionFailpoint for NoPromotionFailpoint {}

pub async fn canonical_existing_file_no_symlink(
    path: impl AsRef<Path>,
) -> Result<PathBuf, VoomError> {
    let path = path.as_ref();
    reject_symlink_components(path).await?;
    let metadata = fs::symlink_metadata(path).await.map_err(|err| {
        config(format!(
            "artifact path must exist: {}: {err}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(config(format!(
            "artifact path must not be a symlink: {}",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(config(format!(
            "artifact path must be a regular file: {}",
            path.display()
        )));
    }
    fs::canonicalize(path).await.map_err(|err| {
        config(format!(
            "cannot canonicalize artifact path {}: {err}",
            path.display()
        ))
    })
}

pub async fn canonical_new_leaf_no_symlink(path: impl AsRef<Path>) -> Result<PathBuf, VoomError> {
    let path = path.as_ref();
    let file_name = path.file_name().ok_or_else(|| {
        config(format!(
            "artifact path must include a file name: {}",
            path.display()
        ))
    })?;
    match fs::symlink_metadata(path).await {
        Ok(_) => {
            return Err(config(format!(
                "artifact path must not already exist: {}",
                path.display()
            )));
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(config(format!(
                "cannot inspect artifact path {}: {err}",
                path.display()
            )));
        }
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    reject_symlink_components(parent).await?;
    let parent_metadata = fs::symlink_metadata(parent).await.map_err(|err| {
        config(format!(
            "artifact parent must exist: {}: {err}",
            parent.display()
        ))
    })?;
    if parent_metadata.file_type().is_symlink() {
        return Err(config(format!(
            "artifact parent must not be a symlink: {}",
            parent.display()
        )));
    }
    if !parent_metadata.is_dir() {
        return Err(config(format!(
            "artifact parent must be a directory: {}",
            parent.display()
        )));
    }

    let parent = fs::canonicalize(parent).await.map_err(|err| {
        config(format!(
            "cannot canonicalize artifact parent {}: {err}",
            parent.display()
        ))
    })?;
    Ok(parent.join(file_name))
}

pub async fn observe_regular_file(path: impl AsRef<Path>) -> Result<ArtifactFileFacts, VoomError> {
    let path = path.as_ref();
    let path_metadata = fs::symlink_metadata(path).await.map_err(|err| {
        VoomError::ArtifactUnavailable(format!(
            "cannot inspect artifact path {}: {err}",
            path.display()
        ))
    })?;
    if path_metadata.file_type().is_symlink() {
        return Err(VoomError::ArtifactUnavailable(format!(
            "artifact path must not be a symlink: {}",
            path.display()
        )));
    }
    if !path_metadata.is_file() {
        return Err(VoomError::ArtifactUnavailable(format!(
            "artifact path must be a regular file: {}",
            path.display()
        )));
    }

    let canonical = fs::canonicalize(path).await.map_err(|err| {
        VoomError::ArtifactUnavailable(format!(
            "cannot canonicalize artifact path {}: {err}",
            path.display()
        ))
    })?;
    let mut file = open_regular_file_no_follow(&canonical).await?;
    let metadata = file.metadata().await.map_err(|err| {
        VoomError::ArtifactUnavailable(format!(
            "cannot inspect artifact path {}: {err}",
            canonical.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(VoomError::ArtifactUnavailable(format!(
            "artifact path must be a regular file: {}",
            canonical.display()
        )));
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).await.map_err(|err| {
            VoomError::ArtifactUnavailable(format!(
                "cannot read artifact path {}: {err}",
                canonical.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    let final_metadata = file.metadata().await.map_err(|err| {
        VoomError::ArtifactUnavailable(format!(
            "cannot inspect artifact path {} after read: {err}",
            canonical.display()
        ))
    })?;
    if metadata_changed(&metadata, &final_metadata) {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "artifact changed while reading it: {}",
            canonical.display()
        )));
    }

    #[cfg(unix)]
    let local_file_key = Some(local_file_key(&metadata));
    #[cfg(not(unix))]
    let local_file_key = None;

    Ok(ArtifactFileFacts {
        path: canonical,
        size_bytes: metadata.len(),
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
        modified_at: metadata.modified().ok(),
        local_file_key,
    })
}

pub fn unique_temp_sibling_path(final_path: impl AsRef<Path>) -> Result<PathBuf, VoomError> {
    let final_path = final_path.as_ref();
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = final_path.file_name().ok_or_else(|| {
        config(format!(
            "artifact path must include a file name: {}",
            final_path.display()
        ))
    })?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = OsString::from(".voom-tmp.");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.{}", std::process::id(), counter));
    Ok(parent.join(temp_name))
}

pub async fn copy_regular_file_checked(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<ArtifactFileFacts, VoomError> {
    let source = observe_regular_file(source).await?;
    let destination = canonical_new_leaf_no_symlink(destination).await?;

    if let Err(err) = copy_regular_file_contents(&source.path, &destination).await {
        return match err {
            CopyFileError::NotCreated(err) => Err(err),
            CopyFileError::Created(err) => {
                Err(remove_new_file_after_error(&destination, err).await)
            }
        };
    }
    if let Err(err) = fsync_file(&destination).await {
        return Err(remove_new_file_after_error(&destination, err).await);
    }

    let copied = match observe_regular_file(&destination).await {
        Ok(facts) => facts,
        Err(err) => return Err(remove_new_file_after_error(&destination, err).await),
    };
    if !same_file_facts(&source, &copied) {
        return Err(remove_new_file_after_error(
            &destination,
            VoomError::ArtifactChecksumMismatch(format!(
                "copied artifact facts do not match source: {}",
                destination.display()
            )),
        )
        .await);
    }

    Ok(copied)
}

#[cfg(test)]
pub async fn copy_to_unique_temp_then_install_no_replace(
    source: impl AsRef<Path>,
    final_path: impl AsRef<Path>,
) -> Result<ArtifactFileFacts, VoomError> {
    let expected = observe_regular_file(source.as_ref()).await?;
    let report =
        promote_staged_add_only(source, final_path, &expected, &NoPromotionFailpoint).await?;
    Ok(report.target)
}

pub async fn promote_staged_add_only(
    staging: impl AsRef<Path>,
    target: impl AsRef<Path>,
    expected: &ArtifactFileFacts,
    failpoint: &dyn PromotionFailpoint,
) -> Result<PromotionReport, VoomError> {
    let staging = staging.as_ref();
    let target = canonical_new_leaf_no_symlink(target).await?;
    let staging_facts = require_expected_staging_facts(staging, expected).await?;
    let temp_path = copy_to_unique_temp(staging, &target).await?;
    promote_staged_add_only_from_temp(staging_facts, &target, temp_path, expected, failpoint).await
}

pub async fn promote_staged_add_only_with_temp(
    staging: impl AsRef<Path>,
    target: impl AsRef<Path>,
    temp_path: impl AsRef<Path>,
    expected: &ArtifactFileFacts,
) -> Result<PromotionReport, VoomError> {
    let staging = staging.as_ref();
    let target = canonical_new_leaf_no_symlink(target).await?;
    let temp_path = canonical_new_leaf_no_symlink(temp_path).await?;
    if temp_path.parent() != target.parent() {
        return Err(VoomError::CommitFailure(format!(
            "temporary artifact path {} must be beside target {}",
            temp_path.display(),
            target.display()
        )));
    }
    let staging_facts = require_expected_staging_facts(staging, expected).await?;
    copy_regular_file_checked(staging, &temp_path).await?;
    promote_staged_add_only_from_temp(
        staging_facts,
        &target,
        temp_path,
        expected,
        &NoPromotionFailpoint,
    )
    .await
}

pub(crate) async fn require_expected_staging_facts(
    staging: &Path,
    expected: &ArtifactFileFacts,
) -> Result<ArtifactFileFacts, VoomError> {
    let staging_facts = observe_regular_file(staging).await?;
    if !same_file_facts(&staging_facts, expected) {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "staged artifact facts do not match expected facts: {}",
            staging_facts.path.display()
        )));
    }
    Ok(staging_facts)
}

async fn promote_staged_add_only_from_temp(
    staging_facts: ArtifactFileFacts,
    target: &Path,
    temp_path: PathBuf,
    expected: &ArtifactFileFacts,
    failpoint: &dyn PromotionFailpoint,
) -> Result<PromotionReport, VoomError> {
    let temp_facts = match observe_regular_file(&temp_path).await {
        Ok(facts) => facts,
        Err(err) => return Err(remove_new_file_after_error(&temp_path, err).await),
    };
    if !same_file_facts(&temp_facts, expected) {
        return Err(remove_new_file_after_error(
            &temp_path,
            VoomError::ArtifactChecksumMismatch(format!(
                "temporary artifact facts do not match expected facts: {}",
                temp_path.display()
            )),
        )
        .await);
    }

    if let Err(err) = failpoint.before_install(PromotionFailpointContext {
        temp_path: &temp_path,
        target_path: target,
    }) {
        return Err(remove_new_file_after_error(&temp_path, err).await);
    }

    if let Err(err) = install_temp_no_replace(&temp_path, target).await {
        return Err(remove_new_file_after_error(&temp_path, err).await);
    }

    let target_facts = match observe_regular_file(&target).await {
        Ok(facts) => facts,
        Err(err) => return Err(remove_new_file_after_error(target, err).await),
    };
    if !same_file_facts(&target_facts, expected) {
        return Err(remove_new_file_after_error(
            target,
            VoomError::ArtifactChecksumMismatch(format!(
                "installed artifact facts do not match expected facts: {}",
                target.display()
            )),
        )
        .await);
    }

    Ok(PromotionReport {
        staging: staging_facts,
        target: target_facts,
        temp_path,
    })
}

async fn copy_to_unique_temp(source: &Path, target: &Path) -> Result<PathBuf, VoomError> {
    for _ in 0..16 {
        let temp_path = unique_temp_sibling_path(target)?;
        match fs::symlink_metadata(&temp_path).await {
            Ok(_) => continue,
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(VoomError::CommitFailure(format!(
                    "cannot inspect temporary artifact path {}: {err}",
                    temp_path.display()
                )));
            }
        }
        copy_regular_file_checked(source, &temp_path).await?;
        return Ok(temp_path);
    }

    Err(VoomError::CommitFailure(format!(
        "could not allocate unique temporary artifact path beside {}",
        target.display()
    )))
}

#[derive(Debug)]
enum CopyFileError {
    NotCreated(VoomError),
    Created(VoomError),
}

async fn copy_regular_file_contents(
    source: &Path,
    destination: &Path,
) -> Result<(), CopyFileError> {
    let mut source_file = open_regular_file_no_follow(source)
        .await
        .map_err(CopyFileError::NotCreated)?;
    let mut destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await
        .map_err(|err| match err.kind() {
            ErrorKind::AlreadyExists => {
                CopyFileError::NotCreated(VoomError::CommitFailure(format!(
                    "artifact destination already exists: {}",
                    destination.display()
                )))
            }
            _ => CopyFileError::NotCreated(VoomError::CommitFailure(format!(
                "cannot create artifact destination {}: {err}",
                destination.display()
            ))),
        })?;

    tokio::io::copy(&mut source_file, &mut destination_file)
        .await
        .map_err(|err| {
            CopyFileError::Created(VoomError::ArtifactUnavailable(format!(
                "cannot copy artifact {} to {}: {err}",
                source.display(),
                destination.display()
            )))
        })?;
    destination_file.flush().await.map_err(|err| {
        CopyFileError::Created(VoomError::CommitFailure(format!(
            "cannot flush artifact destination {}: {err}",
            destination.display()
        )))
    })?;
    drop(destination_file);
    Ok(())
}

async fn install_temp_no_replace(temp_path: &Path, target: &Path) -> Result<(), VoomError> {
    fs::hard_link(temp_path, target)
        .await
        .map_err(|err| match err.kind() {
            ErrorKind::AlreadyExists => VoomError::CommitFailure(format!(
                "artifact target already exists: {}",
                target.display()
            )),
            _ => VoomError::CommitFailure(format!(
                "cannot install artifact {} to {} without replacement: {err}",
                temp_path.display(),
                target.display()
            )),
        })?;

    if let Err(err) = fsync_parent_dir(target).await {
        let _ = remove_file_if_exists(target).await;
        return Err(err);
    }

    if let Err(err) = fs::remove_file(temp_path).await {
        let _ = remove_file_if_exists(target).await;
        return Err(VoomError::CommitFailure(format!(
            "cannot remove temporary artifact path {} after install: {err}",
            temp_path.display()
        )));
    }
    if let Err(err) = fsync_parent_dir(target).await {
        let _ = remove_file_if_exists(target).await;
        return Err(err);
    }
    Ok(())
}

#[cfg(unix)]
async fn open_regular_file_no_follow(path: &Path) -> Result<fs::File, VoomError> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await
        .map_err(|err| {
            VoomError::ArtifactUnavailable(format!(
                "cannot open artifact path without following symlinks {}: {err}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn open_regular_file_no_follow(path: &Path) -> Result<fs::File, VoomError> {
    fs::File::open(path).await.map_err(|err| {
        VoomError::ArtifactUnavailable(format!(
            "cannot open artifact path {}: {err}",
            path.display()
        ))
    })
}

async fn fsync_file(path: &Path) -> Result<(), VoomError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .read(true)
            .open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|err| {
                VoomError::CommitFailure(format!(
                    "cannot fsync artifact file {}: {err}",
                    path.display()
                ))
            })
    })
    .await
    .map_err(|err| VoomError::Internal(format!("artifact fsync task failed: {err}")))?
}

#[cfg(unix)]
async fn fsync_parent_dir(path: &Path) -> Result<(), VoomError> {
    let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|err| {
                VoomError::CommitFailure(format!(
                    "cannot fsync artifact parent directory {}: {err}",
                    parent.display()
                ))
            })
    })
    .await
    .map_err(|err| VoomError::Internal(format!("artifact directory fsync task failed: {err}")))?
}

#[cfg(not(unix))]
async fn fsync_parent_dir(_path: &Path) -> Result<(), VoomError> {
    Ok(())
}

async fn remove_new_file_after_error(path: &Path, err: VoomError) -> VoomError {
    match remove_file_if_exists(path).await {
        Ok(()) => err,
        Err(cleanup_err) => {
            VoomError::CommitFailure(format!("{err}; cleanup failed: {cleanup_err}"))
        }
    }
}

async fn remove_file_if_exists(path: &Path) -> Result<(), VoomError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VoomError::CommitFailure(format!(
            "cannot remove artifact path {}: {err}",
            path.display()
        ))),
    }
}

fn same_file_facts(left: &ArtifactFileFacts, right: &ArtifactFileFacts) -> bool {
    left.size_bytes == right.size_bytes && left.content_hash == right.content_hash
}

fn metadata_changed(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() != after.len() || before.modified().ok() != after.modified().ok()
}

#[cfg(unix)]
fn local_file_key(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;

    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

async fn reject_symlink_components(path: &Path) -> Result<(), VoomError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(config(format!(
                    "artifact path must not traverse a symlink: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => break,
            Err(err) => {
                return Err(config(format!(
                    "cannot inspect artifact path component {}: {err}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

fn config(message: String) -> VoomError {
    VoomError::Config(message)
}

#[cfg(test)]
#[path = "fs_test.rs"]
mod tests;
