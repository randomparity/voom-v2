use serde_json::json;
use voom_control_plane::scan::{
    ScanFileErrorReport, ScanFileReport, ScanMode, ScanReport, ScanReportFileStatus,
    ScanSidecarReport, ScanSummary,
};
use voom_core::{BundleId, ErrorCode, FailureClass, FileAssetId, FileLocationId, FileVersionId};

use super::{ScanData, ScanFileData, failure_class_wire};

#[test]
fn scan_data_serializes_to_spec_shape_with_rows_and_failure_errors() {
    let data = ScanData::from(report_fixture());

    let value = serde_json::to_value(data).unwrap();

    assert_eq!(
        value,
        json!({
            "path": "/library",
            "mode": "directory",
            "summary": {
                "discovered": 3,
                "ingested": 1,
                "probed": 1,
                "snapshots_recorded": 1,
                "skipped": 1,
                "failed": 1
            },
            "files": [
                {
                    "path": "/library/good.mkv",
                    "status": "scanned",
                    "file_asset_id": 10,
                    "file_version_id": 11,
                    "file_location_id": 12,
                    "media_snapshot_id": 13,
                    "content_hash": "blake3:good",
                    "size_bytes": 123,
                    "probe_worker_id": 44,
                    "bundle_id": 20,
                    "bundle_member_role": "primary_video",
                    "sidecars": [
                        {
                            "path": "/library/good.eng.srt",
                            "file_asset_id": 30,
                            "file_version_id": 31,
                            "file_location_id": 32,
                            "bundle_id": 20,
                            "bundle_member_role": "external_subtitle",
                            "content_hash": "sha256:sidecar",
                            "size_bytes": 45
                        }
                    ]
                },
                {
                    "path": "/library/bad.mkv",
                    "status": "failed",
                    "content_hash": "blake3:bad",
                    "size_bytes": 456,
                    "probe_worker_id": 44,
                    "error": {
                        "code": "EXTERNAL_SYSTEM_UNAVAILABLE",
                        "failure_class": "external_system_unavailable",
                        "message": "ffprobe unavailable"
                    }
                }
            ],
            "skipped": [
                {
                    "path": "/library/readme.txt",
                    "status": "skipped_unsupported_extension"
                }
            ]
        })
    );
}

#[test]
fn scan_file_data_uses_failed_content_drift_status() {
    let file = ScanFileData::from(ScanFileReport {
        path: "/library/drift.mkv".into(),
        status: ScanReportFileStatus::FailedContentDrift,
        file_asset_id: None,
        file_version_id: None,
        file_location_id: None,
        media_snapshot_id: None,
        content_hash: Some("blake3:before".to_owned()),
        size_bytes: Some(789),
        probe_worker_id: Some(voom_core::WorkerId(45)),
        bundle_id: None,
        bundle_member_role: None,
        sidecars: Vec::new(),
        error: Some(ScanFileErrorReport {
            code: ErrorCode::ArtifactChecksumMismatch,
            failure_class: FailureClass::ArtifactChecksumMismatch,
            message: "file changed between hashing and probing".to_owned(),
        }),
    });

    assert_eq!(
        serde_json::to_value(file).unwrap(),
        json!({
            "path": "/library/drift.mkv",
            "status": "failed_content_drift",
            "content_hash": "blake3:before",
            "size_bytes": 789,
            "probe_worker_id": 45,
            "error": {
                "code": "ARTIFACT_CHECKSUM_MISMATCH",
                "failure_class": "artifact_checksum_mismatch",
                "message": "file changed between hashing and probing"
            }
        })
    );
}

#[test]
fn failure_class_wire_uses_serde_spelling() {
    assert_eq!(
        failure_class_wire(FailureClass::MalformedWorkerResult),
        "malformed_worker_result"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_path_serializes_losslessly_as_os_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let file = ScanFileData::from(ScanFileReport {
        path: std::path::PathBuf::from(OsString::from_vec(b"/library/bad-\xff.mkv".to_vec())),
        status: ScanReportFileStatus::Failed,
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
    });

    assert_eq!(
        serde_json::to_value(file).unwrap()["path"],
        "os_bytes_hex:2f6c6962726172792f6261642dff2e6d6b76"
    );
}

fn report_fixture() -> ScanReport {
    ScanReport {
        path: "/library".into(),
        mode: ScanMode::Directory,
        summary: ScanSummary {
            discovered: 3,
            ingested: 1,
            probed: 1,
            snapshots_recorded: 1,
            skipped: 1,
            failed: 1,
        },
        files: vec![
            ScanFileReport {
                path: "/library/good.mkv".into(),
                status: ScanReportFileStatus::Scanned,
                file_asset_id: Some(FileAssetId(10)),
                file_version_id: Some(FileVersionId(11)),
                file_location_id: Some(FileLocationId(12)),
                media_snapshot_id: Some(voom_core::MediaSnapshotId(13)),
                content_hash: Some("blake3:good".to_owned()),
                size_bytes: Some(123),
                probe_worker_id: Some(voom_core::WorkerId(44)),
                bundle_id: Some(BundleId(20)),
                bundle_member_role: Some("primary_video".to_owned()),
                sidecars: vec![ScanSidecarReport {
                    path: "/library/good.eng.srt".into(),
                    file_asset_id: FileAssetId(30),
                    file_version_id: FileVersionId(31),
                    file_location_id: FileLocationId(32),
                    bundle_id: BundleId(20),
                    bundle_member_role: "external_subtitle".to_owned(),
                    content_hash: "sha256:sidecar".to_owned(),
                    size_bytes: 45,
                }],
                error: None,
            },
            ScanFileReport {
                path: "/library/bad.mkv".into(),
                status: ScanReportFileStatus::Failed,
                file_asset_id: None,
                file_version_id: None,
                file_location_id: None,
                media_snapshot_id: None,
                content_hash: Some("blake3:bad".to_owned()),
                size_bytes: Some(456),
                probe_worker_id: Some(voom_core::WorkerId(44)),
                bundle_id: None,
                bundle_member_role: None,
                sidecars: Vec::new(),
                error: Some(ScanFileErrorReport {
                    code: ErrorCode::ExternalSystemUnavailable,
                    failure_class: FailureClass::ExternalSystemUnavailable,
                    message: "ffprobe unavailable".to_owned(),
                }),
            },
        ],
        skipped: vec![ScanFileReport {
            path: "/library/readme.txt".into(),
            status: ScanReportFileStatus::SkippedUnsupportedExtension,
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
        }],
    }
}
