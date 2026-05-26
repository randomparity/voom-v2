use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use voom_core::{LeaseId, TicketId, VoomError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStagingPath {
    pub canonical_root: PathBuf,
    pub path: PathBuf,
}

pub async fn staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    source_path: &Path,
) -> Result<PathBuf, VoomError> {
    Ok(
        prepare_staging_path(staging_root, ticket_id, lease_id, source_path)
            .await?
            .path,
    )
}

pub async fn prepare_staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    source_path: &Path,
) -> Result<PreparedStagingPath, VoomError> {
    reject_symlink_components(staging_root, "remux staging root").await?;
    tokio::fs::create_dir_all(staging_root)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create remux staging root {}: {err}",
                staging_root.display()
            ))
        })?;
    reject_symlink_dir(staging_root, "remux staging root").await?;
    secure_private_dir(staging_root, "remux staging root").await?;
    let canonical_root = tokio::fs::canonicalize(staging_root).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize remux staging root {}: {err}",
            staging_root.display()
        ))
    })?;
    let ticket_parent = canonical_root.join(format!("ticket-{}", ticket_id.0));
    reject_symlink_components(&ticket_parent, "remux staging ticket parent").await?;
    tokio::fs::create_dir_all(&ticket_parent)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create remux staging ticket parent {}: {err}",
                ticket_parent.display()
            ))
        })?;
    reject_symlink_dir(&ticket_parent, "remux staging ticket parent").await?;
    secure_private_dir(&ticket_parent, "remux staging ticket parent").await?;
    let parent = ticket_parent.join(format!("lease-{}", lease_id.0));
    reject_symlink_components(&parent, "remux staging parent").await?;
    tokio::fs::create_dir_all(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "create remux staging parent {}: {err}",
            parent.display()
        ))
    })?;
    reject_symlink_dir(&parent, "remux staging parent").await?;
    secure_private_dir(&parent, "remux staging parent").await?;
    let canonical_parent = tokio::fs::canonicalize(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize remux staging parent {}: {err}",
            parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(VoomError::Config(format!(
            "remux staging parent {} escapes root {}",
            canonical_parent.display(),
            canonical_root.display()
        )));
    }
    let path = canonical_parent.join(output_file_name(source_path));
    reject_existing_file(&path, "staging path").await?;
    Ok(PreparedStagingPath {
        canonical_root,
        path,
    })
}

pub async fn target_path(target_dir: &Path, source_path: &Path) -> Result<PathBuf, VoomError> {
    reject_symlink_components(target_dir, "remux target dir").await?;
    tokio::fs::create_dir_all(target_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "create remux target dir {}: {err}",
            target_dir.display()
        ))
    })?;
    reject_symlink_dir(target_dir, "remux target dir").await?;
    let canonical_dir = tokio::fs::canonicalize(target_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize remux target dir {}: {err}",
            target_dir.display()
        ))
    })?;
    let path = canonical_dir.join(output_file_name(source_path));
    reject_existing_file(&path, "target path").await?;
    Ok(path)
}

fn output_file_name(source: &Path) -> String {
    let stem = source
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    format!("{stem}.remux.mkv")
}

async fn reject_existing_file(path: &Path, label: &str) -> Result<(), VoomError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => {
            return Err(VoomError::Config(format!(
                "remux {label} already exists: {}",
                path.display()
            )));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VoomError::Config(format!(
                "stat remux {label} {}: {err}",
                path.display()
            )));
        }
    }
    Ok(())
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
