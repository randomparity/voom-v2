use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use serde_json::json;
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FileLocationId, FileVersionId, VoomError,
};
use voom_events::payload::ArtifactStagedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{NewArtifactHandle, NewArtifactLocation};
use voom_store::repo::identity::{FileLocation, FileLocationKind, FileVersion, IdentityRepo};

use crate::ControlPlane;
use crate::artifact::fs::{
    ArtifactFileFacts, PromotionFailpoint, PromotionFailpointContext,
    canonical_existing_file_no_symlink, canonical_new_leaf_no_symlink, observe_regular_file,
    promote_staged_add_only,
};
use crate::cases::{append_event, begin_tx, commit_tx};

#[derive(Debug)]
pub struct StageCopyInput {
    pub file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub staging_path: PathBuf,
}

#[derive(Debug)]
pub struct StageCopyReport {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: FileLocationId,
    pub source_path: PathBuf,
    pub staging_path: PathBuf,
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug)]
pub struct StageCopyCommandError {
    code: ErrorCode,
    message: String,
    data: Option<serde_json::Value>,
}

impl StageCopyCommandError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub const fn data(&self) -> Option<&serde_json::Value> {
        self.data.as_ref()
    }

    fn with_data(err: &VoomError, data: serde_json::Value) -> Self {
        Self {
            code: err.error_code(),
            message: err.to_string(),
            data: Some(data),
        }
    }
}

impl From<VoomError> for StageCopyCommandError {
    fn from(value: VoomError) -> Self {
        Self {
            code: value.error_code(),
            message: value.to_string(),
            data: None,
        }
    }
}

impl Display for StageCopyCommandError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for StageCopyCommandError {}

impl ControlPlane {
    /// Copy a live source `local_path` into a new staging path and durably
    /// record the staged artifact handle, location, and audit event.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing source version, `Config` for invalid
    /// source/staging selection, artifact errors for unreadable/changing
    /// bytes, and database errors for durable recording failures.
    pub async fn stage_copy(
        &self,
        input: StageCopyInput,
    ) -> Result<StageCopyReport, StageCopyCommandError> {
        stage_copy_with_hooks(self, input, &NoStageCopyHooks).await
    }
}

#[derive(Debug, Clone, Copy)]
#[expect(
    dead_code,
    reason = "test-only stage-copy hooks inspect whichever context fields their failure mode needs"
)]
pub(crate) struct StageCopyInstallContext<'a> {
    pub temp_path: &'a Path,
    pub staging_path: &'a Path,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StageCopyDatabaseContext<'a> {
    pub staging_path: &'a Path,
}

pub(crate) trait StageCopyHooks: Send + Sync {
    fn before_install(&self, _context: StageCopyInstallContext<'_>) -> Result<(), VoomError> {
        Ok(())
    }

    fn before_database_transaction(
        &self,
        context: StageCopyDatabaseContext<'_>,
    ) -> Result<(), VoomError> {
        let _ = context.staging_path;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct NoStageCopyHooks;

impl StageCopyHooks for NoStageCopyHooks {}

pub(crate) async fn stage_copy_with_hooks(
    cp: &ControlPlane,
    input: StageCopyInput,
    hooks: &dyn StageCopyHooks,
) -> Result<StageCopyReport, StageCopyCommandError> {
    let source_version = require_source_version(cp, input.file_version_id).await?;
    let source_location =
        select_source_location(cp, input.file_version_id, input.source_location_id).await?;
    let source_path = canonical_existing_file_no_symlink(&source_location.value).await?;
    let staging_path = canonical_new_leaf_no_symlink(&input.staging_path).await?;
    let source_facts = observe_regular_file(&source_path).await?;
    require_matching_version_facts(&source_version, &source_facts)?;

    let promotion = promote_staged_add_only(
        &source_path,
        &staging_path,
        &source_facts,
        &StageCopyPromotionFailpoint { hooks },
    )
    .await
    .map_err(map_staging_install_error)?;
    let staged_facts = promotion.target;

    let record_result = record_staged_artifact(
        cp,
        &source_version,
        &source_location,
        &source_path,
        &staging_path,
        &staged_facts,
        hooks,
    )
    .await;

    match record_result {
        Ok(report) => Ok(report),
        Err(err) => {
            Err(cleanup_staging_after_database_error(&staging_path, &staged_facts, err).await)
        }
    }
}

async fn require_source_version(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
) -> Result<FileVersion, VoomError> {
    let version = cp
        .identity
        .get_file_version(file_version_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("file_versions {file_version_id} missing")))?;
    if version.retired_at.is_some() {
        return Err(VoomError::NotFound(format!(
            "file_versions {file_version_id} is retired"
        )));
    }
    Ok(version)
}

async fn select_source_location(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    source_location_id: Option<FileLocationId>,
) -> Result<FileLocation, VoomError> {
    if let Some(id) = source_location_id {
        let location = cp
            .identity
            .get_file_location(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("file_locations {id} missing")))?;
        require_live_local_source_location(&location, file_version_id)?;
        return Ok(location);
    }

    let local_locations = cp
        .identity
        .list_live_file_locations_by_version(file_version_id)
        .await?
        .into_iter()
        .filter(|location| location.kind == FileLocationKind::LocalPath)
        .collect::<Vec<_>>();
    let [location] = local_locations.as_slice() else {
        return Err(VoomError::Config(format!(
            "file_version {file_version_id} must have exactly one live local_path source \
             location; found {}",
            local_locations.len()
        )));
    };
    Ok(location.clone())
}

fn require_live_local_source_location(
    location: &FileLocation,
    file_version_id: FileVersionId,
) -> Result<(), VoomError> {
    if location.file_version_id != file_version_id {
        return Err(VoomError::Config(format!(
            "file_location {} belongs to file_version {}, not {}",
            location.id, location.file_version_id, file_version_id
        )));
    }
    if location.retired_at.is_some() {
        return Err(VoomError::Config(format!(
            "file_location {} is retired",
            location.id
        )));
    }
    if location.kind != FileLocationKind::LocalPath {
        return Err(VoomError::Config(format!(
            "file_location {} must be kind local_path",
            location.id
        )));
    }
    Ok(())
}

fn require_matching_version_facts(
    version: &FileVersion,
    facts: &ArtifactFileFacts,
) -> Result<(), VoomError> {
    if version.size_bytes != facts.size_bytes || version.content_hash != facts.content_hash {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "source file facts do not match file_version {}",
            version.id
        )));
    }
    Ok(())
}

async fn record_staged_artifact(
    cp: &ControlPlane,
    source_version: &FileVersion,
    source_location: &FileLocation,
    source_path: &Path,
    staging_path: &Path,
    staged_facts: &ArtifactFileFacts,
    hooks: &dyn StageCopyHooks,
) -> Result<StageCopyReport, VoomError> {
    hooks.before_database_transaction(StageCopyDatabaseContext { staging_path })?;

    let mut tx = begin_tx(&cp.pool).await?;
    let source_version = cp
        .identity
        .get_file_version_in_tx(&mut tx, source_version.id)
        .await?
        .ok_or_else(|| {
            VoomError::NotFound(format!("file_versions {} missing", source_version.id))
        })?;
    if source_version.retired_at.is_some() {
        return Err(VoomError::NotFound(format!(
            "file_versions {} is retired",
            source_version.id
        )));
    }
    require_matching_version_facts(&source_version, staged_facts)?;

    let source_location = cp
        .identity
        .get_file_location_in_tx(&mut tx, source_location.id)
        .await?
        .ok_or_else(|| {
            VoomError::NotFound(format!("file_locations {} missing", source_location.id))
        })?;
    require_live_local_source_location(&source_location, source_version.id)?;

    let now = cp.clock().now();
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(staged_facts.size_bytes).map_err(|err| {
                    VoomError::Internal(format!("artifact size exceeds SQLite integer: {err}"))
                })?),
                checksum: Some(staged_facts.content_hash.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec!["local_path".to_owned()],
                mutability: "immutable".to_owned(),
                source_lineage: Some(json!({
                    "source_file_version_id": source_version.id.0,
                    "source_location_id": source_location.id.0,
                    "source_path": source_path.display().to_string(),
                })),
                file_version_id: Some(source_version.id),
                created_at: now,
            },
        )
        .await?;
    let location = cp
        .artifacts
        .record_location_in_tx(
            &mut tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: "staging".to_owned(),
                value: staging_path.display().to_string(),
                observed_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        &mut tx,
        SubjectType::ArtifactHandle,
        Some(handle.id.0),
        now,
        Event::ArtifactStaged(ArtifactStagedPayload {
            artifact_handle_id: handle.id.0,
            artifact_location_id: location.id.0,
            source_file_version_id: source_version.id.0,
            source_file_location_id: Some(source_location.id.0),
            staging_path: staging_path.display().to_string(),
            size_bytes: staged_facts.size_bytes,
            checksum: staged_facts.content_hash.clone(),
        }),
    )
    .await?;
    commit_tx(tx).await?;

    Ok(StageCopyReport {
        artifact_handle_id: handle.id,
        artifact_location_id: location.id,
        source_file_version_id: source_version.id,
        source_location_id: source_location.id,
        source_path: source_path.to_path_buf(),
        staging_path: staging_path.to_path_buf(),
        size_bytes: staged_facts.size_bytes,
        checksum: staged_facts.content_hash.clone(),
    })
}

struct StageCopyPromotionFailpoint<'a> {
    hooks: &'a dyn StageCopyHooks,
}

impl PromotionFailpoint for StageCopyPromotionFailpoint<'_> {
    fn before_install(&self, context: PromotionFailpointContext<'_>) -> Result<(), VoomError> {
        self.hooks.before_install(StageCopyInstallContext {
            temp_path: context.temp_path,
            staging_path: context.target_path,
        })
    }
}

fn map_staging_install_error(err: VoomError) -> VoomError {
    if matches!(&err, VoomError::CommitFailure(message) if message.contains("artifact target already exists"))
    {
        return VoomError::Config(err.to_string());
    }
    err
}

async fn cleanup_staging_after_database_error(
    staging_path: &Path,
    expected: &ArtifactFileFacts,
    err: VoomError,
) -> StageCopyCommandError {
    let cleanup_result = cleanup_staging_file_if_unchanged(staging_path, expected).await;
    let cleanup_succeeded = cleanup_result.is_ok();
    let cleanup_error = cleanup_result.err();
    let report = json!({
        "staging_path": staging_path.display().to_string(),
        "cleanup_attempted": true,
        "cleanup_succeeded": cleanup_succeeded,
        "cleanup_error": cleanup_error,
        "error_code": err.code(),
        "message": err.to_string(),
    });
    StageCopyCommandError::with_data(&err, report)
}

async fn cleanup_staging_file_if_unchanged(
    staging_path: &Path,
    expected: &ArtifactFileFacts,
) -> Result<(), String> {
    match tokio::fs::symlink_metadata(staging_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.to_string()),
    }
    let current = match observe_regular_file(staging_path).await {
        Ok(facts) => facts,
        Err(err) => return Err(err.to_string()),
    };
    if current.local_file_key.is_none() || current.local_file_key != expected.local_file_key {
        return Err("staging path changed before cleanup".to_owned());
    }
    if current.size_bytes != expected.size_bytes || current.content_hash != expected.content_hash {
        return Err("staging path changed before cleanup".to_owned());
    }
    if current.modified_at != expected.modified_at {
        return Err("staging path changed before cleanup".to_owned());
    }
    tokio::fs::remove_file(staging_path)
        .await
        .map_err(|err| err.to_string())
}

#[cfg(test)]
#[path = "stage_test.rs"]
mod tests;
