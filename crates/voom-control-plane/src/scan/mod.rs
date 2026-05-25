use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::Duration;

use voom_core::{
    ErrorCode, FailureClass, FileAssetId, FileLocationId, FileVersionId, MediaSnapshotId,
    VoomError, WorkerId,
};
use voom_worker_protocol::{ExpectedFileFacts, ProbeFileRequest, ProbeFileResult};

use crate::ControlPlane;

pub mod bootstrap;
pub mod discovery;
pub mod hash;
pub mod persist;
pub mod worker;

#[derive(Debug, Clone)]
pub struct ScanPathInput {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub path: PathBuf,
    pub mode: discovery::ScanMode,
    pub summary: ScanSummary,
    pub files: Vec<ScanFileReport>,
    pub skipped: Vec<ScanFileReport>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub discovered: u64,
    pub ingested: u64,
    pub probed: u64,
    pub snapshots_recorded: u64,
    pub skipped: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFileReport {
    pub path: PathBuf,
    pub status: ScanReportFileStatus,
    pub file_asset_id: Option<FileAssetId>,
    pub file_version_id: Option<FileVersionId>,
    pub file_location_id: Option<FileLocationId>,
    pub media_snapshot_id: Option<MediaSnapshotId>,
    pub content_hash: Option<String>,
    pub size_bytes: Option<u64>,
    pub probe_worker_id: Option<WorkerId>,
    pub error: Option<ScanFileErrorReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanReportFileStatus {
    Scanned,
    SkippedInaccessible,
    SkippedUnsupportedExtension,
    FailedContentDrift,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFileErrorReport {
    pub code: ErrorCode,
    pub failure_class: FailureClass,
    pub message: String,
}

#[derive(Debug)]
pub struct ScanCommandError {
    code: ErrorCode,
    message: String,
    report: ScanReport,
}

impl ScanCommandError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub fn report(&self) -> &ScanReport {
        &self.report
    }

    #[must_use]
    pub fn into_report(self) -> ScanReport {
        self.report
    }
}

impl Display for ScanCommandError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ScanCommandError {}

impl ControlPlane {
    /// Scan an explicit file or directory path, persisting each successfully
    /// probed media file and returning a report of successes, skips, and the
    /// first selected-file failure.
    pub async fn scan_path(&self, input: ScanPathInput) -> Result<ScanReport, ScanCommandError> {
        let mut launcher = BundledFfprobeLauncher;
        self.scan_path_with_launcher(input, &mut launcher).await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "scan orchestration is a linear per-file state machine; splitting would obscure the failure report updates"
    )]
    async fn scan_path_with_launcher<L>(
        &self,
        input: ScanPathInput,
        launcher: &mut L,
    ) -> Result<ScanReport, ScanCommandError>
    where
        L: ScanWorkerLauncher + Send,
    {
        let discovered = discovery::discover_path(&input.path)
            .await
            .map_err(|err| discovery_error(input.path.clone(), &err))?;
        let mut report = ScanReport::from_discovered(&discovered);
        if discovered.candidates.is_empty() {
            return Ok(report);
        }
        let worker_id = self
            .ensure_scan_worker()
            .await
            .map_err(|err| command_error_from_voom(&err, report.clone()))?;
        let mut worker = launcher
            .launch_ffprobe(worker_id)
            .await
            .map_err(|err| command_error_from_worker(&err, report.clone()))?;

        let result = async {
            for candidate in discovered.candidates {
                let candidate_facts = match hash::observe_candidate_file(&candidate.path).await {
                    Ok(facts) => facts,
                    Err(err) => {
                        push_scan_error(
                            &mut report,
                            candidate.path,
                            ScanReportFileStatus::Failed,
                            None,
                            None,
                            scan_file_error_from_discovery(&err),
                        );
                        return Err(command_error_from_report_tail(report));
                    }
                };
                let request = match probe_request(&candidate.path, &candidate_facts) {
                    Ok(request) => request,
                    Err(error) => {
                        push_scan_error(
                            &mut report,
                            candidate.path,
                            ScanReportFileStatus::Failed,
                            Some(&candidate_facts),
                            Some(worker.worker_id()),
                            error,
                        );
                        return Err(command_error_from_report_tail(report));
                    }
                };
                launcher.record_dispatch(worker.worker_id());
                let probe = match worker.dispatch_probe_file(request).await {
                    Ok(probe) => {
                        report.summary.probed += 1;
                        probe
                    }
                    Err(err) => {
                        push_scan_error(
                            &mut report,
                            candidate.path,
                            ScanReportFileStatus::Failed,
                            Some(&candidate_facts),
                            Some(worker.worker_id()),
                            scan_file_error_from_worker(&err),
                        );
                        return Err(command_error_from_worker(&err, report));
                    }
                };
                match persist::persist_scanned_media_snapshot(
                    self,
                    worker.worker_id(),
                    &candidate.path,
                    &candidate_facts,
                    &probe,
                )
                .await
                {
                    Ok(persisted) => {
                        report.summary.ingested += 1;
                        report.summary.snapshots_recorded += 1;
                        report.files.push(scanned_file_report(
                            candidate.path,
                            &candidate_facts,
                            worker.worker_id(),
                            &persisted,
                        ));
                    }
                    Err(persist::ScanPersistError::File(err)) => {
                        push_scan_error(
                            &mut report,
                            candidate.path,
                            err.status(),
                            Some(&candidate_facts),
                            Some(worker.worker_id()),
                            scan_file_error_from_persist(&err),
                        );
                        return Err(command_error_from_report_tail(report));
                    }
                    Err(persist::ScanPersistError::Store(err)) => {
                        push_scan_error(
                            &mut report,
                            candidate.path,
                            ScanReportFileStatus::Failed,
                            Some(&candidate_facts),
                            Some(worker.worker_id()),
                            scan_file_error_from_voom(&err),
                        );
                        return Err(command_error_from_voom(&err, report));
                    }
                }
            }

            Ok(report)
        }
        .await;
        worker.shutdown().await;
        result
    }

    async fn ensure_scan_worker(&self) -> Result<WorkerId, VoomError> {
        let mut tx =
            self.pool.begin().await.map_err(|err| {
                VoomError::Database(format!("scan worker bootstrap begin: {err}"))
            })?;
        let worker = bootstrap::ensure_builtin_ffprobe_worker_in_tx(self, &mut tx).await?;
        tx.commit()
            .await
            .map_err(|err| VoomError::Database(format!("scan worker bootstrap commit: {err}")))?;
        Ok(worker.id)
    }
}

#[async_trait::async_trait]
trait ScanWorkerLauncher {
    async fn launch_ffprobe(
        &mut self,
        worker_id: WorkerId,
    ) -> Result<Box<dyn ProbeWorkerSession + Send>, worker::ScanWorkerError>;

    fn record_dispatch(&mut self, _worker_id: WorkerId) {}
}

#[async_trait::async_trait]
trait ProbeWorkerSession {
    fn worker_id(&self) -> WorkerId;

    async fn dispatch_probe_file(
        &mut self,
        request: ProbeFileRequest,
    ) -> Result<ProbeFileResult, worker::ScanWorkerError>;

    async fn shutdown(self: Box<Self>);
}

struct BundledFfprobeLauncher;

#[async_trait::async_trait]
impl ScanWorkerLauncher for BundledFfprobeLauncher {
    async fn launch_ffprobe(
        &mut self,
        worker_id: WorkerId,
    ) -> Result<Box<dyn ProbeWorkerSession + Send>, worker::ScanWorkerError> {
        worker::BundledWorkerProcess::launch_bundled_ffprobe(worker_id)
            .await
            .map(|worker| Box::new(worker) as Box<dyn ProbeWorkerSession + Send>)
    }
}

#[async_trait::async_trait]
impl ProbeWorkerSession for worker::BundledWorkerProcess {
    fn worker_id(&self) -> WorkerId {
        self.worker_id
    }

    async fn dispatch_probe_file(
        &mut self,
        request: ProbeFileRequest,
    ) -> Result<ProbeFileResult, worker::ScanWorkerError> {
        self.dispatch_probe_file(request).await
    }

    async fn shutdown(self: Box<Self>) {
        let _status = (*self).shutdown(Duration::from_secs(5)).await;
    }
}

impl ScanReport {
    fn from_discovered(discovered: &discovery::DiscoveredScan) -> Self {
        let skipped = discovered
            .skipped
            .iter()
            .map(|file| ScanFileReport {
                path: file.path.clone(),
                status: file_status_from_discovery(file.status),
                file_asset_id: None,
                file_version_id: None,
                file_location_id: None,
                media_snapshot_id: None,
                content_hash: None,
                size_bytes: None,
                probe_worker_id: None,
                error: None,
            })
            .collect::<Vec<_>>();
        let discovered_count =
            u64::try_from(discovered.candidates.len() + skipped.len()).unwrap_or(u64::MAX);
        Self {
            path: discovered.root.clone(),
            mode: discovered.mode,
            summary: ScanSummary {
                discovered: discovered_count,
                skipped: u64::try_from(skipped.len()).unwrap_or(u64::MAX),
                ..ScanSummary::default()
            },
            files: Vec::new(),
            skipped,
        }
    }
}

fn probe_request(
    path: &std::path::Path,
    facts: &hash::ObservedFileFacts,
) -> Result<ProbeFileRequest, ScanFileErrorReport> {
    let path = path.to_str().ok_or_else(|| ScanFileErrorReport {
        code: ErrorCode::ConfigInvalid,
        failure_class: FailureClass::MalformedWorkerResult,
        message: format!(
            "scan path is not valid UTF-8 and cannot be sent to worker: {}",
            path.display()
        ),
    })?;
    Ok(ProbeFileRequest {
        path: path.to_owned(),
        expected: ExpectedFileFacts {
            size_bytes: facts.size_bytes,
            content_hash: facts.content_hash.clone(),
            modified_at: facts
                .modified_at
                .map(|modified| chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339()),
            local_file_key: None,
        },
    })
}

fn scanned_file_report(
    path: PathBuf,
    facts: &hash::ObservedFileFacts,
    worker_id: WorkerId,
    persisted: &persist::PersistedScan,
) -> ScanFileReport {
    ScanFileReport {
        path,
        status: ScanReportFileStatus::Scanned,
        file_asset_id: Some(persisted.file_asset_id),
        file_version_id: Some(persisted.file_version_id),
        file_location_id: Some(persisted.file_location_id),
        media_snapshot_id: Some(persisted.media_snapshot_id),
        content_hash: Some(facts.content_hash.clone()),
        size_bytes: Some(facts.size_bytes),
        probe_worker_id: Some(worker_id),
        error: None,
    }
}

fn push_scan_error(
    report: &mut ScanReport,
    path: PathBuf,
    status: ScanReportFileStatus,
    facts: Option<&hash::ObservedFileFacts>,
    worker_id: Option<WorkerId>,
    error: ScanFileErrorReport,
) {
    report.summary.failed += 1;
    report.files.push(ScanFileReport {
        path,
        status,
        file_asset_id: None,
        file_version_id: None,
        file_location_id: None,
        media_snapshot_id: None,
        content_hash: facts.map(|facts| facts.content_hash.clone()),
        size_bytes: facts.map(|facts| facts.size_bytes),
        probe_worker_id: worker_id,
        error: Some(error),
    });
}

fn file_status_from_discovery(status: discovery::FileScanStatus) -> ScanReportFileStatus {
    match status {
        discovery::FileScanStatus::SkippedInaccessible => ScanReportFileStatus::SkippedInaccessible,
        discovery::FileScanStatus::SkippedSymlink
        | discovery::FileScanStatus::SkippedUnsupportedExtension => {
            ScanReportFileStatus::SkippedUnsupportedExtension
        }
    }
}

fn scan_file_error_from_worker(err: &worker::ScanWorkerError) -> ScanFileErrorReport {
    ScanFileErrorReport {
        code: err.error_code(),
        failure_class: err.failure_class(),
        message: err.to_string(),
    }
}

fn scan_file_error_from_persist(err: &persist::ScanFileError) -> ScanFileErrorReport {
    ScanFileErrorReport {
        code: err.error_code(),
        failure_class: err.failure_class(),
        message: err.message().to_owned(),
    }
}

fn scan_file_error_from_voom(err: &VoomError) -> ScanFileErrorReport {
    ScanFileErrorReport {
        code: err.error_code(),
        failure_class: FailureClass::MalformedWorkerResult,
        message: err.to_string(),
    }
}

fn scan_file_error_from_discovery(err: &discovery::ScanError) -> ScanFileErrorReport {
    ScanFileErrorReport {
        code: err.error_code(),
        failure_class: FailureClass::ArtifactUnavailable,
        message: err.to_string(),
    }
}

fn discovery_error(path: PathBuf, err: &discovery::ScanError) -> ScanCommandError {
    ScanCommandError {
        code: err.error_code(),
        message: err.to_string(),
        report: ScanReport {
            path,
            mode: discovery::ScanMode::File,
            summary: ScanSummary::default(),
            files: Vec::new(),
            skipped: Vec::new(),
        },
    }
}

fn command_error_from_voom(err: &VoomError, report: ScanReport) -> ScanCommandError {
    ScanCommandError {
        code: err.error_code(),
        message: err.to_string(),
        report,
    }
}

fn command_error_from_worker(
    err: &worker::ScanWorkerError,
    report: ScanReport,
) -> ScanCommandError {
    ScanCommandError {
        code: err.error_code(),
        message: err.to_string(),
        report,
    }
}

fn command_error_from_report_tail(report: ScanReport) -> ScanCommandError {
    let Some(file_error) = report.files.last().and_then(|file| file.error.as_ref()) else {
        return ScanCommandError {
            code: ErrorCode::Internal,
            message: "scan failed without a file error".to_owned(),
            report,
        };
    };
    ScanCommandError {
        code: file_error.code,
        message: file_error.message.clone(),
        report,
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;
