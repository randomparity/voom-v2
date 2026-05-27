use super::*;

use voom_core::ids::{ArtifactCommitRecordId, BundleId};

#[test]
fn extract_commit_recovery_required_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(9),
        result_file_version_id: FileVersionId(0),
        result_file_location_id: FileLocationId(0),
        state: ArtifactCommitState::RecoveryRequired,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: Some(commit::AudioExtractRecoveryReport {
            recovery_reason: "audio sidecar commit failed after durable prepare".to_owned(),
            commit_record_id: ArtifactCommitRecordId(9),
            source_bundle_id: BundleId(7),
            role: "commentary_audio",
            target_path: PathBuf::from("/tmp/target.ogg"),
            target_exists: true,
            temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
            temp_exists: false,
            staging_path: PathBuf::from("/tmp/staged.ogg"),
            staging_exists: true,
            result_file_version_id: None,
            result_file_location_id: None,
            error_code: "CONFLICT",
            message: "bundle membership conflict".to_owned(),
        }),
    };

    let err = ensure_extract_commit_succeeded(&report).unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::CommitFailure);
    assert!(err.to_string().contains("requires recovery"));
    assert!(err.to_string().contains("bundle membership conflict"));
}

#[test]
fn extract_commit_non_committed_state_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(10),
        result_file_version_id: FileVersionId(1),
        result_file_location_id: FileLocationId(2),
        state: ArtifactCommitState::Pending,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: None,
    };

    let err = ensure_extract_commit_succeeded(&report).unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::CommitFailure);
    assert!(err.to_string().contains("ended in Pending"));
}
