use std::io;
use std::path::Path;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_control_plane::scan::{
    RootScanBlocked, RootScanOutcome, ScanFileErrorReport, ScanFileReport, ScanMode, ScanPathInput,
    ScanReport, ScanReportFileStatus, ScanSidecarReport, is_supported_media_path,
};
use voom_core::{ErrorCode, FailureClass, LibraryRootId};

use crate::commands::common::open_control_plane;
use crate::envelope::{Local, emit_err, emit_err_with_data_and_warnings, emit_ok};

#[derive(Debug, Serialize)]
pub struct ScanData {
    pub path: String,
    pub mode: String,
    pub summary: ScanSummaryData,
    pub files: Vec<ScanFileData>,
    pub skipped: Vec<ScanFileData>,
}

#[derive(Debug, Serialize)]
pub struct ScanSummaryData {
    pub discovered: u64,
    pub ingested: u64,
    pub probed: u64,
    pub snapshots_recorded: u64,
    pub hardlinked: u64,
    pub skipped: u64,
    pub failed: u64,
}

#[derive(Debug, Serialize)]
pub struct ScanFileData {
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_asset_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_location_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_snapshot_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_worker_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_member_role: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<ScanSidecarData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ScanFileErrorData>,
}

#[derive(Debug, Serialize)]
pub struct ScanSidecarData {
    pub path: String,
    pub file_asset_id: u64,
    pub file_version_id: u64,
    pub file_location_id: u64,
    pub bundle_id: u64,
    pub bundle_member_role: String,
    pub content_hash: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ScanFileErrorData {
    pub code: &'static str,
    pub failure_class: String,
    pub message: String,
}

pub async fn run(
    database_url: &str,
    local: Local,
    path: Option<&Path>,
    root: Option<u64>,
) -> io::Result<i32> {
    match (path, root) {
        (Some(path), None) => run_explicit_path(database_url, local, path).await,
        (None, Some(root_id)) => run_root(database_url, local, LibraryRootId(root_id)).await,
        _ => {
            emit_err(
                "scan",
                ErrorCode::BadArgs.as_str(),
                "exactly one of --path or --root is required".to_owned(),
                None,
                Some(local),
            )?;
            Ok(1)
        }
    }
}

async fn run_explicit_path(database_url: &str, local: Local, path: &Path) -> io::Result<i32> {
    if let Err(message) = validate_explicit_path(path).await {
        emit_err(
            "scan",
            ErrorCode::BadArgs.as_str(),
            message,
            None,
            Some(local),
        )?;
        return Ok(1);
    }

    let cp = match open_control_plane("scan", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    scan_with_control_plane(&cp, local, path).await
}

async fn run_root(database_url: &str, local: Local, root_id: LibraryRootId) -> io::Result<i32> {
    let cp = match open_control_plane("scan", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.scan_library_root(root_id).await {
        Ok(RootScanOutcome::Scanned(report)) => {
            emit_ok("scan", ScanData::from(report), Some(local), scan_warnings()).map(|()| 0)
        }
        Ok(RootScanOutcome::Blocked(blocked)) => {
            let message = format!(
                "library root {} is blocked ({}); not scanned",
                blocked.library_root_id,
                blocked.reason.as_str()
            );
            emit_err_with_data_and_warnings(
                "scan",
                BlockedData::from(blocked),
                ErrorCode::Blocked.as_str(),
                message,
                None,
                Some(local),
                scan_warnings(),
            )?;
            Ok(2)
        }
        Err(err) => {
            let code = err.code();
            let message = err.to_string();
            emit_err_with_data_and_warnings(
                "scan",
                ScanData::from(err.into_report()),
                code.as_str(),
                message,
                None,
                Some(local),
                scan_warnings(),
            )?;
            Ok(2)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BlockedData {
    pub status: &'static str,
    pub reason: &'static str,
    pub library_id: u64,
    pub library_root_id: u64,
    pub canonical_path: String,
}

impl From<RootScanBlocked> for BlockedData {
    fn from(blocked: RootScanBlocked) -> Self {
        Self {
            status: "blocked",
            reason: blocked.reason.as_str(),
            library_id: blocked.library_id.0,
            library_root_id: blocked.library_root_id.0,
            canonical_path: path_wire(&blocked.canonical_path),
        }
    }
}

async fn scan_with_control_plane(cp: &ControlPlane, local: Local, path: &Path) -> io::Result<i32> {
    match cp
        .scan_path(ScanPathInput {
            path: path.to_path_buf(),
            extension_allowlist: Vec::new(),
        })
        .await
    {
        Ok(report) => {
            emit_ok("scan", ScanData::from(report), Some(local), scan_warnings()).map(|()| 0)
        }
        Err(err) => {
            let code = err.code();
            let message = err.to_string();
            emit_err_with_data_and_warnings(
                "scan",
                ScanData::from(err.into_report()),
                code.as_str(),
                message,
                None,
                Some(local),
                scan_warnings(),
            )?;
            Ok(2)
        }
    }
}

fn scan_warnings() -> Vec<String> {
    let Some(ffprobe_bin) = std::env::var_os("VOOM_FFPROBE_BIN") else {
        return Vec::new();
    };
    vec![format!(
        "VOOM_FFPROBE_BIN is set; scan ffprobe binary: {}",
        std::path::Path::new(&ffprobe_bin).display()
    )]
}

async fn validate_explicit_path(path: &Path) -> Result<(), String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|err| format!("cannot inspect scan path {}: {err}", path.display()))?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(format!(
            "scan path must not be a symlink: {}",
            path.display()
        ));
    }
    if file_type.is_file() {
        if is_supported_media_path(path) {
            return Ok(());
        }
        return Err(format!("unsupported media extension: {}", path.display()));
    }
    if file_type.is_dir() {
        return Ok(());
    }
    Err(format!(
        "scan path must be a file or directory: {}",
        path.display()
    ))
}

impl From<ScanReport> for ScanData {
    fn from(report: ScanReport) -> Self {
        Self {
            path: path_wire(&report.path),
            mode: mode_wire(report.mode).to_owned(),
            summary: ScanSummaryData {
                discovered: report.summary.discovered,
                ingested: report.summary.ingested,
                probed: report.summary.probed,
                snapshots_recorded: report.summary.snapshots_recorded,
                hardlinked: report.summary.hardlinked,
                skipped: report.summary.skipped,
                failed: report.summary.failed,
            },
            files: report.files.into_iter().map(ScanFileData::from).collect(),
            skipped: report.skipped.into_iter().map(ScanFileData::from).collect(),
        }
    }
}

impl From<ScanFileReport> for ScanFileData {
    fn from(file: ScanFileReport) -> Self {
        Self {
            path: path_wire(&file.path),
            status: status_wire(file.status).to_owned(),
            file_asset_id: file.file_asset_id.map(|id| id.0),
            file_version_id: file.file_version_id.map(|id| id.0),
            file_location_id: file.file_location_id.map(|id| id.0),
            media_snapshot_id: file.media_snapshot_id.map(|id| id.0),
            content_hash: file.content_hash,
            size_bytes: file.size_bytes,
            probe_worker_id: file.probe_worker_id.map(|id| id.0),
            bundle_id: file.bundle_id.map(|id| id.0),
            bundle_member_role: file.bundle_member_role,
            sidecars: file
                .sidecars
                .into_iter()
                .map(ScanSidecarData::from)
                .collect(),
            error: file.error.map(ScanFileErrorData::from),
        }
    }
}

impl From<ScanSidecarReport> for ScanSidecarData {
    fn from(sidecar: ScanSidecarReport) -> Self {
        Self {
            path: path_wire(&sidecar.path),
            file_asset_id: sidecar.file_asset_id.0,
            file_version_id: sidecar.file_version_id.0,
            file_location_id: sidecar.file_location_id.0,
            bundle_id: sidecar.bundle_id.0,
            bundle_member_role: sidecar.bundle_member_role,
            content_hash: sidecar.content_hash,
            size_bytes: sidecar.size_bytes,
        }
    }
}

impl From<ScanFileErrorReport> for ScanFileErrorData {
    fn from(error: ScanFileErrorReport) -> Self {
        Self {
            code: error.code.as_str(),
            failure_class: failure_class_wire(error.failure_class),
            message: error.message,
        }
    }
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

fn mode_wire(mode: ScanMode) -> &'static str {
    match mode {
        ScanMode::File => "file",
        ScanMode::Directory => "directory",
    }
}

fn status_wire(status: ScanReportFileStatus) -> &'static str {
    match status {
        ScanReportFileStatus::Scanned => "scanned",
        ScanReportFileStatus::ScannedHardlink => "scanned_hardlink",
        ScanReportFileStatus::SkippedInaccessible => "skipped_inaccessible",
        ScanReportFileStatus::SkippedUnsupportedExtension => "skipped_unsupported_extension",
        ScanReportFileStatus::FailedContentDrift => "failed_content_drift",
        ScanReportFileStatus::Failed => "failed",
    }
}

#[must_use]
pub fn failure_class_wire(class: FailureClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "malformed_worker_result".to_owned())
}

#[cfg(test)]
#[path = "scan_test.rs"]
mod tests;
