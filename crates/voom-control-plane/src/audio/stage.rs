use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use voom_core::{LeaseId, TicketId, VoomError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStagingPath {
    pub canonical_root: PathBuf,
    pub path: PathBuf,
}

pub async fn prepare_transcode_staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    source_path: &Path,
    codec: &str,
) -> Result<PreparedStagingPath, VoomError> {
    prepare_staging_path(
        staging_root,
        ticket_id,
        lease_id,
        &transcode_file_name(source_path, codec),
    )
    .await
}

pub async fn prepare_extract_staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    source_path: &Path,
    snapshot_stream_id: &str,
    codec: &str,
) -> Result<PreparedStagingPath, VoomError> {
    prepare_staging_path(
        staging_root,
        ticket_id,
        lease_id,
        &extract_file_name(source_path, snapshot_stream_id, codec),
    )
    .await
}

pub async fn transcode_target_path(
    target_dir: &Path,
    source_path: &Path,
    codec: &str,
) -> Result<PathBuf, VoomError> {
    target_path(target_dir, &transcode_file_name(source_path, codec)).await
}

pub async fn extract_target_path(
    target_dir: &Path,
    source_path: &Path,
    snapshot_stream_id: &str,
    codec: &str,
) -> Result<PathBuf, VoomError> {
    target_path(
        target_dir,
        &extract_file_name(source_path, snapshot_stream_id, codec),
    )
    .await
}

async fn prepare_staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    file_name: &str,
) -> Result<PreparedStagingPath, VoomError> {
    reject_symlink_components(staging_root, "audio staging root").await?;
    tokio::fs::create_dir_all(staging_root)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create audio staging root {}: {err}",
                staging_root.display()
            ))
        })?;
    reject_symlink_dir(staging_root, "audio staging root").await?;
    secure_private_dir(staging_root, "audio staging root").await?;
    let canonical_root = tokio::fs::canonicalize(staging_root).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize audio staging root {}: {err}",
            staging_root.display()
        ))
    })?;
    let ticket_parent = canonical_root.join(format!("ticket-{}", ticket_id.0));
    tokio::fs::create_dir_all(&ticket_parent)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create audio staging ticket parent {}: {err}",
                ticket_parent.display()
            ))
        })?;
    reject_symlink_dir(&ticket_parent, "audio staging ticket parent").await?;
    secure_private_dir(&ticket_parent, "audio staging ticket parent").await?;
    let parent = ticket_parent.join(format!("lease-{}", lease_id.0));
    tokio::fs::create_dir_all(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "create audio staging parent {}: {err}",
            parent.display()
        ))
    })?;
    reject_symlink_dir(&parent, "audio staging parent").await?;
    secure_private_dir(&parent, "audio staging parent").await?;
    let canonical_parent = tokio::fs::canonicalize(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize audio staging parent {}: {err}",
            parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(VoomError::Config(format!(
            "audio staging parent {} escapes root {}",
            canonical_parent.display(),
            canonical_root.display()
        )));
    }
    let path = canonical_parent.join(file_name);
    reject_existing_file(&path, "staging path").await?;
    Ok(PreparedStagingPath {
        canonical_root,
        path,
    })
}

async fn target_path(target_dir: &Path, file_name: &str) -> Result<PathBuf, VoomError> {
    reject_symlink_components(target_dir, "audio target dir").await?;
    tokio::fs::create_dir_all(target_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "create audio target dir {}: {err}",
            target_dir.display()
        ))
    })?;
    reject_symlink_dir(target_dir, "audio target dir").await?;
    let canonical_dir = tokio::fs::canonicalize(target_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize audio target dir {}: {err}",
            target_dir.display()
        ))
    })?;
    let path = canonical_dir.join(file_name);
    reject_existing_file(&path, "target path").await?;
    Ok(path)
}

fn transcode_file_name(source: &Path, codec: &str) -> String {
    format!(
        "{}.audio-{}.mkv",
        source_stem(source),
        sanitize_component(codec)
    )
}

fn extract_file_name(source: &Path, snapshot_stream_id: &str, codec: &str) -> String {
    format!(
        "{}.{}.{}.ogg",
        source_stem(source),
        sanitize_component(snapshot_stream_id),
        sanitize_component(codec)
    )
}

fn source_stem(source: &Path) -> String {
    source
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("output")
        .to_owned()
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "stream".to_owned()
    } else {
        sanitized
    }
}

async fn reject_existing_file(path: &Path, label: &str) -> Result<(), VoomError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => Err(VoomError::Config(format!(
            "audio {label} already exists: {}",
            path.display()
        ))),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VoomError::Config(format!(
            "stat audio {label} {}: {err}",
            path.display()
        ))),
    }
}

async fn reject_symlink_dir(path: &Path, label: &str) -> Result<(), VoomError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|err| VoomError::Config(format!("{label} {}: {err}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(VoomError::Config(format!(
            "{label} must be a non-symlink directory: {}",
            path.display()
        )));
    }
    Ok(())
}

async fn reject_symlink_components(path: &Path, label: &str) -> Result<(), VoomError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() && !current.has_root() {
                    current.push(component.as_os_str());
                }
            }
        }
        if current.as_os_str().is_empty() {
            continue;
        }
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VoomError::Config(format!(
                    "{label} must not traverse a symlink: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(VoomError::Config(format!(
                    "inspect {label} component {}: {err}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn secure_private_dir(path: &Path, label: &str) -> Result<(), VoomError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = tokio::fs::metadata(path)
        .await
        .map_err(|err| VoomError::Config(format!("inspect {label} {}: {err}", path.display())))?
        .permissions();
    permissions.set_mode(0o700);
    tokio::fs::set_permissions(path, permissions)
        .await
        .map_err(|err| VoomError::Config(format!("secure {label} {}: {err}", path.display())))?;
    Ok(())
}

#[cfg(not(unix))]
async fn secure_private_dir(_path: &Path, _label: &str) -> Result<(), VoomError> {
    Ok(())
}

#[cfg(test)]
#[path = "stage_test.rs"]
mod tests;
