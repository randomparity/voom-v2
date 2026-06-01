use std::io;
use std::path::Path;

use serde::Serialize;
use voom_control_plane::artifact::{
    ArtifactDetail, ArtifactInspectionState, ArtifactListInput, ArtifactSummary,
    CommitArtifactInput, CommitArtifactPreMutationReport, CommitArtifactReport,
    CommitRecoveryReport, CommitSummary, PathFacts, PathObservation, RecoverySummary,
    StageCopyInput, StageCopyReport, VerificationSummary, VerifyArtifactInput,
    VerifyArtifactReport,
};
use voom_core::{ArtifactHandleId, ErrorCode, FileLocationId, FileVersionId};
use voom_store::repo::artifacts::{ArtifactCommitState, ArtifactVerificationStatus};

use crate::cli::{ArtifactCommand, ArtifactStateArg};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_err_with_data, emit_ok};

const COMMAND_STAGE_COPY: &str = "artifact.stage_copy";
const COMMAND_VERIFY: &str = "artifact.verify";
const COMMAND_COMMIT: &str = "artifact.commit";
const COMMAND_LIST: &str = "artifact.list";
const COMMAND_SHOW: &str = "artifact.show";

#[derive(Debug, Serialize)]
struct ArtifactEnvelopeData<T> {
    artifact: T,
}

#[derive(Debug, Serialize)]
struct ArtifactListData {
    artifacts: Vec<ArtifactSummaryData>,
}

#[derive(Debug, Serialize)]
struct StageCopyData {
    artifact_handle_id: u64,
    artifact_location_id: u64,
    source_file_version_id: u64,
    source_location_id: u64,
    source_path: String,
    staging_path: String,
    size_bytes: u64,
    checksum: String,
}

#[derive(Debug, Serialize)]
struct VerifyArtifactData {
    artifact_handle_id: u64,
    artifact_location_id: u64,
    verification_id: u64,
    worker_id: u64,
    status: &'static str,
    path: String,
    expected_size_bytes: u64,
    expected_checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct CommitArtifactData {
    commit_record_id: u64,
    artifact_handle_id: u64,
    verification_id: u64,
    target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    temp_path: Option<String>,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_file_location_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_required: Option<CommitRecoveryData>,
}

#[derive(Debug, Serialize)]
struct CommitPreMutationData {
    artifact_handle_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_id: Option<u64>,
    target_path: String,
    error_code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct CommitRecoveryData {
    recovery_reason: String,
    target_path: String,
    target_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temp_path: Option<String>,
    temp_exists: bool,
    staging_path: String,
    staging_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_file_location_id: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ArtifactSummaryData {
    artifact_handle_id: u64,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staging_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_verification: Option<VerificationSummaryData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_commit: Option<CommitSummaryData>,
}

#[derive(Debug, Serialize)]
struct ArtifactDetailData {
    artifact_handle_id: u64,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staging_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    verifications: Vec<VerificationSummaryData>,
    commits: Vec<CommitSummaryData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_verification: Option<VerificationSummaryData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_commit: Option<CommitSummaryData>,
}

#[derive(Debug, Serialize)]
struct VerificationSummaryData {
    id: u64,
    artifact_location_id: u64,
    path: String,
    worker_id: u64,
    status: &'static str,
    expected_size_bytes: u64,
    expected_checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct CommitSummaryData {
    id: u64,
    verification_id: u64,
    target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    temp_path: Option<String>,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_file_location_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<RecoverySummaryData>,
}

#[derive(Debug, Serialize)]
struct RecoverySummaryData {
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    target: PathObservationData,
    #[serde(skip_serializing_if = "Option::is_none")]
    temp: Option<PathObservationData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staging: Option<PathObservationData>,
}

#[derive(Debug, Serialize)]
struct PathObservationData {
    path: String,
    exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    facts: Option<PathFactsData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PathFactsData {
    path: String,
    size_bytes: u64,
    checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_file_key: Option<String>,
}

pub async fn run(database_url: &str, local: Local, command: ArtifactCommand) -> io::Result<i32> {
    match command {
        ArtifactCommand::StageCopy {
            file_version_id,
            source_location_id,
            staging_path,
        } => {
            stage_copy(
                database_url,
                local,
                file_version_id,
                source_location_id,
                staging_path.as_path(),
            )
            .await
        }
        ArtifactCommand::Verify { artifact_handle_id } => {
            verify(database_url, local, artifact_handle_id).await
        }
        ArtifactCommand::Commit {
            artifact_handle_id,
            target_path,
        } => {
            commit(
                database_url,
                local,
                artifact_handle_id,
                target_path.as_path(),
            )
            .await
        }
        ArtifactCommand::List { state, limit } => list(database_url, local, state, limit).await,
        ArtifactCommand::Show { artifact_handle_id } => {
            show(database_url, local, artifact_handle_id).await
        }
    }
}

async fn stage_copy(
    database_url: &str,
    local: Local,
    file_version_id: u64,
    source_location_id: Option<u64>,
    staging_path: &Path,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND_STAGE_COPY, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .stage_copy(StageCopyInput {
            file_version_id: FileVersionId(file_version_id),
            source_location_id: source_location_id.map(FileLocationId),
            staging_path: staging_path.to_path_buf(),
        })
        .await
    {
        Ok(report) => emit_ok(
            COMMAND_STAGE_COPY,
            ArtifactEnvelopeData {
                artifact: StageCopyData::from(report),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => {
            let code = command_error_code(err.code());
            match err.data() {
                Some(data) => emit_err_with_data(
                    COMMAND_STAGE_COPY,
                    data,
                    code,
                    err.to_string(),
                    None,
                    Some(local),
                )?,
                None => emit_err(COMMAND_STAGE_COPY, code, err.to_string(), None, Some(local))?,
            }
            Ok(2)
        }
    }
}

async fn verify(database_url: &str, local: Local, artifact_handle_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND_VERIFY, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .verify_artifact(VerifyArtifactInput {
            artifact_handle_id: ArtifactHandleId(artifact_handle_id),
        })
        .await
    {
        Ok(report) => emit_ok(
            COMMAND_VERIFY,
            ArtifactEnvelopeData {
                artifact: VerifyArtifactData::from(report),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND_VERIFY, &err, local),
    }
}

async fn commit(
    database_url: &str,
    local: Local,
    artifact_handle_id: u64,
    target_path: &Path,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND_COMMIT, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: ArtifactHandleId(artifact_handle_id),
            target_path: target_path.to_path_buf(),
        })
        .await
    {
        Ok(report) => emit_ok(
            COMMAND_COMMIT,
            ArtifactEnvelopeData {
                artifact: CommitArtifactData::from(report),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => {
            let code = command_error_code(err.code());
            if let Some(report) = err.commit_report() {
                emit_err_with_data(
                    COMMAND_COMMIT,
                    ArtifactEnvelopeData {
                        artifact: CommitArtifactData::from(report.clone()),
                    },
                    code,
                    err.to_string(),
                    None,
                    Some(local),
                )?;
            } else if let Some(report) = err.pre_mutation_report() {
                emit_err_with_data(
                    COMMAND_COMMIT,
                    ArtifactEnvelopeData {
                        artifact: CommitPreMutationData::from(report.clone()),
                    },
                    code,
                    err.to_string(),
                    None,
                    Some(local),
                )?;
            } else {
                emit_err(COMMAND_COMMIT, code, err.to_string(), None, Some(local))?;
            }
            Ok(2)
        }
    }
}

async fn list(
    database_url: &str,
    local: Local,
    state: Option<ArtifactStateArg>,
    limit: u32,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND_LIST, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .list_artifacts(ArtifactListInput {
            state: state.map(artifact_state_to_control_plane),
            limit,
        })
        .await
    {
        Ok(artifacts) => emit_ok(
            COMMAND_LIST,
            ArtifactListData {
                artifacts: artifacts
                    .into_iter()
                    .map(ArtifactSummaryData::from)
                    .collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND_LIST, &err, local),
    }
}

async fn show(database_url: &str, local: Local, artifact_handle_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND_SHOW, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.show_artifact(ArtifactHandleId(artifact_handle_id)).await {
        Ok(artifact) => emit_ok(
            COMMAND_SHOW,
            ArtifactEnvelopeData {
                artifact: ArtifactDetailData::from(artifact),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND_SHOW, &err, local),
    }
}

fn artifact_state_to_control_plane(state: ArtifactStateArg) -> ArtifactInspectionState {
    match state {
        ArtifactStateArg::Staged => ArtifactInspectionState::Staged,
        ArtifactStateArg::Verified => ArtifactInspectionState::Verified,
        ArtifactStateArg::Committed => ArtifactInspectionState::Committed,
        ArtifactStateArg::Failed => ArtifactInspectionState::Failed,
        ArtifactStateArg::RecoveryRequired => ArtifactInspectionState::RecoveryRequired,
    }
}

fn command_error_code(code: ErrorCode) -> &'static str {
    code.as_str()
}

impl From<StageCopyReport> for StageCopyData {
    fn from(report: StageCopyReport) -> Self {
        Self {
            artifact_handle_id: report.artifact_handle_id.0,
            artifact_location_id: report.artifact_location_id.0,
            source_file_version_id: report.source_file_version_id.0,
            source_location_id: report.source_location_id.0,
            source_path: path_wire(&report.source_path),
            staging_path: path_wire(&report.staging_path),
            size_bytes: report.size_bytes,
            checksum: report.checksum,
        }
    }
}

impl From<VerifyArtifactReport> for VerifyArtifactData {
    fn from(report: VerifyArtifactReport) -> Self {
        Self {
            artifact_handle_id: report.artifact_handle_id.0,
            artifact_location_id: report.artifact_location_id.0,
            verification_id: report.verification_id.0,
            worker_id: report.worker_id.0,
            status: verification_status_wire(report.status),
            path: path_wire(&report.path),
            expected_size_bytes: report.expected_size_bytes,
            expected_checksum: report.expected_checksum,
            observed_size_bytes: report.observed_size_bytes,
            observed_checksum: report.observed_checksum,
            error_code: report.error_code.map(|code| code.as_str().to_owned()),
            message: report.message,
        }
    }
}

impl From<CommitArtifactReport> for CommitArtifactData {
    fn from(report: CommitArtifactReport) -> Self {
        Self {
            commit_record_id: report.commit_record_id.0,
            artifact_handle_id: report.artifact_handle_id.0,
            verification_id: report.verification_id.0,
            target_path: path_wire(&report.target_path),
            temp_path: report.temp_path.as_deref().map(path_wire),
            state: commit_state_wire(report.state),
            result_file_version_id: report.result_file_version_id.map(|id| id.0),
            result_file_location_id: report.result_file_location_id.map(|id| id.0),
            recovery_required: report.recovery_required.map(CommitRecoveryData::from),
        }
    }
}

impl From<CommitArtifactPreMutationReport> for CommitPreMutationData {
    fn from(report: CommitArtifactPreMutationReport) -> Self {
        Self {
            artifact_handle_id: report.artifact_handle_id.0,
            verification_id: report.verification_id.map(|id| id.0),
            target_path: path_wire(&report.target_path),
            error_code: report.error_code.as_str(),
            message: report.message,
        }
    }
}

impl From<CommitRecoveryReport> for CommitRecoveryData {
    fn from(report: CommitRecoveryReport) -> Self {
        Self {
            recovery_reason: report.recovery_reason,
            target_path: path_wire(&report.target_path),
            target_exists: report.target_exists,
            temp_path: report.temp_path.as_deref().map(path_wire),
            temp_exists: report.temp_exists,
            staging_path: path_wire(&report.staging_path),
            staging_exists: report.staging_exists,
            result_file_version_id: report.result_file_version_id.map(|id| id.0),
            result_file_location_id: report.result_file_location_id.map(|id| id.0),
        }
    }
}

impl From<ArtifactSummary> for ArtifactSummaryData {
    fn from(artifact: ArtifactSummary) -> Self {
        Self {
            artifact_handle_id: artifact.artifact_handle_id.0,
            state: inspection_state_wire(artifact.state),
            source_file_version_id: artifact.source_file_version_id.map(|id| id.0),
            staging_path: artifact.staging_path.as_deref().map(path_wire),
            target_path: artifact.target_path.as_deref().map(path_wire),
            size_bytes: artifact.size_bytes,
            checksum: artifact.checksum,
            latest_verification: artifact
                .latest_verification
                .map(VerificationSummaryData::from),
            latest_commit: artifact.latest_commit.map(CommitSummaryData::from),
        }
    }
}

impl From<ArtifactDetail> for ArtifactDetailData {
    fn from(artifact: ArtifactDetail) -> Self {
        Self {
            artifact_handle_id: artifact.artifact_handle_id.0,
            state: inspection_state_wire(artifact.state),
            source_file_version_id: artifact.source_file_version_id.map(|id| id.0),
            staging_path: artifact.staging_path.as_deref().map(path_wire),
            target_path: artifact.target_path.as_deref().map(path_wire),
            size_bytes: artifact.size_bytes,
            checksum: artifact.checksum,
            verifications: artifact
                .verifications
                .into_iter()
                .map(VerificationSummaryData::from)
                .collect(),
            commits: artifact
                .commits
                .into_iter()
                .map(CommitSummaryData::from)
                .collect(),
            latest_verification: artifact
                .latest_verification
                .map(VerificationSummaryData::from),
            latest_commit: artifact.latest_commit.map(CommitSummaryData::from),
        }
    }
}

impl From<VerificationSummary> for VerificationSummaryData {
    fn from(summary: VerificationSummary) -> Self {
        Self {
            id: summary.id.0,
            artifact_location_id: summary.artifact_location_id.0,
            path: path_wire(&summary.path),
            worker_id: summary.worker_id.0,
            status: verification_status_wire(summary.status),
            expected_size_bytes: summary.expected_size_bytes,
            expected_checksum: summary.expected_checksum,
            observed_size_bytes: summary.observed_size_bytes,
            observed_checksum: summary.observed_checksum,
            failure_class: summary.failure_class,
            error_code: summary.error_code,
            message: summary.message,
        }
    }
}

impl From<CommitSummary> for CommitSummaryData {
    fn from(summary: CommitSummary) -> Self {
        Self {
            id: summary.id.0,
            verification_id: summary.verification_id.0,
            target_path: path_wire(&summary.target_path),
            temp_path: summary.temp_path.as_deref().map(path_wire),
            state: commit_state_wire(summary.state),
            result_file_version_id: summary.result_file_version_id.map(|id| id.0),
            result_file_location_id: summary.result_file_location_id.map(|id| id.0),
            failure_class: summary.failure_class,
            error_code: summary.error_code,
            message: summary.message,
            recovery_reason: summary.recovery_reason,
            recovery: summary.recovery.map(RecoverySummaryData::from),
        }
    }
}

impl From<RecoverySummary> for RecoverySummaryData {
    fn from(summary: RecoverySummary) -> Self {
        Self {
            reason: summary.reason,
            target: PathObservationData::from(summary.target),
            temp: summary.temp.map(PathObservationData::from),
            staging: summary.staging.map(PathObservationData::from),
        }
    }
}

impl From<PathObservation> for PathObservationData {
    fn from(observation: PathObservation) -> Self {
        Self {
            path: path_wire(&observation.path),
            exists: observation.exists,
            facts: observation.facts.map(PathFactsData::from),
            error: observation.error,
        }
    }
}

impl From<PathFacts> for PathFactsData {
    fn from(facts: PathFacts) -> Self {
        Self {
            path: path_wire(&facts.path),
            size_bytes: facts.size_bytes,
            checksum: facts.checksum,
            local_file_key: facts.local_file_key,
        }
    }
}

fn inspection_state_wire(state: ArtifactInspectionState) -> &'static str {
    state.as_str()
}

fn verification_status_wire(status: ArtifactVerificationStatus) -> &'static str {
    status.as_str()
}

fn commit_state_wire(state: ArtifactCommitState) -> &'static str {
    state.as_str()
}

fn path_wire(path: &Path) -> String {
    path.to_str()
        .map_or_else(|| non_utf8_path_wire(path), str::to_owned)
}

#[cfg(unix)]
fn non_utf8_path_wire(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut encoded = String::from("os_bytes_hex:");
    for byte in path.as_os_str().as_bytes() {
        use std::fmt::Write as _;

        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(not(unix))]
fn non_utf8_path_wire(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
