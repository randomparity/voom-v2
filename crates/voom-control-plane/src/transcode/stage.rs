use std::path::{Path, PathBuf};

use voom_core::{LeaseId, TicketId, VoomError};

pub async fn staging_path(
    staging_root: &Path,
    ticket_id: TicketId,
    lease_id: LeaseId,
    source_path: &str,
) -> Result<PathBuf, VoomError> {
    reject_existing_symlink_components(staging_root).await?;
    tokio::fs::create_dir_all(staging_root)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create transcode staging root {}: {err}",
                staging_root.display()
            ))
        })?;
    reject_symlink_dir(staging_root, "transcode staging root").await?;
    let canonical_root = tokio::fs::canonicalize(staging_root).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize transcode staging root {}: {err}",
            staging_root.display()
        ))
    })?;
    let ticket_parent = staging_root.join(format!("ticket-{}", ticket_id.0));
    reject_existing_symlink_components(&ticket_parent).await?;
    tokio::fs::create_dir_all(&ticket_parent)
        .await
        .map_err(|err| {
            VoomError::Config(format!(
                "create transcode staging ticket parent {}: {err}",
                ticket_parent.display()
            ))
        })?;
    reject_symlink_dir(&ticket_parent, "transcode staging ticket parent").await?;
    let parent = ticket_parent.join(format!("lease-{}", lease_id.0));
    reject_existing_symlink_components(&parent).await?;
    tokio::fs::create_dir(&parent).await.map_err(|err| {
        VoomError::Config(format!(
            "create transcode staging parent {}: {err}",
            parent.display()
        ))
    })?;
    reject_symlink_dir(&parent, "transcode staging parent").await?;
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
    Ok(canonical_parent.join(output_file_name(source_path)))
}

pub async fn target_path(target_dir: &Path, source_path: &str) -> Result<PathBuf, VoomError> {
    reject_existing_symlink_components(target_dir).await?;
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
    Ok(canonical_dir.join(output_file_name(source_path)))
}

fn output_file_name(source: &str) -> String {
    let stem = Path::new(source)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    format!("{stem}.hevc.mkv")
}

async fn reject_existing_symlink_components(path: &Path) -> Result<(), VoomError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VoomError::Config(format!(
                    "transcode path component must not be a symlink: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(VoomError::Config(format!(
                    "inspect transcode path component {}: {err}",
                    current.display()
                )));
            }
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

#[cfg(test)]
#[path = "stage_test.rs"]
mod tests;
