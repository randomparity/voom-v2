use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use voom_core::{LeaseId, TicketId, VoomError};

/// Borrowed inputs that determine the output file name for a transcode.
#[derive(Debug)]
pub struct OutputName<'a> {
    pub source_path: &'a str,
    pub profile_id: &'a str,
    pub codec: &'a str,
    pub container: &'a str,
}

pub async fn staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    output: &OutputName<'_>,
) -> Result<PathBuf, VoomError> {
    reject_symlink_components(staging_root, "transcode staging root").await?;
    tokio::fs::create_dir_all(staging_root)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create transcode staging root {}: {err}",
                staging_root.display()
            ))
        })?;
    reject_symlink_dir(staging_root, "transcode staging root").await?;
    secure_private_dir(staging_root, "transcode staging root").await?;
    let canonical_root = tokio::fs::canonicalize(staging_root).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize transcode staging root {}: {err}",
            staging_root.display()
        ))
    })?;
    let ticket_parent = canonical_root.join(format!("ticket-{}", ticket_id.0));
    reject_symlink_components(&ticket_parent, "transcode staging ticket parent").await?;
    tokio::fs::create_dir_all(&ticket_parent)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create transcode staging ticket parent {}: {err}",
                ticket_parent.display()
            ))
        })?;
    reject_symlink_dir(&ticket_parent, "transcode staging ticket parent").await?;
    secure_private_dir(&ticket_parent, "transcode staging ticket parent").await?;
    let parent = ticket_parent.join(format!("lease-{}", lease_id.0));
    reject_symlink_components(&parent, "transcode staging parent").await?;
    tokio::fs::create_dir_all(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "create transcode staging parent {}: {err}",
            parent.display()
        ))
    })?;
    reject_symlink_dir(&parent, "transcode staging parent").await?;
    secure_private_dir(&parent, "transcode staging parent").await?;
    let canonical_parent = tokio::fs::canonicalize(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize transcode staging parent {}: {err}",
            parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(VoomError::Config(format!(
            "transcode staging parent {} escapes root {}",
            canonical_parent.display(),
            canonical_root.display()
        )));
    }
    let path = canonical_parent.join(output_file_name(output));
    reject_existing_file(&path, "staging path").await?;
    Ok(path)
}

pub async fn target_path(target_dir: &Path, output: &OutputName<'_>) -> Result<PathBuf, VoomError> {
    reject_symlink_components(target_dir, "transcode target dir").await?;
    tokio::fs::create_dir_all(target_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "create transcode target dir {}: {err}",
            target_dir.display()
        ))
    })?;
    reject_symlink_dir(target_dir, "transcode target dir").await?;
    let canonical_dir = tokio::fs::canonicalize(target_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize transcode target dir {}: {err}",
            target_dir.display()
        ))
    })?;
    let file_name = output_file_name(output);
    let target = canonical_dir.join(&file_name);
    reject_existing_file(&target, "target path").await?;
    Ok(target)
}

/// Builds the output file name from the source stem, profile identity,
/// target codec, and container extension. The `profile_id` is sanitized
/// so any character outside `[A-Za-z0-9._-]` is replaced with `-`, keeping
/// file names safe across all filesystems.
///
/// Format: `<stem>.<profile_id>.<codec>.<container>`
pub fn output_file_name(output: &OutputName<'_>) -> String {
    let stem = Path::new(output.source_path)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    let sanitized_id: String = output
        .profile_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!(
        "{stem}.{sanitized_id}.{}.{}",
        output.codec, output.container
    )
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

async fn reject_existing_file(path: &Path, label: &str) -> Result<(), VoomError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => Err(VoomError::Config(format!(
            "transcode {label} already exists: {}",
            path.display()
        ))),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VoomError::Config(format!(
            "stat transcode {label} {}: {err}",
            path.display()
        ))),
    }
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
    let mode = tokio::fs::metadata(path)
        .await
        .map_err(|err| VoomError::Config(format!("inspect {label} {}: {err}", path.display())))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o700 {
        return Err(VoomError::Config(format!(
            "{label} must be private: {} has mode {mode:o}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
async fn secure_private_dir(_path: &Path, _label: &str) -> Result<(), VoomError> {
    Ok(())
}

#[cfg(test)]
#[path = "stage_test.rs"]
mod tests;
