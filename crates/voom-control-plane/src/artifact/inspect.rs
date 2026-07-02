use std::path::{Path, PathBuf};

use sqlx::Row;
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, VoomError, WorkerId,
};
use voom_store::repo::artifacts::{
    ArtifactCommitRecord, ArtifactCommitState, ArtifactLocation, ArtifactVerification,
    ArtifactVerificationStatus,
};

use crate::ControlPlane;
use crate::artifact::fs::observe_regular_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactInspectionState {
    Staged,
    Verified,
    Committed,
    Failed,
    RecoveryRequired,
}

impl ArtifactInspectionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Verified => "verified",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::RecoveryRequired => "recovery_required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactListInput {
    pub state: Option<ArtifactInspectionState>,
    /// Keyset continuation token (ADR 0031): scan handles with `id < after_id`.
    pub after_id: Option<u64>,
    pub limit: u32,
}

/// One page of `list_artifacts` results plus its keyset continuation token
/// (ADR 0031). Because the inspection `state` is derived per handle rather than
/// stored in a column, the cursor keys off the last *scanned* handle id — a page
/// that filters out every scanned row still advances and never loops.
#[derive(Debug, Clone)]
pub struct ArtifactListPage {
    pub artifacts: Vec<ArtifactSummary>,
    pub next_cursor: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub artifact_handle_id: ArtifactHandleId,
    pub state: ArtifactInspectionState,
    pub source_file_version_id: Option<FileVersionId>,
    pub staging_path: Option<PathBuf>,
    pub target_path: Option<PathBuf>,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
    pub latest_verification: Option<VerificationSummary>,
    pub latest_commit: Option<CommitSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDetail {
    pub artifact_handle_id: ArtifactHandleId,
    pub state: ArtifactInspectionState,
    pub source_file_version_id: Option<FileVersionId>,
    pub staging_path: Option<PathBuf>,
    pub target_path: Option<PathBuf>,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
    pub verifications: Vec<VerificationSummary>,
    pub commits: Vec<CommitSummary>,
    pub latest_verification: Option<VerificationSummary>,
    pub latest_commit: Option<CommitSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationSummary {
    pub id: ArtifactVerificationId,
    pub artifact_location_id: ArtifactLocationId,
    pub path: PathBuf,
    pub worker_id: WorkerId,
    pub status: ArtifactVerificationStatus,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub observed_size_bytes: Option<u64>,
    pub observed_checksum: Option<String>,
    pub failure_class: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSummary {
    pub id: ArtifactCommitRecordId,
    pub verification_id: ArtifactVerificationId,
    pub target_path: PathBuf,
    pub temp_path: Option<PathBuf>,
    pub state: ArtifactCommitState,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub failure_class: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub recovery_reason: Option<String>,
    pub recovery: Option<RecoverySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySummary {
    pub reason: Option<String>,
    pub target: PathObservation,
    pub temp: Option<PathObservation>,
    pub staging: Option<PathObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathObservation {
    pub path: PathBuf,
    pub exists: bool,
    pub facts: Option<PathFacts>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFacts {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub checksum: String,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone)]
struct HandleFacts {
    id: ArtifactHandleId,
    source_file_version_id: Option<FileVersionId>,
    size_bytes: Option<u64>,
    checksum: Option<String>,
}

impl ControlPlane {
    pub async fn list_artifacts(
        &self,
        input: ArtifactListInput,
    ) -> Result<ArtifactListPage, VoomError> {
        if input.limit == 0 {
            return Ok(ArtifactListPage {
                artifacts: Vec::new(),
                next_cursor: None,
            });
        }

        // Scan exactly `limit` handles per page (newest first), keyed off the
        // stable handle id. The derived `state` filter drops non-matching rows
        // from the result, but the cursor advances by the scan window so paging
        // stays bounded and never re-scans (ADR 0031).
        let handle_ids =
            list_handle_ids_newest_first(self, Some(input.limit), input.after_id).await?;
        let scanned = handle_ids.len();
        let mut artifacts = Vec::new();
        let mut last_scanned = None;
        for handle_id in handle_ids {
            last_scanned = Some(handle_id.0);
            let detail = build_artifact_detail(self, handle_id).await?;
            if input.state.is_none_or(|state| detail.state == state) {
                artifacts.push(detail.into_summary());
            }
        }
        let next_cursor = (scanned as u64 >= u64::from(input.limit))
            .then_some(last_scanned)
            .flatten();
        Ok(ArtifactListPage {
            artifacts,
            next_cursor,
        })
    }

    pub async fn show_artifact(
        &self,
        artifact_handle_id: ArtifactHandleId,
    ) -> Result<ArtifactDetail, VoomError> {
        build_artifact_detail(self, artifact_handle_id).await
    }
}

impl ArtifactDetail {
    fn into_summary(self) -> ArtifactSummary {
        ArtifactSummary {
            artifact_handle_id: self.artifact_handle_id,
            state: self.state,
            source_file_version_id: self.source_file_version_id,
            staging_path: self.staging_path,
            target_path: self.target_path,
            size_bytes: self.size_bytes,
            checksum: self.checksum,
            latest_verification: self.latest_verification,
            latest_commit: self.latest_commit,
        }
    }
}

async fn build_artifact_detail(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
) -> Result<ArtifactDetail, VoomError> {
    let facts = read_handle_facts(cp, handle_id).await?;
    let locations = cp.artifacts.list_locations_for_handle(facts.id).await?;
    let live_staging = one_live_staging_location(&locations);
    let verifications = cp
        .artifacts
        .list_verifications(facts.id)
        .await?
        .into_iter()
        .map(VerificationSummary::from)
        .collect::<Vec<_>>();
    let raw_commits = cp.artifacts.list_commit_records(facts.id).await?;
    let state = derive_state(live_staging, &verifications, &raw_commits);
    let staging_path = live_staging.map(|location| PathBuf::from(&location.value));
    let latest_verification = verifications.last().cloned();
    let commits = summarize_commits(raw_commits, staging_path.as_deref()).await?;
    let latest_commit = commits.last().cloned();
    let target_path = latest_commit
        .as_ref()
        .map(|commit| commit.target_path.clone());

    Ok(ArtifactDetail {
        artifact_handle_id: facts.id,
        state,
        source_file_version_id: facts.source_file_version_id,
        staging_path,
        target_path,
        size_bytes: facts.size_bytes,
        checksum: facts.checksum,
        verifications,
        commits,
        latest_verification,
        latest_commit,
    })
}

fn derive_state(
    live_staging: Option<&ArtifactLocation>,
    verifications: &[VerificationSummary],
    commits: &[ArtifactCommitRecord],
) -> ArtifactInspectionState {
    if let Some(commit) = commits.last() {
        return match commit.state {
            ArtifactCommitState::Committed => ArtifactInspectionState::Committed,
            ArtifactCommitState::Failed => ArtifactInspectionState::Failed,
            ArtifactCommitState::RecoveryRequired => ArtifactInspectionState::RecoveryRequired,
            ArtifactCommitState::Pending => ArtifactInspectionState::Staged,
        };
    }

    if let Some(staging) = live_staging
        && let Some(verification) = verifications.iter().rev().find(|verification| {
            verification.artifact_location_id == staging.id
                && verification.path.as_path() == Path::new(&staging.value)
        })
    {
        return match verification.status {
            ArtifactVerificationStatus::Succeeded => ArtifactInspectionState::Verified,
            ArtifactVerificationStatus::Failed => ArtifactInspectionState::Failed,
        };
    }

    ArtifactInspectionState::Staged
}

async fn summarize_commits(
    commits: Vec<ArtifactCommitRecord>,
    staging_path: Option<&Path>,
) -> Result<Vec<CommitSummary>, VoomError> {
    let mut summaries = Vec::with_capacity(commits.len());
    for commit in commits {
        summaries.push(summarize_commit(commit, staging_path).await?);
    }
    Ok(summaries)
}

async fn summarize_commit(
    commit: ArtifactCommitRecord,
    staging_path: Option<&Path>,
) -> Result<CommitSummary, VoomError> {
    let recovery = if commit.state == ArtifactCommitState::RecoveryRequired {
        Some(RecoverySummary {
            reason: commit.recovery_reason.clone(),
            target: observe_path(&commit.target_path).await?,
            temp: observe_optional_path(commit.temp_path.as_deref()).await?,
            staging: observe_optional_path(staging_path).await?,
        })
    } else {
        None
    };

    Ok(CommitSummary {
        id: commit.id,
        verification_id: commit.verification_id,
        target_path: PathBuf::from(commit.target_path),
        temp_path: commit.temp_path.map(PathBuf::from),
        state: commit.state,
        result_file_version_id: commit.result_file_version_id,
        result_file_location_id: commit.result_file_location_id,
        failure_class: commit.failure_class,
        error_code: commit.error_code,
        message: commit.message,
        recovery_reason: commit.recovery_reason,
        recovery,
    })
}

impl From<ArtifactVerification> for VerificationSummary {
    fn from(value: ArtifactVerification) -> Self {
        Self {
            id: value.id,
            artifact_location_id: value.artifact_location_id,
            path: PathBuf::from(value.path),
            worker_id: value.worker_id,
            status: value.status,
            expected_size_bytes: value.expected_size_bytes,
            expected_checksum: value.expected_checksum,
            observed_size_bytes: value.observed_size_bytes,
            observed_checksum: value.observed_checksum,
            failure_class: value.failure_class,
            error_code: value.error_code,
            message: value.message,
        }
    }
}

fn one_live_staging_location(locations: &[ArtifactLocation]) -> Option<&ArtifactLocation> {
    let mut staging = locations
        .iter()
        .filter(|location| location.kind == "staging");
    let first = staging.next()?;
    if staging.next().is_some() {
        return None;
    }
    Some(first)
}

async fn observe_optional_path(
    path: Option<impl AsRef<Path>>,
) -> Result<Option<PathObservation>, VoomError> {
    match path {
        Some(path) => observe_path(path).await.map(Some),
        None => Ok(None),
    }
}

async fn observe_path(path: impl AsRef<Path>) -> Result<PathObservation, VoomError> {
    let path = path.as_ref();
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PathObservation {
                path: path.to_path_buf(),
                exists: false,
                facts: None,
                error: None,
            });
        }
        Err(err) => {
            return Ok(PathObservation {
                path: path.to_path_buf(),
                exists: false,
                facts: None,
                error: Some(err.to_string()),
            });
        }
    }

    match observe_regular_file(path).await {
        Ok(facts) => Ok(PathObservation {
            path: path.to_path_buf(),
            exists: true,
            facts: Some(PathFacts {
                path: facts.path,
                size_bytes: facts.size_bytes,
                checksum: facts.content_hash,
                local_file_key: facts.local_file_key,
            }),
            error: None,
        }),
        Err(err) => Ok(PathObservation {
            path: path.to_path_buf(),
            exists: true,
            facts: None,
            error: Some(err.to_string()),
        }),
    }
}

async fn list_handle_ids_newest_first(
    cp: &ControlPlane,
    limit: Option<u32>,
    after_id: Option<u64>,
) -> Result<Vec<ArtifactHandleId>, VoomError> {
    let after = after_id
        .map(|id| i64_from_u64(id, "after_id"))
        .transpose()?;
    let rows = match limit {
        Some(limit) => {
            sqlx::query(
                "SELECT id FROM artifact_handles \
                 WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
            )
            .bind(after)
            .bind(i64::from(limit))
            .fetch_all(&cp.pool)
            .await
        }
        None => {
            sqlx::query(
                "SELECT id FROM artifact_handles \
                 WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC",
            )
            .bind(after)
            .fetch_all(&cp.pool)
            .await
        }
    }
    .map_err(|err| VoomError::database_context("artifact_handles list", err))?;
    rows.iter()
        .map(|row| {
            let id: i64 = row
                .try_get("id")
                .map_err(|err| VoomError::database_context("artifact_handles.id", err))?;
            Ok(ArtifactHandleId(u64_from_i64(id, "artifact_handles.id")?))
        })
        .collect()
}

async fn read_handle_facts(
    cp: &ControlPlane,
    handle_id: ArtifactHandleId,
) -> Result<HandleFacts, VoomError> {
    let row = sqlx::query(
        "SELECT id, file_version_id, size_bytes, checksum \
         FROM artifact_handles WHERE id = ?",
    )
    .bind(i64_from_u64(handle_id.0, "artifact_handles.id")?)
    .fetch_optional(&cp.pool)
    .await
    .map_err(|err| VoomError::database_context("artifact_handles facts", err))?
    .ok_or_else(|| VoomError::NotFound(format!("artifact_handles {handle_id} missing")))?;
    let id: i64 = row
        .try_get("id")
        .map_err(|err| VoomError::database_context("artifact_handles.id", err))?;
    let file_version_id: Option<i64> = row
        .try_get("file_version_id")
        .map_err(|err| VoomError::database_context("artifact_handles.file_version_id", err))?;
    let size_bytes: Option<i64> = row
        .try_get("size_bytes")
        .map_err(|err| VoomError::database_context("artifact_handles.size_bytes", err))?;
    let checksum: Option<String> = row
        .try_get("checksum")
        .map_err(|err| VoomError::database_context("artifact_handles.checksum", err))?;

    Ok(HandleFacts {
        id: ArtifactHandleId(u64_from_i64(id, "artifact_handles.id")?),
        source_file_version_id: file_version_id
            .map(|value| u64_from_i64(value, "artifact_handles.file_version_id"))
            .transpose()?
            .map(FileVersionId),
        size_bytes: size_bytes
            .map(|value| u64_from_i64(value, "artifact_handles.size_bytes"))
            .transpose()?,
        checksum,
    })
}

fn i64_from_u64(value: u64, name: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|err| VoomError::Internal(format!("{name} exceeds SQLite integer: {err}")))
}

fn u64_from_i64(value: i64, name: &str) -> Result<u64, VoomError> {
    u64::try_from(value)
        .map_err(|err| VoomError::database_context(format!("{name} is negative"), err))
}

#[cfg(test)]
#[path = "inspect_test.rs"]
mod tests;
