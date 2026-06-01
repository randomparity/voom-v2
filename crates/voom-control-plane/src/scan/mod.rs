use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::Duration;

use voom_core::{
    BundleId, ErrorCode, FailureClass, FileAssetId, FileLocationId, FileVersionId, MediaSnapshotId,
    VoomError, WorkerId, format_iso8601,
};
use voom_worker_protocol::{ExpectedFileFacts, ProbeFileRequest, ProbeFileResult};

use crate::ControlPlane;

pub(crate) mod bootstrap;
pub(crate) mod discovery;
pub(crate) mod hash;
pub(crate) mod persist;
pub(crate) mod worker;

pub use discovery::{ScanMode, is_supported_media_path, is_supported_sidecar_path};

#[derive(Debug, Clone)]
pub struct ScanPathInput {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub path: PathBuf,
    pub mode: ScanMode,
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
    pub bundle_id: Option<BundleId>,
    pub bundle_member_role: Option<String>,
    pub sidecars: Vec<ScanSidecarReport>,
    pub error: Option<ScanFileErrorReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSidecarReport {
    pub path: PathBuf,
    pub file_asset_id: FileAssetId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
    pub bundle_id: BundleId,
    pub bundle_member_role: String,
    pub content_hash: String,
    pub size_bytes: u64,
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
        let mut report = ScanReportBuilder::from_discovered(&discovered);
        if discovered.candidates.is_empty() {
            return Ok(report.finish());
        }
        let mut worker = self.launch_scan_worker(launcher, &report).await?;
        let worker_id = worker.worker_id();

        let result = async {
            for candidate in discovered.candidates {
                let candidate_facts = match hash::observe_candidate_file(&candidate.path).await {
                    Ok(facts) => facts,
                    Err(err) => {
                        return Err(report.fail_observe(candidate.path, &err));
                    }
                };
                let request = match probe_request(&candidate.path, &candidate_facts) {
                    Ok(request) => request,
                    Err(error) => {
                        return Err(report.fail_probe_request(
                            candidate.path,
                            &candidate_facts,
                            worker_id,
                            error,
                        ));
                    }
                };
                launcher.record_dispatch(worker_id);
                let probe = match worker.dispatch_probe_file(request).await {
                    Ok(probe) => {
                        report.record_probe();
                        probe
                    }
                    Err(err) => {
                        return Err(report.fail_worker(
                            candidate.path,
                            &candidate_facts,
                            worker_id,
                            &err,
                        ));
                    }
                };
                match persist::persist_scanned_media_snapshot(
                    self,
                    worker.worker_id(),
                    &candidate.path,
                    &candidate.sidecars,
                    &candidate_facts,
                    &probe,
                )
                .await
                {
                    Ok(persisted) => {
                        report.push_scanned_file(
                            candidate.path,
                            &candidate_facts,
                            worker_id,
                            &persisted,
                        );
                    }
                    Err(persist::ScanPersistError::File(err)) => {
                        return Err(report.fail_persist_file(
                            candidate.path,
                            &candidate_facts,
                            worker_id,
                            &err,
                        ));
                    }
                    Err(persist::ScanPersistError::Store(err)) => {
                        return Err(report.fail_voom(
                            candidate.path,
                            &candidate_facts,
                            worker_id,
                            &err,
                        ));
                    }
                }
            }

            Ok(report.finish())
        }
        .await;
        worker.shutdown().await;
        result
    }

    async fn launch_scan_worker<L>(
        &self,
        launcher: &mut L,
        report: &ScanReportBuilder,
    ) -> Result<Box<dyn ProbeWorkerSession + Send>, ScanCommandError>
    where
        L: ScanWorkerLauncher + Send,
    {
        let worker_id = self
            .ensure_scan_worker()
            .await
            .map_err(|err| command_error_from_voom(&err, report.report().clone()))?;
        launcher
            .launch_ffprobe(worker_id)
            .await
            .map_err(|err| command_error_from_worker(&err, report.report().clone()))
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
        self.worker_id()
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

struct ScanReportBuilder {
    report: ScanReport,
}

impl ScanReportBuilder {
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
                bundle_id: None,
                bundle_member_role: None,
                sidecars: Vec::new(),
                error: None,
            })
            .collect::<Vec<_>>();
        let sidecar_count = discovered
            .candidates
            .iter()
            .map(|candidate| candidate.sidecars.len())
            .sum::<usize>();
        let discovered_count =
            u64::try_from(discovered.candidates.len() + sidecar_count + skipped.len())
                .unwrap_or(u64::MAX);
        Self {
            report: ScanReport {
                path: discovered.root.clone(),
                mode: discovered.mode,
                summary: ScanSummary {
                    discovered: discovered_count,
                    skipped: u64::try_from(skipped.len()).unwrap_or(u64::MAX),
                    ..ScanSummary::default()
                },
                files: Vec::new(),
                skipped,
            },
        }
    }

    const fn report(&self) -> &ScanReport {
        &self.report
    }

    fn finish(self) -> ScanReport {
        self.report
    }

    fn record_probe(&mut self) {
        self.report.summary.probed += 1;
    }

    fn push_scanned_file(
        &mut self,
        path: PathBuf,
        facts: &hash::ObservedFileFacts,
        worker_id: WorkerId,
        persisted: &persist::PersistedScan,
    ) {
        self.report.summary.ingested += 1;
        self.report.summary.ingested += u64::try_from(persisted.sidecars.len()).unwrap_or(u64::MAX);
        self.report.summary.snapshots_recorded += 1;
        self.report
            .files
            .push(Self::scanned_file_report(path, facts, worker_id, persisted));
    }

    fn push_error(
        &mut self,
        path: PathBuf,
        status: ScanReportFileStatus,
        facts: Option<&hash::ObservedFileFacts>,
        worker_id: Option<WorkerId>,
        error: ScanFileErrorReport,
    ) {
        self.report.summary.failed += 1;
        self.report.files.push(ScanFileReport {
            path,
            status,
            file_asset_id: None,
            file_version_id: None,
            file_location_id: None,
            media_snapshot_id: None,
            content_hash: facts.map(|facts| facts.content_hash.clone()),
            size_bytes: facts.map(|facts| facts.size_bytes),
            probe_worker_id: worker_id,
            bundle_id: None,
            bundle_member_role: None,
            sidecars: Vec::new(),
            error: Some(error),
        });
    }

    fn fail_file(
        mut self,
        path: PathBuf,
        status: ScanReportFileStatus,
        facts: Option<&hash::ObservedFileFacts>,
        worker_id: Option<WorkerId>,
        error: ScanFileErrorReport,
    ) -> ScanCommandError {
        self.push_error(path, status, facts, worker_id, error);
        command_error_from_report_tail(self.finish())
    }

    fn fail_observe(self, path: PathBuf, err: &discovery::ScanError) -> ScanCommandError {
        self.fail_file(
            path,
            ScanReportFileStatus::Failed,
            None,
            None,
            scan_file_error_from_discovery(err),
        )
    }

    fn fail_probe_request(
        self,
        path: PathBuf,
        facts: &hash::ObservedFileFacts,
        worker_id: WorkerId,
        error: ScanFileErrorReport,
    ) -> ScanCommandError {
        self.fail_file(
            path,
            ScanReportFileStatus::Failed,
            Some(facts),
            Some(worker_id),
            error,
        )
    }

    fn fail_persist_file(
        self,
        path: PathBuf,
        facts: &hash::ObservedFileFacts,
        worker_id: WorkerId,
        err: &persist::ScanFileError,
    ) -> ScanCommandError {
        self.fail_file(
            path,
            err.status(),
            Some(facts),
            Some(worker_id),
            scan_file_error_from_persist(err),
        )
    }

    fn fail_worker(
        mut self,
        path: PathBuf,
        facts: &hash::ObservedFileFacts,
        worker_id: WorkerId,
        err: &worker::ScanWorkerError,
    ) -> ScanCommandError {
        self.push_error(
            path,
            ScanReportFileStatus::Failed,
            Some(facts),
            Some(worker_id),
            scan_file_error_from_worker(err),
        );
        command_error_from_worker(err, self.finish())
    }

    fn fail_voom(
        mut self,
        path: PathBuf,
        facts: &hash::ObservedFileFacts,
        worker_id: WorkerId,
        err: &VoomError,
    ) -> ScanCommandError {
        self.push_error(
            path,
            ScanReportFileStatus::Failed,
            Some(facts),
            Some(worker_id),
            scan_file_error_from_voom(err),
        );
        command_error_from_voom(err, self.finish())
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
            bundle_id: persisted.bundle_id,
            bundle_member_role: persisted.bundle_member_role.clone(),
            sidecars: persisted
                .sidecars
                .iter()
                .map(|sidecar| ScanSidecarReport {
                    path: sidecar.path.clone(),
                    file_asset_id: sidecar.file_asset_id,
                    file_version_id: sidecar.file_version_id,
                    file_location_id: sidecar.file_location_id,
                    bundle_id: sidecar.bundle_id,
                    bundle_member_role: sidecar.bundle_member_role.clone(),
                    content_hash: sidecar.content_hash.clone(),
                    size_bytes: sidecar.size_bytes,
                })
                .collect(),
            error: None,
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
                .map(|modified| format_iso8601(time::OffsetDateTime::from(modified))),
            local_file_key: None,
        },
    })
}

fn file_status_from_discovery(status: discovery::FileScanStatus) -> ScanReportFileStatus {
    match status {
        discovery::FileScanStatus::Inaccessible => ScanReportFileStatus::SkippedInaccessible,
        discovery::FileScanStatus::Symlink | discovery::FileScanStatus::UnsupportedExtension => {
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
