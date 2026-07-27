use super::*;

use async_trait::async_trait;
use sqlx::Row;
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitRecordId, BundleId};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{JobId, LeaseId, TicketId};
use voom_store::repo::bundles::NewAssetBundle;
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome};
use voom_store::repo::identity::{MediaWorkKind, NewMediaVariant, NewMediaWork};
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, ExtractAudioOutputResult, ExtractAudioRequest,
    ExtractAudioResult, TranscodeAudioRequest, TranscodeAudioResult, VerifyArtifactObservedFacts,
    VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

#[test]
fn extract_commit_recovery_without_target_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(9),
        result_file_version_id: None,
        result_file_location_id: None,
        state: ArtifactCommitState::RecoveryRequired,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: Some(commit::AudioExtractRecoveryReport {
            recovery_reason: "audio sidecar commit failed after durable prepare".to_owned(),
            commit_record_id: ArtifactCommitRecordId(9),
            source_bundle_id: BundleId(7),
            role: "commentary_audio",
            target_path: PathBuf::from("/tmp/target.ogg"),
            target_exists: false,
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
        result_file_version_id: None,
        result_file_location_id: None,
        state: ArtifactCommitState::Pending,
        target_path: PathBuf::from("/tmp/target.ogg"),
        temp_path: PathBuf::from("/tmp/.target.ogg.tmp"),
        recovery_required: None,
    };

    let err = ensure_extract_commit_succeeded(&report).unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::CommitFailure);
    assert!(err.to_string().contains("ended in Pending"));
}

#[tokio::test]
async fn transcode_failure_records_audio_failed_event() {
    let (cp, _db) = fixture().await;
    let input = transcode_input();

    let err = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &UncalledTranscodeDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::NotFound);
    assert_event_count(&cp, "artifact.audio_transcode_failed", 1).await;
}

#[tokio::test]
async fn late_transcode_failure_event_keeps_attempt_context_and_worker_result() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let input = transcode_input_for_source(&source, &dir);

    let err = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingTranscodeDispatcher {
            output_bytes: b"transcoded".to_vec(),
        },
        &MismatchedVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::VerificationFailure);
    let payload = latest_event_payload(&cp, "artifact.audio_transcode_failed").await;
    assert_eq!(payload["source_file_version_id"], source.version.0);
    assert_eq!(payload["source_file_location_id"], source.location.0);
    assert_eq!(payload["source_media_snapshot_id"], source.snapshot);
    assert!(payload["artifact_handle_id"].as_u64().is_some());
    assert!(payload["artifact_location_id"].as_u64().is_some());
    assert!(
        payload["staging_path"]
            .as_str()
            .unwrap()
            .contains("voom-audio-stage")
    );
    assert_eq!(payload["selected_streams"][0]["snapshot_stream_id"], "a-1");
    assert_eq!(
        payload["selected_output_streams"][0]["output_provider_stream_index"],
        0
    );
    assert_eq!(payload["provider"], "ffmpeg");
    assert_eq!(payload["provider_version"], "test");
}

#[tokio::test]
async fn extract_failure_records_audio_failed_event() {
    let (cp, _db) = fixture().await;
    let input = extract_input();

    let err = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::NotFound);
    assert_event_count(&cp, "artifact.audio_extract_failed", 1).await;
}

#[tokio::test]
async fn extract_audio_plural_commits_every_output_and_lineage_atomically() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let operation_id = "node_extract_audio_plural";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);
    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.outputs.len(), 2);
    assert!(
        report.outputs[0]
            .target_path
            .ends_with("source.a-1.opus.ogg")
    );
    assert!(
        report.outputs[1]
            .target_path
            .ends_with("source.a-2.opus.ogg")
    );
    assert!(
        report
            .outputs
            .iter()
            .all(|output| output.target_path.is_file())
    );
    assert_table_count(&cp, "artifact_handles", 2).await;
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "file_versions", 3).await;
    assert_table_count(&cp, "asset_bundle_members", 2).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 2).await;
    assert_event_count(&cp, "artifact.audio_extract_succeeded", 1).await;
    let event = latest_event_payload(&cp, "artifact.audio_extract_succeeded").await;
    assert_eq!(event["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(
        event["outputs"][0]["output_id"],
        voom_plan::audio::extract_output_id(operation_id, "a-1")
    );
    assert_eq!(event["outputs"][1]["source_snapshot_stream_id"], "a-2");
    assert_eq!(
        event["outputs"][1]["result_file_location_id"],
        report.outputs[1].result_file_location_id.0
    );
    assert_eq!(
        event["outputs"][1]["bundle_member_id"],
        report.outputs[1].bundle_member_id
    );
}

#[tokio::test]
async fn committed_plural_extract_retry_returns_same_ordered_identities_without_dispatch() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let operation_id = "node_extract_audio_retry";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);

    let first = execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
    )
    .await
    .unwrap();
    let retry = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(retry.outputs, first.outputs);
    assert_table_count(&cp, "artifact_handles", 2).await;
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "file_versions", 3).await;
    assert_table_count(&cp, "asset_bundle_members", 2).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 2).await;
}

#[tokio::test]
async fn plural_extract_recovers_after_promotions_without_duplicate_rows() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let operation_id = "node_extract_audio_recovery";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);
    sqlx::query(
        "CREATE TRIGGER fail_audio_extract_finalize BEFORE INSERT ON file_assets \
         BEGIN SELECT RAISE(ABORT, 'injected finalize failure'); END;",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::DbUnreachable);
    assert!(input.target_dir.join("source.a-1.opus.ogg").is_file());
    assert!(input.target_dir.join("source.a-2.opus.ogg").is_file());
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "file_versions", 1).await;
    assert_table_count(&cp, "asset_bundle_members", 0).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 0).await;
    sqlx::query("DROP TRIGGER fail_audio_extract_finalize")
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let recovered = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(recovered.outputs.len(), 2);
    assert!(
        recovered
            .outputs
            .iter()
            .all(|output| output.target_path.is_file())
    );
    assert_table_count(&cp, "artifact_handles", 2).await;
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "file_versions", 3).await;
    assert_table_count(&cp, "asset_bundle_members", 2).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 2).await;
}

#[tokio::test]
async fn extract_audio_malformed_result_list_stops_before_verifier_and_commit() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);
    let target_path = input.target_dir.join("source.a-1.opus.ogg");

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &MissingOutputsExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &UncalledVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
    assert!(error.to_string().contains("missing the outputs list"));
    assert!(!target_path.exists());
    assert_table_count(&cp, "artifact_handles", 0).await;
    assert_table_count(&cp, "artifact_commit_records", 0).await;
    assert_table_count(&cp, "file_versions", 1).await;
    assert_event_count(&cp, "artifact.audio_extract_succeeded", 0).await;
}

#[tokio::test]
async fn extract_audio_inconsistent_or_extra_results_stop_before_verifier_and_commit() {
    for mutation in [
        ExtractResultMutation::ProjectionMismatch,
        ExtractResultMutation::ExtraOutput,
    ] {
        let (cp, _db, dir) = fixture_with_dir().await;
        let source = seed_audio_source(&cp, &dir, b"source").await;
        let bundle = seed_bundle(&cp).await;
        let input = extract_input_for_source(&source, bundle.id, &dir);
        let target_path = input.target_dir.join("source.a-1.opus.ogg");

        let error = execute_extract_audio_with_dispatchers(
            &cp,
            input,
            &MutatingOutputsExtractDispatcher {
                output_bytes: b"extracted".to_vec(),
                mutation,
            },
            &UncalledVerifyDispatcher,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.error_code(),
            voom_core::ErrorCode::MalformedWorkerResult
        );
        assert!(!target_path.exists());
        assert_table_count(&cp, "artifact_handles", 0).await;
        assert_table_count(&cp, "artifact_commit_records", 0).await;
        assert_table_count(&cp, "file_versions", 1).await;
        assert_event_count(&cp, "artifact.audio_extract_succeeded", 0).await;
    }
}

#[tokio::test]
async fn extract_audio_legacy_singleton_remains_executable() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let payload = input.operation_payload.as_object_mut().unwrap();
    payload.remove("operation_id");
    payload.remove("outputs");

    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
    )
    .await
    .unwrap();

    assert!(report.target_path.is_file());
    assert!(report.commit_record_id.0 > 0);
    assert!(report.result_file_version_id.0 > source.version.0);
    assert_event_count(&cp, "artifact.audio_extract_succeeded", 1).await;
}

#[test]
fn committed_extract_recovery_with_target_is_not_reported_as_success() {
    let report = commit::CommitAudioExtractSidecarReport {
        commit_record_id: ArtifactCommitRecordId(9),
        result_file_version_id: None,
        result_file_location_id: None,
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
}

#[tokio::test]
async fn staged_result_probe_failure_does_not_commit() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let input = transcode_input_for_source(&source, &dir);

    let err = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingTranscodeDispatcher {
            output_bytes: b"transcoded".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &FailingProbeDispatcher,
    )
    .await
    .unwrap_err();

    // The probe now runs before commit, so a probe failure leaves nothing
    // committed and the caller records exactly one failed event.
    assert_eq!(err.error_code(), voom_core::ErrorCode::Internal);
    assert_event_count(&cp, "artifact.audio_transcode_failed", 1).await;
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 0).await;
    assert_event_count(&cp, "artifact.commit_completed", 0).await;
    let snapshots = cp
        .identity
        .list_media_snapshots_by_version(source.version)
        .await
        .unwrap();
    // Only the seeded source snapshot exists; no result snapshot was recorded.
    assert_eq!(snapshots.len(), 1);
}

#[tokio::test]
async fn transcode_post_commit_snapshot_write_failure_returns_recovery_report() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let input = transcode_input_for_source(&source, &dir);
    // The staged probe succeeds; the post-commit media_snapshots insert aborts,
    // so the only post-commit failure is the local DB write — the path the
    // recovery report still covers. The source snapshot was already written by
    // seed_audio_source above, so this trigger only catches the result snapshot.
    sqlx::query(
        "CREATE TRIGGER fail_audio_transcode_result_snapshot \
         BEFORE INSERT ON media_snapshots \
         BEGIN SELECT RAISE(ABORT, 'snapshot write unavailable'); END;",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingTranscodeDispatcher {
            output_bytes: b"transcoded".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();

    assert!(report.commit_record_id.0 > 0);
    assert!(report.result_file_version_id.0 > 0);
    assert!(report.result_file_location_id.0 > 0);
    assert_eq!(report.result_media_snapshot_id.0, 0);
    let recovery = report.commit_recovery_required.unwrap();
    assert_eq!(recovery.commit_record_id, report.commit_record_id);
    assert_eq!(
        recovery.result_file_version_id,
        report.result_file_version_id
    );
    assert_eq!(
        recovery.result_file_location_id,
        report.result_file_location_id
    );
    assert_eq!(recovery.result_media_snapshot_id, None);
    assert_event_count(&cp, "artifact.audio_transcode_failed", 0).await;
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 0).await;
}

#[tokio::test]
async fn test_extract_post_commit_succeeded_event_failure_returns_ok_with_context() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);

    sqlx::query(
        "CREATE TRIGGER fail_extract_succeeded BEFORE INSERT ON events \
         WHEN NEW.kind = 'artifact.audio_extract_succeeded' \
         BEGIN SELECT RAISE(ABORT, 'injected post-commit event failure'); END;",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
    )
    .await
    .unwrap();

    assert!(report.commit_record_id.0 > 0);
    assert!(report.result_file_version_id.0 > 0);
    assert!(report.result_file_location_id.0 > 0);
    let recovery = report.commit_recovery_required.unwrap();
    assert_eq!(recovery.commit_record_id, report.commit_record_id);
    assert_eq!(
        recovery.result_file_version_id,
        Some(report.result_file_version_id)
    );
    assert_eq!(
        recovery.result_file_location_id,
        Some(report.result_file_location_id)
    );
    assert_eq!(
        recovery.recovery_reason,
        "audio extract post-commit reporting failed"
    );
    assert!(recovery.target_exists);
    assert_event_count(&cp, "artifact.audio_extract_failed", 0).await;
    assert_event_count(&cp, "artifact.audio_extract_succeeded", 0).await;
    assert_event_count(&cp, "artifact.commit_completed", 1).await;
}

async fn fixture() -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(1))),
    )
    .await
    .unwrap();
    (cp, db)
}

async fn fixture_with_dir() -> (
    crate::ControlPlane,
    tempfile::NamedTempFile,
    tempfile::TempDir,
) {
    let (cp, db) = fixture().await;
    (
        cp,
        db,
        tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap(),
    )
}

#[derive(Debug, Clone, Copy)]
struct SeededAudioSource {
    version: FileVersionId,
    location: FileLocationId,
    snapshot: u64,
}

async fn seed_audio_source(
    cp: &crate::ControlPlane,
    dir: &tempfile::TempDir,
    bytes: &[u8],
) -> SeededAudioSource {
    let source_path = dir.path().join("source.mkv");
    std::fs::write(&source_path, bytes).unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: source_path.display().to_string(),
                content_hash: blake3_checksum(bytes),
                size_bytes: u64::try_from(bytes.len()).unwrap(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id,
        file_location_id,
        ..
    } = outcome
    else {
        panic!("seed_audio_source should create a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({
                "container": "mkv",
                "streams": [
                    {
                        "id": "v-1",
                        "index": 0,
                        "kind": "video",
                        "codec_name": "h264"
                    },
                    {
                        "id": "a-1",
                        "index": 1,
                        "kind": "audio",
                        "codec_name": "aac",
                        "language": "eng",
                        "title": "Main",
                        "channels": 2,
                        "disposition": {
                            "default": true,
                            "forced": false,
                            "commentary": false
                        }
                    }
                ]
            }),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    SeededAudioSource {
        version: file_version_id,
        location: file_location_id,
        snapshot: snapshot.id.0,
    }
}

async fn record_plural_audio_snapshot(
    cp: &crate::ControlPlane,
    file_version_id: FileVersionId,
) -> u64 {
    cp.record_media_snapshot(
        file_version_id,
        None,
        serde_json::json!({
            "container": "mkv",
            "streams": [
                {
                    "id": "v-1",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264"
                },
                {
                    "id": "a-1",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "aac",
                    "language": "eng",
                    "title": "Main",
                    "channels": 2,
                    "disposition": {
                        "default": true,
                        "forced": false,
                        "commentary": false
                    }
                },
                {
                    "id": "a-2",
                    "index": 2,
                    "kind": "audio",
                    "codec_name": "aac",
                    "language": "eng",
                    "title": "Commentary",
                    "channels": 2,
                    "disposition": {
                        "default": false,
                        "forced": false,
                        "commentary": true
                    }
                }
            ]
        }),
        OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap()
    .id
    .0
}

async fn seed_bundle(cp: &crate::ControlPlane) -> voom_store::repo::bundles::AssetBundle {
    let work = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "movie".to_owned(),
            provisional: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let variant = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: work.id,
            label: "main".to_owned(),
            provisional: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    cp.create_bundle(NewAssetBundle {
        media_variant_id: variant.id,
        display_name: "bundle".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
    })
    .await
    .unwrap()
}

fn transcode_input() -> ExecuteTranscodeAudioInput {
    ExecuteTranscodeAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: FileVersionId(999),
        source_location_id: None,
        operation_payload: serde_json::json!({
            "type": "transcode_audio",
            "target_codec": "aac",
            "container": "mkv",
            "source_media_snapshot_id": 888,
            "filter": null
        }),
        staging_root: PathBuf::from("/tmp/voom-audio-stage"),
        target_dir: PathBuf::from("/tmp/voom-audio-out"),
        backup_root: None,
    }
}

fn transcode_input_for_source(
    source: &SeededAudioSource,
    dir: &tempfile::TempDir,
) -> ExecuteTranscodeAudioInput {
    ExecuteTranscodeAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: source.version,
        source_location_id: Some(source.location),
        operation_payload: serde_json::json!({
            "type": "transcode_audio",
            "target_codec": "aac",
            "container": "mkv",
            "source_media_snapshot_id": source.snapshot,
            "filter": null
        }),
        staging_root: dir.path().join("voom-audio-stage"),
        target_dir: dir.path().join("voom-audio-out"),
        backup_root: None,
    }
}

fn extract_input() -> ExecuteExtractAudioInput {
    ExecuteExtractAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: FileVersionId(999),
        source_location_id: None,
        source_bundle_id: BundleId(777),
        operation_payload: serde_json::json!({
            "type": "extract_audio",
            "target_codec": "opus",
            "container": "ogg",
            "source_media_snapshot_id": 888,
            "filter": null
        }),
        staging_root: PathBuf::from("/tmp/voom-audio-stage"),
        target_dir: PathBuf::from("/tmp/voom-audio-out"),
        backup_root: None,
    }
}

fn extract_input_for_source(
    source: &SeededAudioSource,
    source_bundle_id: BundleId,
    dir: &tempfile::TempDir,
) -> ExecuteExtractAudioInput {
    let operation_id = "node_extract_audio_test";
    ExecuteExtractAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: source.version,
        source_location_id: Some(source.location),
        source_bundle_id,
        operation_payload: serde_json::json!({
            "type": "extract_audio",
            "operation_id": operation_id,
            "target_codec": "opus",
            "container": "ogg",
            "source_media_snapshot_id": source.snapshot,
            "snapshot_stream_id": "a-1",
            "filter": null,
            "outputs": [{
                "output_id": voom_plan::audio::extract_output_id(operation_id, "a-1"),
                "source_snapshot_stream_id": "a-1",
                "source_provider_stream_index": 1,
                "name_suffix": "a-1.opus.ogg",
                "bundle_role": "external_audio"
            }]
        }),
        staging_root: dir.path().join("voom-audio-stage"),
        target_dir: dir.path().join("voom-audio-out"),
        backup_root: None,
    }
}

async fn assert_event_count(cp: &crate::ControlPlane, kind: &str, expected: i64) {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM events WHERE kind = ?")
        .bind(kind)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let count: i64 = row.try_get("count").unwrap();
    assert_eq!(count, expected);
}

async fn assert_table_count(cp: &crate::ControlPlane, table: &str, expected: i64) {
    let query = format!("SELECT COUNT(*) AS count FROM {table}");
    let row = sqlx::query(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let count: i64 = row.try_get("count").unwrap();
    assert_eq!(count, expected, "unexpected row count for {table}");
}

async fn latest_event_payload(cp: &crate::ControlPlane, kind: &str) -> serde_json::Value {
    let row =
        sqlx::query("SELECT payload FROM events WHERE kind = ? ORDER BY event_id DESC LIMIT 1")
            .bind(kind)
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let payload: String = row.try_get("payload").unwrap();
    serde_json::from_str(&payload).unwrap()
}

struct UncalledTranscodeDispatcher;

#[async_trait]
impl TranscodeAudioDispatcher for UncalledTranscodeDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        _request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        panic!("transcode dispatcher should not be called")
    }
}

struct UncalledExtractDispatcher;

#[async_trait]
impl ExtractAudioDispatcher for UncalledExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        _request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        panic!("extract dispatcher should not be called")
    }
}

struct UncalledVerifyDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for UncalledVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        panic!("verify dispatcher should not be called")
    }
}

struct MismatchedVerifyDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for MismatchedVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "test-verify".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: 1,
                content_hash: "blake3:mismatch".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        })
    }
}

struct SuccessfulVerifyDispatcher;

#[async_trait]
impl VerifyArtifactDispatcher for SuccessfulVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "test-verify".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: request.expected.size_bytes,
                content_hash: request.expected.content_hash,
                modified_at: None,
                local_file_key: None,
            },
        })
    }
}

struct UncalledProbeDispatcher;

#[async_trait]
impl commit::AudioResultProbeDispatcher for UncalledProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        _cp: &crate::ControlPlane,
        _request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        panic!("probe dispatcher should not be called")
    }
}

struct FailingProbeDispatcher;

#[async_trait]
impl commit::AudioResultProbeDispatcher for FailingProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        _cp: &crate::ControlPlane,
        _request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        Err(VoomError::Internal(
            "simulated staged-probe failure".to_owned(),
        ))
    }
}

struct SucceedingProbeDispatcher;

#[async_trait]
impl commit::AudioResultProbeDispatcher for SucceedingProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &crate::ControlPlane,
        request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        let mut tx = cp.pool_for_test().begin().await.unwrap();
        let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Echo the expected facts so verify_probe_facts passes and the flow
        // reaches the post-commit snapshot write.
        let facts = voom_worker_protocol::ObservedFileFacts {
            size_bytes: request.expected.size_bytes,
            content_hash: request.expected.content_hash,
            modified_at: None,
            local_file_key: None,
        };
        Ok(commit::ProbedAudioResult {
            worker_id: worker.id,
            result: voom_worker_protocol::ProbeFileResult {
                status: voom_worker_protocol::ProbeFileStatus::Probed,
                provider: "ffprobe".to_owned(),
                provider_version: "test".to_owned(),
                pre_probe: facts.clone(),
                post_probe: facts,
                snapshot: serde_json::json!({
                    "format": "sprint10-v1",
                    "probe": { "provider": "ffprobe", "provider_version": "test" },
                    "container": { "format_name": "matroska,webm" },
                    "streams": [
                        { "index": 0, "kind": "audio", "codec_name": "opus" }
                    ]
                }),
            },
        })
    }
}

struct WritingTranscodeDispatcher {
    output_bytes: Vec<u8>,
}

#[async_trait]
impl TranscodeAudioDispatcher for WritingTranscodeDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        tokio::fs::write(&request.output.path, &self.output_bytes)
            .await
            .unwrap();
        let output_hash = blake3_checksum(&self.output_bytes);
        Ok(TranscodeAudioResult {
            status: voom_worker_protocol::TranscodeAudioStatus::Transcoded,
            provider: "ffmpeg".to_owned(),
            provider_version: "test".to_owned(),
            input_pre: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            input_post: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            output: observed(
                u64::try_from(self.output_bytes.len()).unwrap(),
                &output_hash,
            ),
            output_container: "mkv".to_owned(),
            selected_snapshot_stream_ids: vec!["a-1".to_owned()],
            output_audio_codecs: vec!["aac".to_owned()],
            selected_output_streams: vec![AudioOutputStreamFact {
                snapshot_stream_id: "a-1".to_owned(),
                output_provider_stream_index: 0,
                codec: "aac".to_owned(),
                language: Some("eng".to_owned()),
                title: Some("Main".to_owned()),
                default: Some(true),
                disposition: Some(voom_worker_protocol::AudioDispositionFact {
                    default: Some(true),
                    forced: Some(false),
                    commentary: Some(false),
                }),
                channels: Some(2),
            }],
        })
    }
}

struct WritingExtractDispatcher {
    output_bytes: Vec<u8>,
}

struct MissingOutputsExtractDispatcher {
    output_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum ExtractResultMutation {
    ProjectionMismatch,
    ExtraOutput,
}

struct MutatingOutputsExtractDispatcher {
    output_bytes: Vec<u8>,
    mutation: ExtractResultMutation,
}

#[async_trait]
impl ExtractAudioDispatcher for MutatingOutputsExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let mut result = WritingExtractDispatcher {
            output_bytes: self.output_bytes.clone(),
        }
        .dispatch_extract_audio(request)
        .await?;
        match self.mutation {
            ExtractResultMutation::ProjectionMismatch => {
                result.output_title = Some("Wrong projection".to_owned());
            }
            ExtractResultMutation::ExtraOutput => {
                let Some(outputs) = result.outputs.as_mut() else {
                    return Err(VoomError::Internal(
                        "planned singleton request did not produce an output list".to_owned(),
                    ));
                };
                let mut extra = outputs[0].clone();
                extra.output_id.push_str("_extra");
                extra.selection.snapshot_stream_id.push_str("-extra");
                extra.selection.provider_stream_index += 1;
                extra.path.push_str(".extra");
                outputs.push(extra);
            }
        }
        Ok(result)
    }
}

#[async_trait]
impl ExtractAudioDispatcher for MissingOutputsExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let mut result = WritingExtractDispatcher {
            output_bytes: self.output_bytes.clone(),
        }
        .dispatch_extract_audio(request)
        .await?;
        result.outputs = None;
        Ok(result)
    }
}

#[async_trait]
impl ExtractAudioDispatcher for WritingExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        if let Some(outputs) = &request.outputs {
            for descriptor in outputs {
                tokio::fs::write(&descriptor.output.path, &self.output_bytes)
                    .await
                    .unwrap();
            }
        } else {
            tokio::fs::write(&request.output.path, &self.output_bytes)
                .await
                .unwrap();
        }
        let output_hash = blake3_checksum(&self.output_bytes);
        let output = observed(
            u64::try_from(self.output_bytes.len()).unwrap(),
            &output_hash,
        );
        let outputs = request.outputs.as_ref().map(|descriptors| {
            descriptors
                .iter()
                .map(|descriptor| ExtractAudioOutputResult {
                    output_id: descriptor.output_id.clone(),
                    selection: descriptor.selection.clone(),
                    path: descriptor.output.path.clone(),
                    output: output.clone(),
                    output_container: "ogg".to_owned(),
                    output_audio_codec: "opus".to_owned(),
                    output_language: Some("eng".to_owned()),
                    output_title: Some(
                        if descriptor.selection.snapshot_stream_id == "a-2" {
                            "Commentary"
                        } else {
                            "Main"
                        }
                        .to_owned(),
                    ),
                })
                .collect()
        });
        Ok(ExtractAudioResult {
            status: voom_worker_protocol::ExtractAudioStatus::Extracted,
            provider: "ffmpeg".to_owned(),
            provider_version: "test".to_owned(),
            input_pre: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            input_post: observed(
                request.input.expected.size_bytes,
                &request.input.expected.content_hash,
            ),
            output,
            output_container: "ogg".to_owned(),
            output_audio_codec: "opus".to_owned(),
            selected_snapshot_stream_id: request.selection.snapshot_stream_id.clone(),
            output_language: Some("eng".to_owned()),
            output_title: Some("Main".to_owned()),
            outputs,
        })
    }
}

fn observed(size_bytes: u64, content_hash: &str) -> AudioObservedFacts {
    AudioObservedFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
