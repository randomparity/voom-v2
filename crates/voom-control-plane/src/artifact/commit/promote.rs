use std::io::ErrorKind;
use std::path::Path;

use tokio::fs;
use voom_core::VoomError;

use crate::ControlPlane;
use crate::artifact::commit::{
    CommitArtifactHooks, CommitArtifactInstallContext, CommitArtifactPreparedContext,
    PreparedCommit, PromotionOutcome, same_file_facts,
};
use crate::artifact::fs::{copy_regular_file_checked, observe_regular_file};

pub(super) async fn promote_prepared(
    _cp: &ControlPlane,
    prepared: &PreparedCommit,
    hooks: &dyn CommitArtifactHooks,
) -> Result<PromotionOutcome, VoomError> {
    let staging_facts = observe_regular_file(&prepared.staging_path).await?;
    if !same_file_facts(&staging_facts, &prepared.expected_facts) {
        return Err(VoomError::ArtifactChecksumMismatch(
            "staged artifact facts drifted after durable prepare".to_owned(),
        ));
    }
    hooks.before_temp_copy(CommitArtifactPreparedContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
        staging_path: &prepared.staging_path,
    })?;
    let staging_facts = observe_regular_file(&prepared.staging_path).await?;
    if !same_file_facts(&staging_facts, &prepared.expected_facts) {
        return Err(VoomError::ArtifactChecksumMismatch(
            "staged artifact facts drifted after durable prepare".to_owned(),
        ));
    }
    let temp_facts = copy_regular_file_checked(&prepared.staging_path, &prepared.temp_path).await?;
    if !same_file_facts(&temp_facts, &prepared.expected_facts) {
        let _cleanup = remove_file_if_exists(&prepared.temp_path).await;
        return Err(VoomError::ArtifactChecksumMismatch(
            "temporary artifact facts do not match verified staged artifact".to_owned(),
        ));
    }
    hooks.before_install(CommitArtifactInstallContext {
        commit_record_id: prepared.record.id,
        target_path: &prepared.target_path,
        temp_path: &prepared.temp_path,
    })?;
    install_temp_no_replace(&prepared.temp_path, &prepared.target_path).await?;
    let target_facts = observe_regular_file(&prepared.target_path).await?;
    if !same_file_facts(&target_facts, &prepared.expected_facts) {
        return Err(VoomError::VerificationFailure(
            "committed target facts do not match verified staged artifact".to_owned(),
        ));
    }
    Ok(PromotionOutcome { target_facts })
}

async fn install_temp_no_replace(temp_path: &Path, target_path: &Path) -> Result<(), VoomError> {
    fs::hard_link(temp_path, target_path)
        .await
        .map_err(|err| match err.kind() {
            ErrorKind::AlreadyExists => VoomError::CommitFailure(format!(
                "artifact target already exists: {}",
                target_path.display()
            )),
            _ => VoomError::CommitFailure(format!(
                "cannot install artifact {} to {} without replacement: {err}",
                temp_path.display(),
                target_path.display()
            )),
        })?;
    if let Err(err) = fsync_parent_dir(target_path).await {
        let _ = remove_file_if_exists(target_path).await;
        return Err(err);
    }
    if let Err(err) = fs::remove_file(temp_path).await {
        return Err(VoomError::CommitFailure(format!(
            "cannot remove temporary artifact path {} after install: {err}",
            temp_path.display()
        )));
    }
    fsync_parent_dir(target_path).await
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

pub(super) async fn remove_file_if_exists(path: &Path) -> Result<(), VoomError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VoomError::CommitFailure(format!(
            "cannot remove artifact path {}: {err}",
            path.display()
        ))),
    }
}
