use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use serde_json::json;
use voom_control_plane::artifact::{
    ArtifactDetail, ArtifactInspectionState, ArtifactSummary, CommitArtifactReport,
    CommitRecoveryReport, CommitSummary, PathFacts, PathObservation, RecoverySummary,
    VerificationSummary, VerifyArtifactReport,
};
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, ErrorCode, FailureClass, FileLocationId, FileVersionId,
    WorkerId,
};
use voom_store::repo::media::artifacts::{ArtifactCommitState, ArtifactVerificationStatus};

use super::{
    ArtifactDetailData, ArtifactEnvelopeData, ArtifactSummaryData, CommitArtifactData,
    VerifyArtifactData, artifact_state_to_control_plane, command_error_code, path_wire,
};
use crate::cli::{ArtifactCommand, ArtifactStateArg, Cli, Command};
use crate::envelope::{Envelope, Status};

#[test]
fn artifact_command_names_and_flags_parse() {
    for command in [
        [
            "voom",
            "artifact",
            "verify",
            "--artifact-handle-id",
            "12",
            "--staging-root",
            "/tmp/stage",
        ]
        .as_slice(),
        [
            "voom",
            "artifact",
            "commit",
            "--artifact-handle-id",
            "12",
            "--target-path",
            "/tmp/target.bin",
        ]
        .as_slice(),
        [
            "voom",
            "artifact",
            "recover-commit",
            "--artifact-handle-id",
            "12",
        ]
        .as_slice(),
        [
            "voom", "artifact", "list", "--state", "verified", "--limit", "5",
        ]
        .as_slice(),
        ["voom", "artifact", "show", "--artifact-handle-id", "12"].as_slice(),
    ] {
        Cli::try_parse_from(command).unwrap();
    }
}

#[test]
fn artifact_state_enum_parses_snake_case_values() {
    for (raw, expected) in [
        ("staged", ArtifactStateArg::Staged),
        ("verified", ArtifactStateArg::Verified),
        ("committed", ArtifactStateArg::Committed),
        ("failed", ArtifactStateArg::Failed),
        ("recovery_required", ArtifactStateArg::RecoveryRequired),
    ] {
        let cli = Cli::try_parse_from(["voom", "artifact", "list", "--state", raw]).unwrap();
        let state = parsed_artifact_list_state(&cli.command);
        assert_eq!(state, Some(expected));
        assert_eq!(
            artifact_state_to_control_plane(expected),
            match expected {
                ArtifactStateArg::Staged => ArtifactInspectionState::Staged,
                ArtifactStateArg::Verified => ArtifactInspectionState::Verified,
                ArtifactStateArg::Committed => ArtifactInspectionState::Committed,
                ArtifactStateArg::Failed => ArtifactInspectionState::Failed,
                ArtifactStateArg::RecoveryRequired => ArtifactInspectionState::RecoveryRequired,
            }
        );
    }
}

#[test]
fn artifact_list_defaults_to_reasonable_limit() {
    let cli = Cli::try_parse_from(["voom", "artifact", "list"]).unwrap();
    assert_eq!(parsed_artifact_list_limit(&cli.command), Some(100));
}

#[test]
fn artifact_summary_serializes_ids_paths_and_snake_case_states() {
    let summary = ArtifactSummaryData::from(summary_fixture());

    assert_eq!(
        serde_json::to_value(summary).unwrap(),
        json!({
            "artifact_handle_id": 10,
            "state": "verified",
            "source_file_version_id": 20,
            "staging_path": "/tmp/staged.bin",
            "size_bytes": 123,
            "checksum": "blake3:expected",
            "latest_verification": {
                "id": 30,
                "artifact_location_id": 40,
                "path": "/tmp/staged.bin",
                "worker_id": 50,
                "status": "succeeded",
                "expected_size_bytes": 123,
                "expected_checksum": "blake3:expected",
                "observed_size_bytes": 123,
                "observed_checksum": "blake3:expected"
            }
        })
    );
}

#[test]
fn artifact_detail_serializes_top_level_and_verification_fields() {
    let value = serde_json::to_value(ArtifactDetailData::from(detail_fixture())).unwrap();

    assert_eq!(value["artifact_handle_id"], json!(10));
    assert_eq!(value["state"], "recovery_required");
    assert_eq!(value["source_file_version_id"], json!(20));
    assert_eq!(value["staging_path"], "/tmp/staged.bin");
    assert_eq!(value["target_path"], "/tmp/target.bin");
    assert_eq!(value["size_bytes"], json!(123));
    assert_eq!(value["checksum"], "blake3:expected");
    assert_eq!(
        value["verifications"][0],
        json!({
            "id": 30,
            "artifact_location_id": 40,
            "path": "/tmp/staged.bin",
            "worker_id": 50,
            "status": "failed",
            "expected_size_bytes": 123,
            "expected_checksum": "blake3:expected",
            "error_code": "ARTIFACT_CHECKSUM_MISMATCH",
            "message": "checksum drift"
        })
    );
    assert_eq!(value["latest_verification"], value["verifications"][0]);
}

#[test]
fn artifact_detail_serializes_commit_recovery_fields() {
    let value = serde_json::to_value(ArtifactDetailData::from(detail_fixture())).unwrap();

    assert_eq!(value["commits"][0], expected_commit_summary_json());
    assert_eq!(value["latest_commit"], expected_commit_summary_json());
}

#[test]
fn operation_reports_serialize_stable_ids_and_paths() {
    assert_eq!(
        serde_json::to_value(VerifyArtifactData::from(verify_report_fixture())).unwrap(),
        json!({
            "artifact_handle_id": 10,
            "artifact_location_id": 40,
            "verification_id": 30,
            "worker_id": 50,
            "status": "succeeded",
            "path": "/tmp/staged.bin",
            "expected_size_bytes": 123,
            "expected_checksum": "blake3:expected",
            "observed_size_bytes": 123,
            "observed_checksum": "blake3:expected"
        })
    );
    assert_eq!(
        serde_json::to_value(CommitArtifactData::from(commit_report_fixture())).unwrap(),
        json!({
            "commit_record_id": 60,
            "artifact_handle_id": 10,
            "verification_id": 30,
            "target_path": "/tmp/target.bin",
            "temp_path": "/tmp/target.bin.voom.tmp",
            "state": "recovery_required",
            "result_file_version_id": 70,
            "result_file_location_id": 80,
            "recovery_required": {
                "recovery_reason": "temp_installed",
                "target_path": "/tmp/target.bin",
                "target_exists": true,
                "temp_path": "/tmp/target.bin.voom.tmp",
                "temp_exists": false,
                "staging_path": "/tmp/staged.bin",
                "staging_exists": true,
                "result_file_version_id": 70,
                "result_file_location_id": 80
            }
        })
    );
}

#[test]
fn error_code_mapping_preserves_command_error_codes() {
    assert_eq!(
        command_error_code(ErrorCode::ConfigInvalid),
        "CONFIG_INVALID"
    );
    assert_eq!(command_error_code(ErrorCode::NotFound), "NOT_FOUND");
    assert_eq!(
        command_error_code(ErrorCode::ArtifactChecksumMismatch),
        "ARTIFACT_CHECKSUM_MISMATCH"
    );
}

#[test]
fn each_artifact_command_shape_has_exactly_one_json_envelope() {
    let commands = [
        "artifact.verify",
        "artifact.commit",
        "artifact.list",
        "artifact.show",
    ];

    for command in commands {
        let envelope = Envelope {
            schema_version: crate::envelope::SCHEMA_VERSION,
            command,
            status: Status::Ok,
            data: Some(ArtifactEnvelopeData {
                artifact: ArtifactSummaryData::from(summary_fixture()),
            }),
            next_cursor: None,
            local: None,
            warnings: Vec::new(),
            error: None,
        };
        let rendered = serde_json::to_string(&envelope).unwrap();
        let value = serde_json::from_str::<serde_json::Value>(&rendered).unwrap();

        assert_eq!(rendered.lines().count(), 1);
        assert_eq!(value["schema_version"], crate::envelope::SCHEMA_VERSION);
        assert_eq!(value["status"], "ok");
        assert!(value["data"].is_object());
        assert_eq!(value["command"], command);
    }
}

#[test]
fn clap_help_lists_artifact_subcommands() {
    let mut command = Cli::command();
    let artifact = command.find_subcommand_mut("artifact").unwrap();

    assert!(artifact.find_subcommand_mut("verify").is_some());
    assert!(artifact.find_subcommand_mut("commit").is_some());
    assert!(artifact.find_subcommand_mut("list").is_some());
    assert!(artifact.find_subcommand_mut("show").is_some());
}

#[test]
fn path_wire_serializes_utf8_path_as_string() {
    assert_eq!(
        path_wire(&PathBuf::from("/tmp/staged.bin")),
        "/tmp/staged.bin"
    );
}

fn parsed_artifact_list_state(command: &Command) -> Option<ArtifactStateArg> {
    match command {
        Command::Artifact(ArtifactCommand::List { state, .. }) => *state,
        _ => None,
    }
}

fn parsed_artifact_list_limit(command: &Command) -> Option<u32> {
    match command {
        Command::Artifact(ArtifactCommand::List { limit, .. }) => Some(*limit),
        _ => None,
    }
}

fn expected_commit_summary_json() -> serde_json::Value {
    json!({
        "id": 60,
        "verification_id": 30,
        "target_path": "/tmp/target.bin",
        "temp_path": "/tmp/target.bin.voom.tmp",
        "state": "recovery_required",
        "result_file_version_id": 70,
        "result_file_location_id": 80,
        "failure_class": "commit_failure",
        "error_code": "DB_UNREACHABLE",
        "message": "finalize failed",
        "recovery_reason": "temp_installed",
        "recovery": {
            "reason": "temp_installed",
            "target": {
                "path": "/tmp/target.bin",
                "exists": true,
                "facts": {
                    "path": "/tmp/target.bin",
                    "size_bytes": 123,
                    "checksum": "blake3:expected",
                    "local_file_key": "1:2"
                }
            },
            "temp": {
                "path": "/tmp/target.bin.voom.tmp",
                "exists": false
            },
            "staging": {
                "path": "/tmp/staged.bin",
                "exists": true,
                "facts": {
                    "path": "/tmp/staged.bin",
                    "size_bytes": 123,
                    "checksum": "blake3:expected"
                }
            }
        }
    })
}

fn summary_fixture() -> ArtifactSummary {
    ArtifactSummary {
        artifact_handle_id: ArtifactHandleId(10),
        state: ArtifactInspectionState::Verified,
        source_file_version_id: Some(FileVersionId(20)),
        staging_path: Some(PathBuf::from("/tmp/staged.bin")),
        target_path: None,
        size_bytes: Some(123),
        checksum: Some("blake3:expected".to_owned()),
        latest_verification: Some(success_verification_fixture()),
        latest_commit: None,
    }
}

fn detail_fixture() -> ArtifactDetail {
    ArtifactDetail {
        artifact_handle_id: ArtifactHandleId(10),
        state: ArtifactInspectionState::RecoveryRequired,
        source_file_version_id: Some(FileVersionId(20)),
        staging_path: Some(PathBuf::from("/tmp/staged.bin")),
        target_path: Some(PathBuf::from("/tmp/target.bin")),
        size_bytes: Some(123),
        checksum: Some("blake3:expected".to_owned()),
        verifications: vec![failed_verification_fixture()],
        commits: vec![commit_summary_fixture()],
        latest_verification: Some(failed_verification_fixture()),
        latest_commit: Some(commit_summary_fixture()),
    }
}

fn success_verification_fixture() -> VerificationSummary {
    VerificationSummary {
        id: ArtifactVerificationId(30),
        artifact_location_id: ArtifactLocationId(40),
        path: PathBuf::from("/tmp/staged.bin"),
        worker_id: WorkerId(50),
        status: ArtifactVerificationStatus::Succeeded,
        expected_size_bytes: 123,
        expected_checksum: "blake3:expected".to_owned(),
        observed_size_bytes: Some(123),
        observed_checksum: Some("blake3:expected".to_owned()),
        failure_class: None,
        error_code: None,
        message: None,
    }
}

fn failed_verification_fixture() -> VerificationSummary {
    VerificationSummary {
        status: ArtifactVerificationStatus::Failed,
        observed_size_bytes: None,
        observed_checksum: None,
        failure_class: None,
        error_code: Some(ErrorCode::ArtifactChecksumMismatch),
        message: Some("checksum drift".to_owned()),
        ..success_verification_fixture()
    }
}

fn commit_summary_fixture() -> CommitSummary {
    CommitSummary {
        id: ArtifactCommitRecordId(60),
        verification_id: ArtifactVerificationId(30),
        target_path: PathBuf::from("/tmp/target.bin"),
        temp_path: Some(PathBuf::from("/tmp/target.bin.voom.tmp")),
        state: ArtifactCommitState::RecoveryRequired,
        result_file_version_id: Some(FileVersionId(70)),
        result_file_location_id: Some(FileLocationId(80)),
        failure_class: Some(FailureClass::CommitFailure),
        error_code: Some(ErrorCode::DbUnreachable),
        message: Some("finalize failed".to_owned()),
        recovery_reason: Some("temp_installed".to_owned()),
        recovery: Some(RecoverySummary {
            reason: Some("temp_installed".to_owned()),
            target: PathObservation {
                path: PathBuf::from("/tmp/target.bin"),
                exists: true,
                facts: Some(PathFacts {
                    path: PathBuf::from("/tmp/target.bin"),
                    size_bytes: 123,
                    checksum: "blake3:expected".to_owned(),
                    local_file_key: Some("1:2".to_owned()),
                }),
                error: None,
            },
            temp: Some(PathObservation {
                path: PathBuf::from("/tmp/target.bin.voom.tmp"),
                exists: false,
                facts: None,
                error: None,
            }),
            staging: Some(PathObservation {
                path: PathBuf::from("/tmp/staged.bin"),
                exists: true,
                facts: Some(PathFacts {
                    path: PathBuf::from("/tmp/staged.bin"),
                    size_bytes: 123,
                    checksum: "blake3:expected".to_owned(),
                    local_file_key: None,
                }),
                error: None,
            }),
        }),
    }
}

fn verify_report_fixture() -> VerifyArtifactReport {
    VerifyArtifactReport {
        artifact_handle_id: ArtifactHandleId(10),
        artifact_location_id: ArtifactLocationId(40),
        verification_id: ArtifactVerificationId(30),
        worker_id: WorkerId(50),
        status: ArtifactVerificationStatus::Succeeded,
        path: PathBuf::from("/tmp/staged.bin"),
        expected_size_bytes: 123,
        expected_checksum: "blake3:expected".to_owned(),
        observed_size_bytes: Some(123),
        observed_checksum: Some("blake3:expected".to_owned()),
        error_code: None,
        message: None,
    }
}

fn commit_report_fixture() -> CommitArtifactReport {
    CommitArtifactReport {
        commit_record_id: ArtifactCommitRecordId(60),
        artifact_handle_id: ArtifactHandleId(10),
        verification_id: ArtifactVerificationId(30),
        target_path: PathBuf::from("/tmp/target.bin"),
        temp_path: Some(PathBuf::from("/tmp/target.bin.voom.tmp")),
        state: ArtifactCommitState::RecoveryRequired,
        result_file_version_id: Some(FileVersionId(70)),
        result_file_location_id: Some(FileLocationId(80)),
        recovery_required: Some(CommitRecoveryReport {
            recovery_reason: "temp_installed".to_owned(),
            target_path: PathBuf::from("/tmp/target.bin"),
            target_exists: true,
            temp_path: Some(PathBuf::from("/tmp/target.bin.voom.tmp")),
            temp_exists: false,
            staging_path: PathBuf::from("/tmp/staged.bin"),
            staging_exists: true,
            result_file_version_id: Some(FileVersionId(70)),
            result_file_location_id: Some(FileLocationId(80)),
        }),
    }
}
