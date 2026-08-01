use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use sqlx::Row;
use time::OffsetDateTime;
use voom_core::ids::BundleId;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{JobId, LeaseId, TicketId};
use voom_store::repo::media::artifacts::{
    NewArtifactCommitRecord, NewArtifactHandle, NewArtifactLocation, NewSidecarArtifactCommit,
};
use voom_store::repo::media::bundles::{BundleMemberRole, NewAssetBundle, NewBundleMember};
use voom_store::repo::media::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::media::identity::{
    MediaSnapshotRepo, MediaWorkKind, NewMediaVariant, NewMediaWork,
};
use voom_store::repo::media::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind, UseLeaseReleaseReason,
};
use voom_worker_protocol::{
    AudioObservedFacts, AudioOutputStreamFact, ExtractAudioOutputResult, ExtractAudioRequest,
    ExtractAudioResult, TranscodeAudioRequest, TranscodeAudioResult, VerifyArtifactObservedFacts,
    VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

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
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::NotFound);
    assert_event_count(&cp, "artifact.audio_extract_failed", 1).await;
}

#[tokio::test]
async fn first_extract_plan_failure_rolls_back_bundle_operation_outputs_and_events() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    sqlx::query(
        "CREATE TRIGGER fail_first_extract_output \
         BEFORE INSERT ON audio_extract_operation_outputs \
         BEGIN SELECT RAISE(ABORT, 'injected first extract output failure'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let tables = [
        "media_works",
        "media_variants",
        "asset_bundles",
        "asset_bundle_members",
        "audio_extract_operations",
        "audio_extract_operation_outputs",
    ];
    let before = table_counts(&cp, &tables).await;

    let error = plan_first_extract_with_bundle(&cp, first_extract_plan_input(&source, &dir))
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::DbUnreachable);
    assert_eq!(table_counts(&cp, &tables).await, before);
    for kind in [
        "media_work.created",
        "media_variant.created",
        "asset_bundle.created",
        "asset_bundle.member_added",
    ] {
        assert_event_count(&cp, kind, 0).await;
    }
    assert!(
        directory_is_empty(
            &dir.path()
                .join("voom-audio-out")
                .join("operation-node_extract_audio_test")
        )
        .await
    );
}

#[tokio::test]
async fn concurrent_first_extract_plans_converge_without_duplicate_rows_or_events() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let input = first_extract_plan_input(&source, &dir);

    let (first, second) = tokio::join!(
        plan_first_extract_with_bundle(&cp, input.clone()),
        plan_first_extract_with_bundle(&cp, input.clone()),
    );

    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first, second);
    assert_eq!(
        table_counts(
            &cp,
            &[
                "media_works",
                "media_variants",
                "asset_bundles",
                "asset_bundle_members",
                "audio_extract_operations",
                "audio_extract_operation_outputs",
            ],
        )
        .await,
        [1, 1, 1, 1, 1, 1]
    );
    for kind in [
        "media_work.created",
        "media_variant.created",
        "asset_bundle.created",
        "asset_bundle.member_added",
    ] {
        assert_event_count(&cp, kind, 1).await;
    }

    let replay = plan_first_extract_with_bundle(&cp, input).await.unwrap();
    assert_eq!(replay, first);
    for kind in [
        "media_work.created",
        "media_variant.created",
        "asset_bundle.created",
        "asset_bundle.member_added",
    ] {
        assert_event_count(&cp, kind, 1).await;
    }
}

#[tokio::test]
async fn extract_failure_preserves_primary_error_when_cleanup_and_event_writes_fail() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);
    sqlx::query(
        "CREATE TRIGGER fail_extract_claim_release \
         BEFORE UPDATE OF claim_token ON audio_extract_operations \
         WHEN OLD.claim_token IS NOT NULL AND NEW.claim_token IS NULL \
         BEGIN SELECT RAISE(ABORT, 'injected claim release failure'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_extract_failure_event \
         BEFORE INSERT ON events WHEN NEW.kind = 'artifact.audio_extract_failed' \
         BEGIN SELECT RAISE(ABORT, 'injected failure event failure'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &CrashingExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::WorkerCrash);
    assert!(error.to_string().contains("injected worker crash"));
    assert_event_count(&cp, "artifact.audio_extract_failed", 0).await;
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
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);
    let verify = SessionTrackingVerifyDispatcher::default();
    let probe = SessionTrackingProbeDispatcher::default();
    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &verify,
        &probe,
    )
    .await
    .unwrap();

    assert_eq!(verify.sessions.load(Ordering::Relaxed), 1);
    assert_eq!(verify.dispatches.load(Ordering::Relaxed), 2);
    assert_eq!(verify.shutdowns.load(Ordering::Relaxed), 1);
    assert_eq!(probe.sessions.load(Ordering::Relaxed), 1);
    assert_eq!(probe.dispatches.load(Ordering::Relaxed), 2);
    assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 1);
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
    let started = latest_event_payload(&cp, "artifact.audio_extract_started").await;
    assert_eq!(started["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(started["outputs"][0]["source_snapshot_stream_id"], "a-1");
    assert_eq!(started["outputs"][1]["source_snapshot_stream_id"], "a-2");
    let event = latest_event_payload(&cp, "artifact.audio_extract_succeeded").await;
    assert_eq!(event["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(
        event["outputs"][0]["output_id"],
        voom_plan::planner::audio::extract_output_id(operation_id, "a-1")
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
async fn plural_extract_commit_uses_one_claim_assertion_per_boundary() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    set_plural_extract_outputs(&mut input, "claim_fence_count");
    let fences = RecordingClaimFences::default();

    execute_extract_audio_with_services(
        &cp,
        input,
        ExtractAudioExecutionServices {
            extract: &WritingExtractDispatcher {
                output_bytes: b"extracted".to_vec(),
            },
            verify: &SuccessfulVerifyDispatcher,
            result_probe: &SucceedingProbeDispatcher,
            claim_fence_hooks: &fences,
        },
    )
    .await
    .unwrap();

    assert_eq!(fences.boundaries(), vec![0, 1, 2]);
}

#[tokio::test]
async fn plural_extract_recovery_uses_one_claim_assertion_per_boundary() {
    let (cp, _db, _dir, input) = plural_extract_fixture("recovery_fence_count").await;
    sqlx::query(
        "CREATE TRIGGER fail_audio_extract_finalize BEFORE INSERT ON file_assets \
         BEGIN SELECT RAISE(ABORT, 'injected finalize failure'); END;",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap_err();
    sqlx::query("DROP TRIGGER fail_audio_extract_finalize")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let fences = RecordingClaimFences::default();

    execute_extract_audio_with_services(
        &cp,
        input,
        ExtractAudioExecutionServices {
            extract: &UncalledExtractDispatcher,
            verify: &UncalledVerifyDispatcher,
            result_probe: &UncalledProbeDispatcher,
            claim_fence_hooks: &fences,
        },
    )
    .await
    .unwrap();

    assert_eq!(fences.boundaries(), vec![0, 1, 2]);
}

#[tokio::test]
async fn fresh_commit_claim_loss_at_every_boundary_is_recoverable() {
    for mode in ClaimLossMode::ALL {
        for boundary_index in 0..=2 {
            assert_fresh_claim_loss(mode, boundary_index).await;
        }
    }
}

#[tokio::test]
async fn recovery_claim_loss_at_every_boundary_retains_evidence() {
    for mode in ClaimLossMode::ALL {
        for boundary_index in 0..=2 {
            assert_recovery_claim_loss(mode, boundary_index).await;
        }
    }
}

#[tokio::test]
async fn synthesis_commits_companion_lineage_once_and_replays_without_dispatch() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_surround_audio_snapshot(&cp, source.version).await;
    let input = synthesis_input_for_source(&source, &dir);

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingSynthesisDispatcher {
            output_bytes: b"synthesized".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SynthesisProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.synthesized_companions.len(), 1);
    let attempt_statuses = sqlx::query_scalar::<_, String>(
        "SELECT status FROM audio_synthesis_dispatch_attempts ORDER BY generation",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(attempt_statuses, vec!["terminal"]);
    let companion = &report.synthesized_companions[0];
    assert_eq!(companion.source_snapshot_stream_id, "a-1");
    assert_eq!(companion.source_provider_stream_index, 1);
    assert_eq!(companion.result_provider_stream_index, 2);
    assert_eq!(companion.channels, 2);
    assert_eq!(companion.codec, "aac");
    assert!(companion.lineage_id > 0);
    let result_snapshot = cp
        .identity()
        .get_media_snapshot(report.result_media_snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result_snapshot.payload["streams"][1]["channels"], 6);
    assert_eq!(
        result_snapshot.payload["streams"][2]["id"],
        companion.result_snapshot_stream_id
    );
    assert_eq!(result_snapshot.payload["streams"][2]["channels"], 2);
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 1).await;
    let event = latest_event_payload(&cp, "artifact.audio_transcode_succeeded").await;
    assert_eq!(event["synthesis_operation_id"], "node_synthesis_test");
    assert_eq!(
        event["synthesis_operation_key"],
        format!("synthesize:{}:node_synthesis_test", source.version.0)
    );
    assert_eq!(
        event["synthesized_companions"][0]["companion_id"],
        companion.companion_id
    );
    assert_eq!(
        event["synthesized_companions"][0]["source_snapshot_stream_id"],
        "a-1"
    );
    assert_eq!(
        event["synthesized_companions"][0]["result_provider_stream_index"],
        2
    );
    let counts = synthesis_publication_counts(&cp).await;

    let replay = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &UncalledTranscodeDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(replay, report);
    assert_eq!(synthesis_publication_counts(&cp).await, counts);
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 1).await;
}

#[tokio::test]
async fn malformed_synthesis_output_fences_staging_generation_before_retry() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_surround_audio_snapshot(&cp, source.version).await;
    let input = synthesis_input_for_source(&source, &dir);

    let error = execute_transcode_audio_with_dispatchers(
        &cp,
        input.clone(),
        &PartialSynthesisDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
    let generation: i64 =
        sqlx::query_scalar("SELECT dispatch_generation FROM audio_synthesis_operations")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(generation, 1);
    let first_status: String = sqlx::query_scalar(
        "SELECT status FROM audio_synthesis_dispatch_attempts WHERE generation = 0",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(first_status, "terminal");

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingSynthesisDispatcher {
            output_bytes: b"retry-synthesized".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SynthesisProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.synthesized_companions.len(), 1);
    assert!(
        report
            .staging_path
            .to_string_lossy()
            .contains("generation-1")
    );
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 1).await;
    let attempt_statuses = sqlx::query_scalar::<_, String>(
        "SELECT status FROM audio_synthesis_dispatch_attempts ORDER BY generation",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(attempt_statuses, vec!["terminal", "terminal"]);
}

#[tokio::test]
async fn active_synthesis_attempt_replays_same_worker_key_and_path_after_restart() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_surround_audio_snapshot(&cp, source.version).await;
    let input = synthesis_input_for_source(&source, &dir);
    let selected =
        source::select_source(&cp, input.source_file_version_id, input.source_location_id)
            .await
            .unwrap();
    let snapshot =
        source::read_media_snapshot(&cp, input.source_file_version_id, &input.operation_payload)
            .await
            .unwrap();
    let selection = selection::transcode_selection_from_payload_and_snapshot(
        &input.operation_payload,
        &snapshot,
    )
    .unwrap();
    let target = stage::synthesis_target_path(
        &input.target_dir,
        &selected.canonical_path,
        &selection.target_codec,
    )
    .await
    .unwrap();
    let operation = resolve_synthesis_operation(&cp, &input, &snapshot, &selection, &target)
        .await
        .unwrap();
    let now = cp.clock().now();
    let claim = NewAudioSynthesisClaim {
        operation_key: operation.operation.operation_key.clone(),
        expected_generation: 0,
        lease_id: input.lease_id,
        claim_token: "crashed-writer".to_owned(),
        expires_at: now + time::Duration::minutes(1),
    };
    cp.audio_synthesis_operations
        .acquire_claim(&claim, now)
        .await
        .unwrap();
    let staging = stage::prepare_synthesis_staging_path(
        &input.staging_root,
        &synthesis_operation_token(&operation.operation.operation_key),
        0,
        &selected.canonical_path,
        &selection.target_codec,
    )
    .await
    .unwrap();
    let idempotency_key = format!("audio-synthesis:{}:0", operation.operation.operation_key);
    let attempt = cp
        .audio_synthesis_operations
        .record_dispatch_attempt(
            &claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: input.lease_id,
                worker_id: 1,
                worker_epoch: 0,
                idempotency_key: idempotency_key.clone(),
                attempt_directory: staging.path.parent().unwrap().display().to_string(),
                staging_path: staging.path.display().to_string(),
            },
            now,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE audio_synthesis_operations SET claim_expires_at = ? WHERE id = ?")
        .bind(now - time::Duration::seconds(1))
        .bind(i64::try_from(operation.operation.id).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let expected_dispatch_lease_id = input.lease_id;
    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &ExpectedKeySynthesisDispatcher {
            expected_key: idempotency_key,
            expected_dispatch_lease_id,
            output_bytes: b"replayed-synthesis".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SynthesisProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.staging_path, staging.path);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM audio_synthesis_dispatch_attempts WHERE id = ?",
        )
        .bind(i64::try_from(attempt.id).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap(),
        "terminal"
    );
    assert_table_count(&cp, "audio_synthesis_dispatch_attempts", 1).await;
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 1).await;
}

#[tokio::test]
async fn staged_synthesis_probe_failure_reuses_bound_artifact_on_retry() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_surround_audio_snapshot(&cp, source.version).await;
    let input = synthesis_input_for_source(&source, &dir);

    let error = execute_transcode_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingSynthesisDispatcher {
            output_bytes: b"synthesized".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &FailingProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::Internal);
    assert_table_count(&cp, "artifact_handles", 1).await;
    assert_table_count(&cp, "artifact_locations", 1).await;
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 0).await;

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &UncalledTranscodeDispatcher,
        &SuccessfulVerifyDispatcher,
        &SynthesisProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.synthesized_companions.len(), 1);
    assert_table_count(&cp, "artifact_handles", 1).await;
    assert_table_count(&cp, "artifact_commit_records", 1).await;
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 1).await;
}

#[tokio::test]
async fn ambiguous_synthesis_dispatch_error_replays_same_attempt_key() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_surround_audio_snapshot(&cp, source.version).await;
    let mut input = synthesis_input_for_source(&source, &dir);

    let error = execute_transcode_audio_with_dispatchers(
        &cp,
        input.clone(),
        &CrashingSynthesisDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::WorkerCrash);
    let (idempotency_key, status): (String, String) =
        sqlx::query_as("SELECT idempotency_key, status FROM audio_synthesis_dispatch_attempts")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(status, "active");
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        "UPDATE leases SET state = 'released', release_reason = 'failure', released_at = ? \
         WHERE id = ?",
    )
    .bind(now)
    .bind(i64::try_from(input.lease_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let successor_lease_id = LeaseId(4);
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, ttl_seconds) \
         VALUES (?, 2, 1, 'held', ?, ?, ?, 3600)",
    )
    .bind(i64::try_from(successor_lease_id.0).unwrap())
    .bind(now)
    .bind(now + time::Duration::hours(1))
    .bind(now)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    input.lease_id = successor_lease_id;

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &ExpectedKeySynthesisDispatcher {
            expected_key: idempotency_key,
            expected_dispatch_lease_id: LeaseId(3),
            output_bytes: b"replayed-after-ambiguous-error".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SynthesisProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.synthesized_companions.len(), 1);
    assert_table_count(&cp, "audio_synthesis_dispatch_attempts", 1).await;
    assert_table_count(&cp, "artifact_handles", 1).await;
}

#[tokio::test]
async fn plural_synthesis_preserves_sources_and_reports_ordered_lineage() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_surround_audio_snapshot(&cp, source.version).await;
    let input = plural_synthesis_input_for_source(&source, &dir);

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &WritingSynthesisDispatcher {
            output_bytes: b"plural-synthesized".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &PluralSynthesisProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.synthesized_companions.len(), 2);
    assert_eq!(
        report
            .synthesized_companions
            .iter()
            .map(|companion| companion.source_snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-1", "a-2"]
    );
    assert_eq!(
        report
            .synthesized_companions
            .iter()
            .map(|companion| companion.result_provider_stream_index)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(
        report
            .synthesized_companions
            .iter()
            .all(|companion| companion.lineage_id > 0 && companion.channels == 2)
    );
    let snapshot = cp
        .identity()
        .get_media_snapshot(report.result_media_snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.payload["streams"][1]["channels"], 6);
    assert_eq!(snapshot.payload["streams"][2]["channels"], 6);
    assert_eq!(
        snapshot.payload["streams"][3]["id"],
        report.synthesized_companions[0].result_snapshot_stream_id
    );
    assert_eq!(
        snapshot.payload["streams"][4]["id"],
        report.synthesized_companions[1].result_snapshot_stream_id
    );
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 2).await;
}

#[tokio::test]
async fn synthesis_atomically_recovers_after_lineage_transaction_failure() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_surround_audio_snapshot(&cp, source.version).await;
    let input = synthesis_input_for_source(&source, &dir);
    sqlx::query(
        "CREATE TRIGGER fail_synthesis_lineage \
         BEFORE INSERT ON audio_synthesis_stream_lineage \
         BEGIN SELECT RAISE(ABORT, 'injected lineage failure'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = execute_transcode_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingSynthesisDispatcher {
            output_bytes: b"synthesized".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SynthesisProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::DbUnreachable);
    assert_table_count(&cp, "artifact_commit_records", 1).await;
    assert_table_count(&cp, "file_versions", 1).await;
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 0).await;
    assert_event_count(&cp, "artifact.commit_completed", 0).await;
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 0).await;
    sqlx::query("DROP TRIGGER fail_synthesis_lineage")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let counts_before_replay = synthesis_publication_counts(&cp).await;

    let report = execute_transcode_audio_with_dispatchers(
        &cp,
        input,
        &UncalledTranscodeDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.synthesized_companions.len(), 1);
    assert_table_count(&cp, "artifact_commit_records", 1).await;
    assert_table_count(&cp, "audio_synthesis_stream_lineage", 1).await;
    assert_event_count(&cp, "artifact.commit_completed", 1).await;
    assert_event_count(&cp, "artifact.audio_transcode_succeeded", 1).await;
    let counts_after_replay = synthesis_publication_counts(&cp).await;
    assert_eq!(counts_after_replay[0], counts_before_replay[0]);
    assert_eq!(counts_after_replay[1], counts_before_replay[1]);
    assert_eq!(counts_after_replay[2], counts_before_replay[2] + 1);
    assert_eq!(counts_after_replay[3], counts_before_replay[3] + 1);
    assert_eq!(counts_after_replay[4], counts_before_replay[4]);
    assert_eq!(counts_after_replay[5], counts_before_replay[5]);
    assert_eq!(counts_after_replay[6], counts_before_replay[6] + 1);
}

#[tokio::test]
async fn committed_legacy_singleton_is_adopted_once_without_redispatch() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let seeded = seed_legacy_singleton(&cp, &dir).await;
    let SeededLegacySingleton {
        input,
        target,
        legacy_commit_record_id,
        legacy_result_file_version_id,
        ..
    } = seeded;
    assert_table_count(&cp, "media_snapshots", 1).await;
    let immutable_counts = legacy_publication_counts(&cp).await;

    let adopted = execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(adopted.outputs.len(), 1);
    assert_eq!(adopted.outputs[0].target_path, target);
    assert_eq!(adopted.outputs[0].commit_record_id, legacy_commit_record_id);
    assert_eq!(
        adopted.outputs[0].result_file_version_id,
        legacy_result_file_version_id
    );
    assert_eq!(legacy_publication_counts(&cp).await, immutable_counts);
    assert_table_count(&cp, "media_snapshots", 2).await;
    assert_table_count(&cp, "audio_extract_operations", 1).await;
    assert_table_count(&cp, "audio_extract_operation_outputs", 1).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 1).await;

    let replay = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap();
    assert_eq!(replay, adopted);
    assert_eq!(legacy_publication_counts(&cp).await, immutable_counts);
    assert_table_count(&cp, "media_snapshots", 2).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 1).await;
}

#[tokio::test]
async fn legacy_adoption_rejects_ambiguous_or_different_source_snapshot() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let seeded = seed_legacy_singleton(&cp, &dir).await;
    let original = cp
        .identity()
        .get_media_snapshot(MediaSnapshotId(seeded.source.snapshot))
        .await
        .unwrap()
        .unwrap();
    let second = cp
        .record_media_snapshot(
            seeded.source.version,
            None,
            original.payload,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        )
        .await
        .unwrap();
    let mut input = seeded.input;
    input.operation_payload["source_media_snapshot_id"] = serde_json::json!(second.id.0);

    assert_legacy_adoption_rejected_without_mutation(&cp, input, &seeded.target).await;
}

#[tokio::test]
async fn published_operation_isolated_from_legacy_singleton() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let seeded = seed_legacy_singleton(&cp, &dir).await;
    let mut input = seeded.input;
    let operation_id = "different-published-operation";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([{
        "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
        "source_snapshot_stream_id": "a-1",
        "source_provider_stream_index": 1,
        "name_suffix": "a-1.opus.ogg",
        "bundle_role": "external_audio"
    }]);

    let legacy_bytes = tokio::fs::read(&seeded.target).await.unwrap();
    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"published-output".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();

    assert_ne!(report.commit_record_id, seeded.legacy_commit_record_id);
    assert_ne!(
        report.result_file_version_id,
        seeded.legacy_result_file_version_id
    );
    assert_ne!(report.target_path, seeded.target);
    assert!(report.target_path.is_file());
    assert_eq!(tokio::fs::read(&seeded.target).await.unwrap(), legacy_bytes);
    assert_table_count(&cp, "audio_extract_operations", 1).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 1).await;
}

#[derive(Debug, Clone, Copy)]
enum LegacyEvidenceMutation {
    WrongStream,
    MissingLineage,
    MismatchedVerification,
    RetiredResultLocation,
}

#[tokio::test]
async fn legacy_adoption_rejects_mismatched_or_incomplete_evidence() {
    for mutation in [
        LegacyEvidenceMutation::WrongStream,
        LegacyEvidenceMutation::MissingLineage,
        LegacyEvidenceMutation::MismatchedVerification,
        LegacyEvidenceMutation::RetiredResultLocation,
    ] {
        let (cp, _db, dir) = fixture_with_dir().await;
        let seeded = seed_legacy_singleton(&cp, &dir).await;
        mutate_legacy_evidence(&cp, mutation).await;

        assert_legacy_adoption_rejected_without_mutation(&cp, seeded.input, &seeded.target).await;
    }
}

#[tokio::test]
async fn legacy_adoption_rejects_unowned_existing_target() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let payload = input.operation_payload.as_object_mut().unwrap();
    payload.remove("operation_id");
    payload.remove("outputs");
    payload.remove("snapshot_stream_id");
    let snapshot = cp
        .identity()
        .get_media_snapshot(MediaSnapshotId(source.snapshot))
        .await
        .unwrap()
        .unwrap();
    let selection =
        selection::extract_selection_from_payload_and_snapshot(&input.operation_payload, &snapshot)
            .unwrap();
    let target = stage::extract_target_paths(
        &input.target_dir,
        &dir.path().join("source.mkv"),
        &selection,
    )
    .await
    .unwrap()
    .remove(0);
    tokio::fs::write(&target, b"unowned").await.unwrap();

    assert_legacy_adoption_rejected_without_mutation(&cp, input, &target).await;
}

#[tokio::test]
async fn exact_extract_quiescence_acknowledgement_records_audit_event() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let repo = SqliteAudioExtractOperationRepo::new(cp.pool.clone());
    let now = cp.clock().now();
    let operation = repo
        .create_planned(
            NewAudioExtractOperation {
                operation_key: "extract:quiescence-test".to_owned(),
                operation_id: Some("op-test".to_owned()),
                source_file_version_id: source.version,
                source_bundle_id: bundle.id,
                source_media_snapshot_id: MediaSnapshotId(source.snapshot),
            },
            &[NewAudioExtractOutput {
                output_id: Some("out-test".to_owned()),
                source_snapshot_stream_id: "a-1".to_owned(),
                source_provider_stream_index: 1,
                bundle_role: "external_audio".to_owned(),
                target_path: dir.path().join("out.ogg").display().to_string(),
            }],
            now,
        )
        .await
        .unwrap();
    let claim = NewAudioExtractClaim {
        operation_key: operation.operation.operation_key.clone(),
        expected_generation: 0,
        lease_id: LeaseId(3),
        claim_token: "quiescence-claim".to_owned(),
        expires_at: now + time::Duration::minutes(1),
    };
    repo.acquire_claim(&claim, now).await.unwrap();
    let attempt = repo
        .record_dispatch_attempt(
            &claim,
            NewAudioExtractDispatchAttempt {
                worker_id: voom_core::WorkerId(1),
                worker_epoch: 0,
                idempotency_key: "audio-extract:extract:quiescence-test:0".to_owned(),
                attempt_directory: dir.path().display().to_string(),
                paths: vec![dir.path().join("attempt.ogg").display().to_string()],
            },
            now,
        )
        .await
        .unwrap();
    repo.quarantine_dispatch(&claim, attempt.id, now)
        .await
        .unwrap();
    sqlx::query("UPDATE audio_extract_operations SET claim_expires_at = ? WHERE id = ?")
        .bind(now - time::Duration::seconds(1))
        .bind(i64::try_from(operation.operation.id).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    cp.acknowledge_extract_dispatch_quiescence(AcknowledgeExtractDispatchQuiescenceInput {
        operation_key: operation.operation.operation_key,
        generation: 0,
        attempt_id: attempt.id,
        worker_id: attempt.worker_id,
        worker_epoch: attempt.worker_epoch,
        idempotency_key: attempt.idempotency_key,
        acknowledged_by: "operator@example".to_owned(),
    })
    .await
    .unwrap();

    let event = latest_event_payload(&cp, "artifact.audio_extract_quiesced").await;
    assert_eq!(event["attempt_id"], attempt.id);
    assert_eq!(event["worker_id"], 1);
    assert_eq!(event["acknowledged_by"], "operator@example");
}

#[tokio::test]
async fn workflow_lease_heartbeat_renews_audio_operation_claims() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let now = cp.clock().now();
    let operation = cp
        .audio_extract_operations
        .create_planned(
            NewAudioExtractOperation {
                operation_key: "extract:heartbeat-test".to_owned(),
                operation_id: Some("op-heartbeat".to_owned()),
                source_file_version_id: source.version,
                source_bundle_id: bundle.id,
                source_media_snapshot_id: MediaSnapshotId(source.snapshot),
            },
            &[NewAudioExtractOutput {
                output_id: Some("out-heartbeat".to_owned()),
                source_snapshot_stream_id: "a-1".to_owned(),
                source_provider_stream_index: 1,
                bundle_role: "external_audio".to_owned(),
                target_path: dir.path().join("heartbeat.ogg").display().to_string(),
            }],
            now,
        )
        .await
        .unwrap();
    let claim = NewAudioExtractClaim {
        operation_key: operation.operation.operation_key,
        expected_generation: 0,
        lease_id: LeaseId(3),
        claim_token: "heartbeat-claim".to_owned(),
        expires_at: now + time::Duration::seconds(1),
    };
    cp.audio_extract_operations
        .acquire_claim(&claim, now)
        .await
        .unwrap();
    let synthesis_claim = seed_synthesis_heartbeat_claim(&cp, source, &dir, now).await;

    cp.heartbeat_lease(
        claim.lease_id,
        time::Duration::minutes(1),
        now + time::Duration::milliseconds(500),
    )
    .await
    .unwrap();
    cp.audio_extract_operations
        .record_dispatch_attempt(
            &claim,
            NewAudioExtractDispatchAttempt {
                worker_id: voom_core::WorkerId(1),
                worker_epoch: 0,
                idempotency_key: "audio-extract:extract:heartbeat-test:0".to_owned(),
                attempt_directory: dir.path().display().to_string(),
                paths: vec![dir.path().join("heartbeat.ogg").display().to_string()],
            },
            now + time::Duration::seconds(2),
        )
        .await
        .unwrap();
    cp.audio_synthesis_operations
        .record_dispatch_attempt(
            &synthesis_claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: LeaseId(3),
                worker_id: 1,
                worker_epoch: 0,
                idempotency_key: "audio-synthesis:synthesize:heartbeat-test:0".to_owned(),
                attempt_directory: dir.path().join("synthesis").display().to_string(),
                staging_path: dir
                    .path()
                    .join("synthesis")
                    .join("heartbeat.mkv")
                    .display()
                    .to_string(),
            },
            now + time::Duration::seconds(2),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn active_extract_attempt_replays_after_crash_before_worker_response() {
    assert_active_extract_attempt_replays(false).await;
}

#[tokio::test]
async fn active_extract_attempt_replays_after_response_before_terminal_persistence() {
    assert_active_extract_attempt_replays(true).await;
}

#[tokio::test]
async fn active_extract_attempt_from_lost_worker_epoch_requires_quiescence() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);
    let seeded = seed_active_extract_attempt(&cp, &input).await;
    sqlx::query("UPDATE workers SET epoch = 1 WHERE id = 1")
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);
    assert!(error.to_string().contains("must prove terminal completion"));
    assert_eq!(
        dispatch_attempt_status(&cp, seeded.attempt.id).await,
        "quarantined"
    );
    let row = sqlx::query(
        "SELECT dispatch_generation, claim_token FROM audio_extract_operations WHERE id = ?",
    )
    .bind(i64::try_from(seeded.attempt.operation_id).unwrap())
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(row.try_get::<i64, _>("dispatch_generation").unwrap(), 0);
    assert!(
        row.try_get::<Option<String>, _>("claim_token")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn malformed_active_extract_attempt_paths_quarantine_without_dispatch() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    set_plural_extract_outputs(&mut input, "node_extract_audio_malformed_replay");
    let seeded = seed_active_extract_attempt(&cp, &input).await;
    sqlx::query(
        "DELETE FROM audio_extract_dispatch_attempt_paths \
         WHERE attempt_id = ? AND ordinal = 1",
    )
    .bind(i64::try_from(seeded.attempt.id).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);
    assert!(
        error
            .to_string()
            .contains("deterministic ordered staging paths")
    );
    assert_eq!(
        dispatch_attempt_status(&cp, seeded.attempt.id).await,
        "quarantined"
    );
    assert_table_count(&cp, "audio_extract_dispatch_attempts", 1).await;
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
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-2"),
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
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();
    let retry = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
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
async fn committed_extract_rejects_replay_into_a_different_bundle() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let first_bundle = seed_bundle(&cp).await;
    let second_bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, first_bundle.id, &dir);

    execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();
    let mut replay = input;
    replay.source_bundle_id = second_bundle.id;

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        replay,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::ConfigInvalid);
    assert!(
        error
            .to_string()
            .contains("does not match persisted descriptor")
    );
    assert_eq!(
        cp.bundles
            .list_members(first_bundle.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        cp.bundles
            .list_members(second_bundle.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_table_count(&cp, "audio_extract_operations", 1).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 1).await;
}

#[tokio::test]
async fn prepared_extract_resume_failure_persists_diagnostics_then_recovers_without_duplicates() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let operation_id = "node_extract_audio_recovery";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-2"),
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
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::DbUnreachable);
    let target_dir = input.target_dir.join(format!("operation-{operation_id}"));
    assert!(target_dir.join("source.a-1.opus.ogg").is_file());
    assert!(target_dir.join("source.a-2.opus.ogg").is_file());
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "file_versions", 1).await;
    assert_table_count(&cp, "asset_bundle_members", 0).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 0).await;
    assert_prepared_resume_failure_is_durable(&cp, &input).await;
    sqlx::query("DROP TRIGGER fail_audio_extract_finalize")
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let recovered = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
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
    let failed = latest_event_payload(&cp, "artifact.audio_extract_failed").await;
    assert_eq!(failed["outputs"].as_array().unwrap().len(), 2);
    assert!(failed["outputs"][0]["artifact_handle_id"].is_u64());
    assert!(failed["outputs"][1]["artifact_handle_id"].is_u64());
}

async fn assert_prepared_resume_failure_is_durable(
    cp: &crate::ControlPlane,
    input: &ExecuteExtractAudioInput,
) {
    let operation_id = rewind_extract_recovery_to_prepared(cp).await;
    let blocking = cp
        .use_leases
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(input.source_file_version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "prepared-recovery-test".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(time::Duration::hours(1)),
            acquired_at: cp.clock().now(),
        })
        .await
        .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        cp,
        input.clone(),
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();
    assert_eq!(error.error_code(), voom_core::ErrorCode::BlockedByUseLease);
    let recovery = sqlx::query(
        "SELECT state, recovery_failure_class, recovery_error_code, recovery_message \
         FROM audio_extract_operations WHERE id = ?",
    )
    .bind(operation_id)
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        recovery.try_get::<String, _>("state").unwrap(),
        "recovery_required"
    );
    assert_eq!(
        recovery
            .try_get::<String, _>("recovery_failure_class")
            .unwrap(),
        "commit_failure"
    );
    assert_eq!(
        recovery
            .try_get::<String, _>("recovery_error_code")
            .unwrap(),
        "BLOCKED_BY_USE_LEASE"
    );
    assert!(
        recovery
            .try_get::<String, _>("recovery_message")
            .unwrap()
            .contains(&blocking.id.0.to_string())
    );
    assert_event_count(cp, "artifact.commit_recovery_required", 6).await;
    let claim_loss_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind = 'artifact.commit_recovery_required' \
         AND json_extract(payload, '$.recovery_reason') = \
             'audio extraction successor recovery after prior claim loss'",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(claim_loss_events, 2);
    assert_table_count(cp, "artifact_handles", 2).await;
    assert_table_count(cp, "artifact_commit_records", 2).await;
    assert_table_count(cp, "file_versions", 1).await;
    assert_table_count(cp, "asset_bundle_members", 0).await;
    assert_table_count(cp, "audio_extract_output_lineage", 0).await;
    cp.use_leases
        .release(
            blocking.id,
            UseLeaseReleaseReason::Released,
            cp.clock().now(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn prepared_successor_records_evidence_before_missing_member_field_decode() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);
    seed_rewound_prepared_extract(&cp, &input).await;
    sqlx::query("UPDATE audio_extract_operation_outputs SET temp_path = NULL")
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("missing temp_path"));
    assert_decode_failure_left_recovery_evidence(&cp, "missing temp_path").await;
}

#[tokio::test]
async fn prepared_successor_records_evidence_before_malformed_result_decode() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);
    seed_rewound_prepared_extract(&cp, &input).await;
    sqlx::query("UPDATE audio_extract_operation_outputs SET result_facts = '{}'")
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("malformed result_facts"));
    assert_decode_failure_left_recovery_evidence(&cp, "malformed result_facts").await;
}

async fn seed_rewound_prepared_extract(cp: &crate::ControlPlane, input: &ExecuteExtractAudioInput) {
    sqlx::query(
        "CREATE TRIGGER fail_audio_extract_finalize BEFORE INSERT ON file_assets \
         BEGIN SELECT RAISE(ABORT, 'injected finalize failure'); END;",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    execute_extract_audio_with_dispatchers(
        cp,
        input.clone(),
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap_err();
    sqlx::query("DROP TRIGGER fail_audio_extract_finalize")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    rewind_extract_recovery_to_prepared(cp).await;
}

async fn rewind_extract_recovery_to_prepared(cp: &crate::ControlPlane) -> i64 {
    let operation_id = sqlx::query_scalar("SELECT id FROM audio_extract_operations")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE audio_extract_operations SET state = 'prepared', \
         recovery_failure_class = NULL, recovery_error_code = NULL, recovery_message = NULL \
         WHERE id = ?",
    )
    .bind(operation_id)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE artifact_commit_records SET state = 'pending', failure_class = NULL, \
         error_code = NULL, message = NULL, recovery_reason = NULL, finished_at = NULL",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    operation_id
}

async fn assert_decode_failure_left_recovery_evidence(
    cp: &crate::ControlPlane,
    expected_message: &str,
) {
    let row = sqlx::query("SELECT state, recovery_message FROM audio_extract_operations")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(
        row.try_get::<String, _>("state").unwrap(),
        "recovery_required"
    );
    assert!(
        row.try_get::<String, _>("recovery_message")
            .unwrap()
            .contains(expected_message)
    );
    assert_table_count(cp, "file_versions", 1).await;
    assert_table_count(cp, "asset_bundle_members", 0).await;
    assert_table_count(cp, "audio_extract_output_lineage", 0).await;
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
        input.clone(),
        &MissingOutputsExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
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
async fn partial_extract_result_cleans_terminal_staging_and_retries_without_duplicates() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let operation_id = "node_extract_audio_partial_retry";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);

    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &PartialOutputsExtractDispatcher {
            output_bytes: b"partial".to_vec(),
        },
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
    assert_table_count(&cp, "artifact_handles", 0).await;
    assert_table_count(&cp, "artifact_commit_records", 0).await;

    let retry = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &WritingExtractDispatcher {
            output_bytes: b"complete".to_vec(),
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();
    assert_eq!(retry.outputs.len(), 2);
    assert_table_count(&cp, "artifact_handles", 2).await;
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "asset_bundle_members", 2).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 2).await;
}

#[tokio::test]
async fn second_extract_verification_failure_never_promotes_or_registers_any_member() {
    let (cp, _db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    let operation_id = "node_extract_audio_verify_second";
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);
    let first_target = input.target_dir.join("source.a-1.opus.ogg");
    let second_target = input.target_dir.join("source.a-2.opus.ogg");

    let verify = FailSecondVerifyDispatcher::default();
    let probe = SessionTrackingUncalledProbeDispatcher::default();
    let error = execute_extract_audio_with_dispatchers(
        &cp,
        input.clone(),
        &WritingExtractDispatcher {
            output_bytes: b"extracted".to_vec(),
        },
        &verify,
        &probe,
    )
    .await
    .unwrap_err();

    assert_eq!(verify.sessions.load(Ordering::Relaxed), 1);
    assert_eq!(verify.calls.load(Ordering::Relaxed), 2);
    assert_eq!(verify.shutdowns.load(Ordering::Relaxed), 1);
    assert_eq!(probe.sessions.load(Ordering::Relaxed), 1);
    assert_eq!(probe.dispatches.load(Ordering::Relaxed), 0);
    assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 1);
    assert_eq!(
        error.error_code(),
        voom_core::ErrorCode::VerificationFailure
    );
    assert!(!first_target.exists());
    assert!(!second_target.exists());
    assert_table_count(&cp, "artifact_commit_records", 0).await;
    assert_table_count(&cp, "asset_bundle_members", 0).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 0).await;
    let verification_rows =
        sqlx::query("SELECT status, error_code FROM artifact_verifications ORDER BY id ASC")
            .fetch_all(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(verification_rows.len(), 2);
    assert_eq!(
        verification_rows[0].try_get::<String, _>("status").unwrap(),
        "succeeded"
    );
    assert_eq!(
        verification_rows[1].try_get::<String, _>("status").unwrap(),
        "failed"
    );
    assert_eq!(
        verification_rows[1]
            .try_get::<String, _>("error_code")
            .unwrap(),
        "WORKER_CRASH"
    );
    assert_event_count(&cp, "artifact.verification_started", 2).await;
    assert_event_count(&cp, "artifact.verification_succeeded", 1).await;
    assert_event_count(&cp, "artifact.verification_failed", 1).await;

    let retry = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &UncalledExtractDispatcher,
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();
    assert_eq!(retry.outputs.len(), 2);
    assert_table_count(&cp, "artifact_handles", 2).await;
    assert_table_count(&cp, "artifact_commit_records", 2).await;
    assert_table_count(&cp, "asset_bundle_members", 2).await;
    assert_table_count(&cp, "audio_extract_output_lineage", 2).await;
    let failed = latest_event_payload(&cp, "artifact.audio_extract_failed").await;
    assert_eq!(failed["outputs"].as_array().unwrap().len(), 2);
    assert!(failed["outputs"][0]["artifact_handle_id"].is_u64());
    assert!(failed["outputs"][1]["artifact_handle_id"].is_u64());
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
            &UncalledProbeDispatcher,
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
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();

    assert!(report.target_path.is_file());
    assert!(report.commit_record_id.0 > 0);
    assert!(report.result_file_version_id.0 > source.version.0);
    assert_event_count(&cp, "artifact.audio_extract_succeeded", 1).await;
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
        &SucceedingProbeDispatcher,
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

async fn fixture() -> (crate::ControlPlane, voom_test_support::TempDatabase) {
    let db = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    seed_extract_execution_lease(&pool).await;
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(1))),
    )
    .await
    .unwrap();
    (cp, db)
}

async fn seed_extract_execution_lease(pool: &sqlx::SqlitePool) {
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    sqlx::query(
        "INSERT INTO workers \
         (id, name, kind, status, registered_at, last_seen_at) \
         VALUES (1, 'audio-test-worker', 'synthetic', 'active', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) \
         VALUES (1, 'audio-test', 'open', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (2, 1, 'audio-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, \
          last_heartbeat_at, ttl_seconds) VALUES (3, 2, 1, 'held', ?, ?, ?, 3600)",
    )
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn fixture_with_dir() -> (
    crate::ControlPlane,
    voom_test_support::TempDatabase,
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

async fn seed_synthesis_heartbeat_claim(
    cp: &crate::ControlPlane,
    source: SeededAudioSource,
    dir: &tempfile::TempDir,
    now: OffsetDateTime,
) -> NewAudioSynthesisClaim {
    let synthesis = cp
        .audio_synthesis_operations
        .create_planned(
            NewAudioSynthesisOperation {
                operation_key: "synthesize:heartbeat-test".to_owned(),
                planned_operation_id: "op-synthesis-heartbeat".to_owned(),
                source_file_version_id: source.version,
                source_media_snapshot_id: MediaSnapshotId(source.snapshot),
                target_codec: "aac".to_owned(),
                target_channels: 2,
                container: "mkv".to_owned(),
                target_path: dir.path().join("synthesized.mkv").display().to_string(),
            },
            &[NewAudioSynthesisCompanion {
                companion_id: "derived-a-1".to_owned(),
                source_snapshot_stream_id: "a-1".to_owned(),
                source_provider_stream_index: 1,
                result_snapshot_stream_id: "derived-a-1".to_owned(),
            }],
            now,
        )
        .await
        .unwrap();
    let claim = NewAudioSynthesisClaim {
        operation_key: synthesis.operation.operation_key,
        expected_generation: 0,
        lease_id: LeaseId(3),
        claim_token: "synthesis-heartbeat-claim".to_owned(),
        expires_at: now + time::Duration::seconds(1),
    };
    cp.audio_synthesis_operations
        .acquire_claim(&claim, now)
        .await
        .unwrap();
    claim
}

struct SeededLegacySingleton {
    source: SeededAudioSource,
    input: ExecuteExtractAudioInput,
    target: PathBuf,
    legacy_commit_record_id: ArtifactCommitRecordId,
    legacy_result_file_version_id: FileVersionId,
}

struct HistoricalStagedArtifact {
    handle_id: ArtifactHandleId,
    verification_id: ArtifactVerificationId,
}

struct SeededActiveExtractAttempt {
    attempt: voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttempt,
    staging: stage::PreparedStagingPaths,
}

async fn seed_active_extract_attempt(
    cp: &crate::ControlPlane,
    input: &ExecuteExtractAudioInput,
) -> SeededActiveExtractAttempt {
    let mut context = ExtractAttemptContext::default();
    let prepared = prepare_extract_execution(cp, input, &UncalledProbeDispatcher, &mut context)
        .await
        .unwrap();
    let dispatch = claim_extract_dispatch(
        cp,
        input,
        &prepared.paths.operation,
        &prepared.paths.targets,
    )
    .await
    .unwrap();
    let attempt_directory = dispatch
        .staging
        .paths
        .first()
        .and_then(|path| path.parent())
        .unwrap()
        .display()
        .to_string();
    let attempt = cp
        .audio_extract_operations
        .record_dispatch_attempt(
            &dispatch.claim,
            NewAudioExtractDispatchAttempt {
                worker_id: dispatch.worker_id,
                worker_epoch: dispatch.worker_epoch,
                idempotency_key: dispatch.idempotency_key,
                attempt_directory,
                paths: dispatch
                    .staging
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            },
            cp.clock().now(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE audio_extract_operations SET claim_expires_at = ? WHERE id = ?")
        .bind(OffsetDateTime::UNIX_EPOCH)
        .bind(i64::try_from(prepared.paths.operation.operation.id).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    SeededActiveExtractAttempt {
        attempt,
        staging: dispatch.staging,
    }
}

async fn assert_active_extract_attempt_replays(worker_already_responded: bool) {
    let (cp, _db, dir) = fixture_with_dir().await;
    let source = seed_audio_source(&cp, &dir, b"source").await;
    let bundle = seed_bundle(&cp).await;
    let input = extract_input_for_source(&source, bundle.id, &dir);
    let seeded = seed_active_extract_attempt(&cp, &input).await;
    let output_bytes = b"replayed-extract".to_vec();
    if worker_already_responded {
        for path in &seeded.staging.paths {
            tokio::fs::write(path, &output_bytes).await.unwrap();
        }
    }
    let expected_paths = seeded.attempt.paths.clone();
    let report = execute_extract_audio_with_dispatchers(
        &cp,
        input,
        &ReplayedExtractDispatcher {
            expected_idempotency_key: seeded.attempt.idempotency_key.clone(),
            expected_paths,
            output_bytes,
            worker_already_responded,
        },
        &SuccessfulVerifyDispatcher,
        &SucceedingProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.outputs.len(), 1);
    assert_table_count(&cp, "audio_extract_dispatch_attempts", 1).await;
    assert_eq!(
        dispatch_attempt_status(&cp, seeded.attempt.id).await,
        "terminal"
    );
}

async fn dispatch_attempt_status(cp: &crate::ControlPlane, attempt_id: u64) -> String {
    sqlx::query_scalar("SELECT status FROM audio_extract_dispatch_attempts WHERE id = ?")
        .bind(i64::try_from(attempt_id).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

fn set_plural_extract_outputs(input: &mut ExecuteExtractAudioInput, operation_id: &str) {
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["outputs"] = serde_json::json!([
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "name_suffix": "a-1.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-2"),
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "name_suffix": "a-2.opus.ogg",
            "bundle_role": "commentary_audio"
        }
    ]);
}

async fn seed_legacy_singleton(
    cp: &crate::ControlPlane,
    dir: &tempfile::TempDir,
) -> SeededLegacySingleton {
    let source = seed_audio_source(cp, dir, b"source").await;
    let bundle = seed_bundle(cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, dir);
    let payload = input.operation_payload.as_object_mut().unwrap();
    payload.remove("operation_id");
    payload.remove("outputs");
    payload.remove("snapshot_stream_id");
    let snapshot = cp
        .identity()
        .get_media_snapshot(MediaSnapshotId(source.snapshot))
        .await
        .unwrap()
        .unwrap();
    let selection =
        selection::extract_selection_from_payload_and_snapshot(&input.operation_payload, &snapshot)
            .unwrap();
    let targets = stage::extract_target_paths(
        &input.target_dir,
        &dir.path().join("source.mkv"),
        &selection,
    )
    .await
    .unwrap();
    let target = targets[0].clone();
    let staging = dir.path().join("legacy-singleton.ogg");
    let bytes = b"legacy-sidecar";
    tokio::fs::write(&staging, bytes).await.unwrap();
    let output = observed(u64::try_from(bytes.len()).unwrap(), &blake3_checksum(bytes));
    let staged = seed_historical_staged_artifact(cp, source, &staging, &output).await;
    tokio::fs::write(&target, bytes).await.unwrap();
    let sidecar =
        commit_historical_sidecar(cp, source.version, bundle.id, &staged, &target, &output).await;
    SeededLegacySingleton {
        source,
        input,
        target,
        legacy_commit_record_id: sidecar.commit_record.id,
        legacy_result_file_version_id: sidecar.file_version_id,
    }
}

async fn seed_historical_staged_artifact(
    cp: &crate::ControlPlane,
    source: SeededAudioSource,
    staging: &std::path::Path,
    output: &AudioObservedFacts,
) -> HistoricalStagedArtifact {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let handle = cp
        .artifacts()
        .create_handle_in_tx(
            &mut tx,
            NewArtifactHandle {
                size_bytes: Some(i64::try_from(output.size_bytes).unwrap()),
                checksum: Some(output.content_hash.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec!["local_path".to_owned()],
                mutability: "immutable".to_owned(),
                source_lineage: Some(serde_json::json!({
                    "operation": "extract_audio",
                    "source_file_version_id": source.version.0,
                    "source_file_location_id": source.location.0,
                    "selected_snapshot_stream_id": "a-1",
                    "intended_role": "external_audio",
                })),
                file_version_id: Some(source.version),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let location = cp
        .artifacts()
        .record_location_in_tx(
            &mut tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: "staging".to_owned(),
                value: staging.display().to_string(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let verification = crate::artifact::verify::verify_artifact_with_dispatcher(
        cp,
        crate::artifact::VerifyArtifactInput::for_staged_file(handle.id, staging),
        &SuccessfulVerifyDispatcher,
        &crate::artifact::verify::NoVerifyArtifactHooks,
    )
    .await
    .unwrap();
    assert_eq!(verification.artifact_location_id, location.id);
    HistoricalStagedArtifact {
        handle_id: handle.id,
        verification_id: verification.verification_id,
    }
}

async fn commit_historical_sidecar(
    cp: &crate::ControlPlane,
    source_file_version_id: FileVersionId,
    source_bundle_id: BundleId,
    staged: &HistoricalStagedArtifact,
    target: &std::path::Path,
    output: &AudioObservedFacts,
) -> voom_store::repo::media::artifacts::SidecarArtifactCommit {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let pending = cp
        .artifacts()
        .create_pending_commit_in_tx(
            &mut tx,
            NewArtifactCommitRecord {
                artifact_handle_id: staged.handle_id,
                source_file_version_id,
                verification_id: staged.verification_id,
                target_path: target.display().to_string(),
                temp_path: Some(format!("{}.legacy-tmp", target.display())),
                report: serde_json::json!({"operation": "extract_audio_sidecar"}),
                started_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let sidecar = cp
        .artifacts()
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: pending.id,
                target_path: target.display().to_string(),
                content_hash: output.content_hash.clone(),
                size_bytes: output.size_bytes,
                observed_at: OffsetDateTime::UNIX_EPOCH,
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    cp.bundles
        .add_member_in_tx(
            &mut tx,
            NewBundleMember {
                bundle_id: source_bundle_id,
                file_asset_id: sidecar.file_asset_id,
                role: BundleMemberRole::ExternalAudio,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sidecar
}

async fn mutate_legacy_evidence(cp: &crate::ControlPlane, mutation: LegacyEvidenceMutation) {
    let query = match mutation {
        LegacyEvidenceMutation::WrongStream => {
            "UPDATE artifact_handles \
             SET source_lineage = json_set(source_lineage, \
                 '$.selected_snapshot_stream_id', 'a-2')"
        }
        LegacyEvidenceMutation::MissingLineage => {
            "UPDATE artifact_handles SET source_lineage = NULL"
        }
        LegacyEvidenceMutation::MismatchedVerification => {
            "UPDATE artifact_verifications SET expected_checksum = 'blake3:mismatch'"
        }
        LegacyEvidenceMutation::RetiredResultLocation => {
            "UPDATE file_locations SET retired_at = '1970-01-01T00:00:01Z' \
             WHERE id = (SELECT result_file_location_id FROM artifact_commit_records)"
        }
    };
    sqlx::query(query)
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

async fn assert_legacy_adoption_rejected_without_mutation(
    cp: &crate::ControlPlane,
    input: ExecuteExtractAudioInput,
    target: &std::path::Path,
) {
    let publication_counts = legacy_publication_counts(cp).await;
    let media_snapshot_count = table_count(cp, "media_snapshots").await;
    let target_bytes = tokio::fs::read(target).await.unwrap();

    let error = execute_extract_audio_with_dispatchers(
        cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);
    assert_eq!(legacy_publication_counts(cp).await, publication_counts);
    assert_eq!(
        table_count(cp, "media_snapshots").await,
        media_snapshot_count
    );
    assert_eq!(table_count(cp, "audio_extract_operations").await, 0);
    assert_eq!(table_count(cp, "audio_extract_operation_outputs").await, 0);
    assert_eq!(table_count(cp, "audio_extract_output_lineage").await, 0);
    assert_eq!(tokio::fs::read(target).await.unwrap(), target_bytes);
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

async fn record_surround_audio_snapshot(
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
                    "channels": 6,
                    "disposition": {
                        "default": true,
                        "forced": false,
                        "commentary": false
                    }
                }
            ]
        }),
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap()
    .id
    .0
}

async fn record_plural_surround_audio_snapshot(
    cp: &crate::ControlPlane,
    file_version_id: FileVersionId,
) -> u64 {
    cp.record_media_snapshot(
        file_version_id,
        None,
        serde_json::json!({
            "container": "mkv",
            "streams": [
                {"id": "v-1", "index": 0, "kind": "video", "codec_name": "h264"},
                {
                    "id": "a-1",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "ac3",
                    "language": "eng",
                    "title": "Main",
                    "channels": 6,
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
                    "codec_name": "ac3",
                    "language": "jpn",
                    "title": "Secondary",
                    "channels": 6,
                    "disposition": {
                        "default": false,
                        "forced": false,
                        "commentary": false
                    }
                }
            ]
        }),
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap()
    .id
    .0
}

async fn seed_bundle(cp: &crate::ControlPlane) -> voom_store::repo::media::bundles::AssetBundle {
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

fn synthesis_input_for_source(
    source: &SeededAudioSource,
    dir: &tempfile::TempDir,
) -> ExecuteTranscodeAudioInput {
    let operation_id = "node_synthesis_test";
    let companion_id = voom_plan::planner::audio::synthesis_companion_id(operation_id, "a-1");
    ExecuteTranscodeAudioInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: source.version,
        source_location_id: Some(source.location),
        operation_payload: serde_json::json!({
            "type": "synthesize_audio",
            "operation_id": operation_id,
            "target_codec": "aac",
            "target_channels": 2,
            "container": "mkv",
            "source_media_snapshot_id": source.snapshot,
            "filter": {"type": "channels", "op": "gte", "value": 6},
            "companions": [{
                "companion_id": companion_id,
                "source_snapshot_stream_id": "a-1",
                "source_provider_stream_index": 1,
                "result_snapshot_stream_id": companion_id
            }]
        }),
        staging_root: dir.path().join("voom-audio-stage"),
        target_dir: dir.path().join("voom-audio-out"),
        backup_root: None,
    }
}

fn plural_synthesis_input_for_source(
    source: &SeededAudioSource,
    dir: &tempfile::TempDir,
) -> ExecuteTranscodeAudioInput {
    let operation_id = "node_plural_synthesis_test";
    let first = voom_plan::planner::audio::synthesis_companion_id(operation_id, "a-1");
    let second = voom_plan::planner::audio::synthesis_companion_id(operation_id, "a-2");
    let mut input = synthesis_input_for_source(source, dir);
    input.operation_payload["operation_id"] = serde_json::json!(operation_id);
    input.operation_payload["filter"] =
        serde_json::json!({"type": "channels", "op": "gte", "value": 6});
    input.operation_payload["companions"] = serde_json::json!([
        {
            "companion_id": first,
            "source_snapshot_stream_id": "a-1",
            "source_provider_stream_index": 1,
            "result_snapshot_stream_id": first
        },
        {
            "companion_id": second,
            "source_snapshot_stream_id": "a-2",
            "source_provider_stream_index": 2,
            "result_snapshot_stream_id": second
        }
    ]);
    input
}

async fn synthesis_publication_counts(cp: &crate::ControlPlane) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "artifact_handles",
        "artifact_commit_records",
        "file_versions",
        "media_snapshots",
        "audio_synthesis_operations",
        "audio_synthesis_companions",
        "audio_synthesis_stream_lineage",
    ] {
        counts.push(assert_table_count_value(cp, table).await);
    }
    counts
}

async fn assert_table_count_value(cp: &crate::ControlPlane, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) AS count FROM {table}");
    sqlx::query_scalar(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
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
                "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "a-1"),
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

fn first_extract_plan_input(
    source: &SeededAudioSource,
    dir: &tempfile::TempDir,
) -> FirstExtractPlanInput {
    let input = extract_input_for_source(source, BundleId(0), dir);
    FirstExtractPlanInput {
        source_file_version_id: input.source_file_version_id,
        source_location_id: input.source_location_id,
        operation_payload: input.operation_payload,
        target_dir: input.target_dir,
    }
}

async fn table_counts<const N: usize>(cp: &crate::ControlPlane, tables: &[&str; N]) -> [i64; N] {
    let mut counts = [0; N];
    for (index, table) in tables.iter().enumerate() {
        counts[index] = table_count(cp, table).await;
    }
    counts
}

async fn directory_is_empty(path: &std::path::Path) -> bool {
    let mut entries = tokio::fs::read_dir(path).await.unwrap();
    entries.next_entry().await.unwrap().is_none()
}

#[derive(Default)]
struct RecordingClaimFences {
    boundaries: std::sync::Mutex<Vec<usize>>,
}

impl RecordingClaimFences {
    fn boundaries(&self) -> Vec<usize> {
        self.boundaries.lock().unwrap().clone()
    }
}

#[async_trait]
impl commit::ExtractClaimFenceHooks for RecordingClaimFences {
    async fn before_assert(
        &self,
        _cp: &ControlPlane,
        context: commit::ExtractClaimFenceContext<'_>,
    ) -> Result<(), VoomError> {
        self.boundaries.lock().unwrap().push(context.boundary_index);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum ClaimLossMode {
    Expiry,
    Takeover,
    GenerationAdvance,
}

impl ClaimLossMode {
    const ALL: [Self; 3] = [Self::Expiry, Self::Takeover, Self::GenerationAdvance];

    const fn label(self) -> &'static str {
        match self {
            Self::Expiry => "expiry",
            Self::Takeover => "takeover",
            Self::GenerationAdvance => "generation",
        }
    }
}

struct MutatingClaimFence {
    boundary_index: usize,
    mode: ClaimLossMode,
    injected: std::sync::atomic::AtomicBool,
    boundaries: std::sync::Mutex<Vec<usize>>,
}

impl MutatingClaimFence {
    fn new(boundary_index: usize, mode: ClaimLossMode) -> Self {
        Self {
            boundary_index,
            mode,
            injected: std::sync::atomic::AtomicBool::new(false),
            boundaries: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn boundaries(&self) -> Vec<usize> {
        self.boundaries.lock().unwrap().clone()
    }
}

#[async_trait]
impl commit::ExtractClaimFenceHooks for MutatingClaimFence {
    async fn before_assert(
        &self,
        cp: &ControlPlane,
        context: commit::ExtractClaimFenceContext<'_>,
    ) -> Result<(), VoomError> {
        assert_eq!(context.member_count, 2);
        self.boundaries.lock().unwrap().push(context.boundary_index);
        if context.boundary_index != self.boundary_index
            || self.injected.swap(true, Ordering::Relaxed)
        {
            return Ok(());
        }
        mutate_extract_claim(cp, context.claim, context.boundary_index, self.mode).await
    }
}

async fn mutate_extract_claim(
    cp: &ControlPlane,
    claim: &NewAudioExtractClaim,
    boundary_index: usize,
    mode: ClaimLossMode,
) -> Result<(), VoomError> {
    let takeover_lease_id = if let ClaimLossMode::Takeover = mode {
        Some(seed_takeover_lease(cp).await?)
    } else {
        None
    };
    let result = match mode {
        ClaimLossMode::Expiry => {
            sqlx::query(
                "UPDATE audio_extract_operations SET claim_expires_at = ? \
                 WHERE operation_key = ? AND dispatch_generation = ? \
                 AND claim_lease_id = ? AND claim_token = ?",
            )
            .bind("1970-01-01T00:00:00Z")
            .bind(&claim.operation_key)
            .bind(i64::from(claim.expected_generation))
            .bind(i64::try_from(claim.lease_id.0).unwrap())
            .bind(&claim.claim_token)
            .execute(cp.pool_for_test())
            .await
        }
        ClaimLossMode::Takeover => {
            let Some(takeover_lease_id) = takeover_lease_id else {
                return Err(VoomError::Internal(
                    "takeover mutation is missing its competing lease".to_owned(),
                ));
            };
            sqlx::query(
                "UPDATE audio_extract_operations SET claim_lease_id = ?, claim_token = ? \
                 WHERE operation_key = ? AND dispatch_generation = ? \
                 AND claim_lease_id = ? AND claim_token = ?",
            )
            .bind(i64::try_from(takeover_lease_id.0).unwrap())
            .bind(format!("takeover-boundary-{boundary_index}"))
            .bind(&claim.operation_key)
            .bind(i64::from(claim.expected_generation))
            .bind(i64::try_from(claim.lease_id.0).unwrap())
            .bind(&claim.claim_token)
            .execute(cp.pool_for_test())
            .await
        }
        ClaimLossMode::GenerationAdvance => {
            sqlx::query(
                "UPDATE audio_extract_operations \
                 SET dispatch_generation = dispatch_generation + 1, \
                     claim_lease_id = NULL, claim_token = NULL, claim_expires_at = NULL \
                 WHERE operation_key = ? AND dispatch_generation = ? \
                 AND claim_lease_id = ? AND claim_token = ?",
            )
            .bind(&claim.operation_key)
            .bind(i64::from(claim.expected_generation))
            .bind(i64::try_from(claim.lease_id.0).unwrap())
            .bind(&claim.claim_token)
            .execute(cp.pool_for_test())
            .await
        }
    }
    .map_err(|error| VoomError::database_context("inject audio extract claim loss", error))?;
    if result.rows_affected() != 1 {
        return Err(VoomError::Internal(format!(
            "claim-loss injection at boundary {boundary_index} changed {} rows",
            result.rows_affected()
        )));
    }
    Ok(())
}

async fn seed_takeover_lease(cp: &ControlPlane) -> Result<LeaseId, VoomError> {
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (4, 1, 'audio-takeover-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(cp.pool_for_test())
    .await
    .map_err(|error| VoomError::database_context("seed takeover ticket", error))?;
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, \
          last_heartbeat_at, ttl_seconds) VALUES (5, 4, 1, 'held', ?, ?, ?, 3600)",
    )
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .execute(cp.pool_for_test())
    .await
    .map_err(|error| VoomError::database_context("seed takeover lease", error))?;
    Ok(LeaseId(5))
}

async fn plural_extract_fixture(
    operation_id: &str,
) -> (
    crate::ControlPlane,
    voom_test_support::TempDatabase,
    tempfile::TempDir,
    ExecuteExtractAudioInput,
) {
    let (cp, db, dir) = fixture_with_dir().await;
    let mut source = seed_audio_source(&cp, &dir, b"source").await;
    source.snapshot = record_plural_audio_snapshot(&cp, source.version).await;
    let bundle = seed_bundle(&cp).await;
    let mut input = extract_input_for_source(&source, bundle.id, &dir);
    set_plural_extract_outputs(&mut input, operation_id);
    (cp, db, dir, input)
}

async fn assert_fresh_claim_loss(mode: ClaimLossMode, boundary_index: usize) {
    let operation_id = format!("fresh_{}_{}", mode.label(), boundary_index);
    let (cp, _db, _dir, input) = plural_extract_fixture(&operation_id).await;
    let fences = MutatingClaimFence::new(boundary_index, mode);
    let error = execute_plural_extract_with_fences(&cp, input.clone(), &fences).await;

    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);
    assert_eq!(
        fences.boundaries(),
        (0..=boundary_index).collect::<Vec<_>>()
    );
    assert_promoted_prefix(&input, &operation_id, boundary_index);
    assert_extract_operation_state(&cp, "prepared").await;
    assert_extract_generation(&cp, mode).await;
    assert_artifact_commit_state(&cp, "pending", 2).await;
    assert_unpublished_extract_state(&cp).await;
    assert_event_count(&cp, "artifact.commit_recovery_required", 0).await;
    assert_event_count(&cp, "artifact.audio_extract_failed", 1).await;

    expire_current_extract_claim(&cp).await;
    retry_plural_extract(&cp, input).await;
    assert_extract_operation_state(&cp, "committed").await;
    assert_artifact_commit_state(&cp, "committed", 2).await;
    assert_event_count(&cp, "artifact.commit_recovery_required", 2).await;
    assert_event_count(&cp, "artifact.audio_extract_failed", 1).await;
}

async fn assert_recovery_claim_loss(mode: ClaimLossMode, boundary_index: usize) {
    let operation_id = format!("recovery_{}_{}", mode.label(), boundary_index);
    let (cp, _db, _dir, input) = plural_extract_fixture(&operation_id).await;
    let initial_loss = MutatingClaimFence::new(0, ClaimLossMode::Expiry);
    let initial_error = execute_plural_extract_with_fences(&cp, input.clone(), &initial_loss).await;
    assert_eq!(initial_error.error_code(), voom_core::ErrorCode::Conflict);
    let fences = MutatingClaimFence::new(boundary_index, mode);
    let error = execute_resume_with_fences(&cp, input.clone(), &fences).await;

    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);
    assert_eq!(
        fences.boundaries(),
        (0..=boundary_index).collect::<Vec<_>>()
    );
    assert_promoted_prefix(&input, &operation_id, boundary_index);
    assert_extract_operation_state(&cp, "recovery_required").await;
    assert_extract_generation(&cp, mode).await;
    assert_artifact_commit_state(&cp, "recovery_required", 2).await;
    assert_unpublished_extract_state(&cp).await;
    assert_event_count(&cp, "artifact.commit_recovery_required", 2).await;
    assert_event_count(&cp, "artifact.audio_extract_failed", 2).await;

    expire_current_extract_claim(&cp).await;
    retry_plural_extract(&cp, input).await;
    assert_extract_operation_state(&cp, "committed").await;
    assert_artifact_commit_state(&cp, "committed", 2).await;
    assert_event_count(&cp, "artifact.commit_recovery_required", 2).await;
    assert_event_count(&cp, "artifact.audio_extract_failed", 2).await;
}

async fn execute_plural_extract_with_fences(
    cp: &ControlPlane,
    input: ExecuteExtractAudioInput,
    fences: &dyn commit::ExtractClaimFenceHooks,
) -> VoomError {
    execute_extract_audio_with_services(
        cp,
        input,
        ExtractAudioExecutionServices {
            extract: &WritingExtractDispatcher {
                output_bytes: b"extracted".to_vec(),
            },
            verify: &SuccessfulVerifyDispatcher,
            result_probe: &SucceedingProbeDispatcher,
            claim_fence_hooks: fences,
        },
    )
    .await
    .unwrap_err()
}

async fn execute_resume_with_fences(
    cp: &ControlPlane,
    input: ExecuteExtractAudioInput,
    fences: &dyn commit::ExtractClaimFenceHooks,
) -> VoomError {
    execute_extract_audio_with_services(
        cp,
        input,
        ExtractAudioExecutionServices {
            extract: &UncalledExtractDispatcher,
            verify: &UncalledVerifyDispatcher,
            result_probe: &UncalledProbeDispatcher,
            claim_fence_hooks: fences,
        },
    )
    .await
    .unwrap_err()
}

async fn retry_plural_extract(cp: &ControlPlane, input: ExecuteExtractAudioInput) {
    let report = execute_extract_audio_with_dispatchers(
        cp,
        input,
        &UncalledExtractDispatcher,
        &UncalledVerifyDispatcher,
        &UncalledProbeDispatcher,
    )
    .await
    .unwrap();
    assert_eq!(report.outputs.len(), 2);
}

fn assert_promoted_prefix(
    input: &ExecuteExtractAudioInput,
    operation_id: &str,
    promoted_count: usize,
) {
    let target_dir = input.target_dir.join(format!("operation-{operation_id}"));
    for (index, name) in ["source.a-1.opus.ogg", "source.a-2.opus.ogg"]
        .iter()
        .enumerate()
    {
        assert_eq!(
            target_dir.join(name).is_file(),
            index < promoted_count,
            "unexpected target state for member {index} at boundary {promoted_count}"
        );
    }
}

async fn assert_extract_operation_state(cp: &ControlPlane, expected: &str) {
    let states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM audio_extract_operations ORDER BY id")
            .fetch_all(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(states, vec![expected]);
}

async fn assert_extract_generation(cp: &ControlPlane, mode: ClaimLossMode) {
    let generation: i64 =
        sqlx::query_scalar("SELECT dispatch_generation FROM audio_extract_operations")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let expected = match mode {
        ClaimLossMode::Expiry | ClaimLossMode::Takeover => 0,
        ClaimLossMode::GenerationAdvance => 1,
    };
    assert_eq!(generation, expected);
}

async fn assert_artifact_commit_state(cp: &ControlPlane, expected: &str, count: i64) {
    let actual: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM artifact_commit_records WHERE state = ?")
            .bind(expected)
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(actual, count);
    assert_table_count(cp, "artifact_commit_records", count).await;
}

async fn assert_unpublished_extract_state(cp: &ControlPlane) {
    assert_table_count(cp, "file_versions", 1).await;
    assert_table_count(cp, "file_locations", 1).await;
    assert_table_count(cp, "media_snapshots", 2).await;
    assert_table_count(cp, "asset_bundle_members", 0).await;
    assert_table_count(cp, "audio_extract_output_lineage", 0).await;
}

async fn expire_current_extract_claim(cp: &ControlPlane) {
    sqlx::query(
        "UPDATE audio_extract_operations SET claim_expires_at = ? \
         WHERE claim_token IS NOT NULL",
    )
    .bind("1970-01-01T00:00:00Z")
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

async fn legacy_publication_counts(cp: &crate::ControlPlane) -> (i64, i64, i64, i64, i64) {
    let row = sqlx::query(
        "SELECT \
         (SELECT COUNT(*) FROM artifact_handles) AS handles, \
         (SELECT COUNT(*) FROM artifact_commit_records) AS commits, \
         (SELECT COUNT(*) FROM file_versions) AS versions, \
         (SELECT COUNT(*) FROM file_locations) AS locations, \
         (SELECT COUNT(*) FROM asset_bundle_members) AS members",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    (
        row.try_get("handles").unwrap(),
        row.try_get("commits").unwrap(),
        row.try_get("versions").unwrap(),
        row.try_get("locations").unwrap(),
        row.try_get("members").unwrap(),
    )
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
    assert_eq!(table_count(cp, table).await, expected);
}

async fn table_count(cp: &crate::ControlPlane, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) AS count FROM {table}");
    let row = sqlx::query(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    row.try_get("count").unwrap()
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
        _dispatch_lease_id: LeaseId,
        _idempotency_key: &str,
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
        _idempotency_key: &str,
        _request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        panic!("extract dispatcher should not be called")
    }
}

struct CrashingExtractDispatcher;

#[async_trait]
impl ExtractAudioDispatcher for CrashingExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        _idempotency_key: &str,
        _request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        Err(VoomError::WorkerCrash("injected worker crash".to_owned()))
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

#[derive(Default)]
struct FailSecondVerifyDispatcher {
    calls: AtomicUsize,
    sessions: AtomicUsize,
    shutdowns: AtomicUsize,
}

#[async_trait]
impl VerifyArtifactDispatcher for FailSecondVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if call == 1 {
            return Err(crate::artifact::worker::VerifyWorkerError::terminal_error(
                voom_core::FailureClass::WorkerCrash,
                voom_core::ErrorCode::WorkerCrash,
                "injected mid-set verifier crash",
            ));
        }
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

    fn start_session(&self) -> Box<dyn crate::artifact::verify::VerifyArtifactSession + '_> {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        Box::new(FailSecondVerifySession { dispatcher: self })
    }
}

struct FailSecondVerifySession<'a> {
    dispatcher: &'a FailSecondVerifyDispatcher,
}

#[async_trait]
impl VerifyArtifactDispatcher for FailSecondVerifySession<'_> {
    async fn dispatch_verify_artifact(
        &self,
        worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        self.dispatcher
            .dispatch_verify_artifact(worker_id, request)
            .await
    }
}

#[async_trait]
impl crate::artifact::verify::VerifyArtifactSession for FailSecondVerifySession<'_> {
    async fn shutdown(self: Box<Self>) {
        self.dispatcher.shutdowns.fetch_add(1, Ordering::Relaxed);
    }
}

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

#[derive(Default)]
struct SessionTrackingVerifyDispatcher {
    sessions: AtomicUsize,
    dispatches: AtomicUsize,
    shutdowns: AtomicUsize,
}

#[async_trait]
impl VerifyArtifactDispatcher for SessionTrackingVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
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

    fn start_session(&self) -> Box<dyn crate::artifact::verify::VerifyArtifactSession + '_> {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        Box::new(SessionTrackingVerifySession { dispatcher: self })
    }
}

struct SessionTrackingVerifySession<'a> {
    dispatcher: &'a SessionTrackingVerifyDispatcher,
}

#[async_trait]
impl VerifyArtifactDispatcher for SessionTrackingVerifySession<'_> {
    async fn dispatch_verify_artifact(
        &self,
        worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        self.dispatcher
            .dispatch_verify_artifact(worker_id, request)
            .await
    }
}

#[async_trait]
impl crate::artifact::verify::VerifyArtifactSession for SessionTrackingVerifySession<'_> {
    async fn shutdown(self: Box<Self>) {
        self.dispatcher.shutdowns.fetch_add(1, Ordering::Relaxed);
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

#[derive(Default)]
struct SessionTrackingProbeDispatcher {
    sessions: AtomicUsize,
    dispatches: AtomicUsize,
    shutdowns: AtomicUsize,
}

#[async_trait]
impl commit::AudioResultProbeDispatcher for SessionTrackingProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &crate::ControlPlane,
        request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        SucceedingProbeDispatcher
            .dispatch_result_probe(cp, request)
            .await
    }

    fn start_session(&self) -> Box<dyn commit::AudioResultProbeSession + '_> {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        Box::new(SessionTrackingProbeSession { dispatcher: self })
    }
}

struct SessionTrackingProbeSession<'a> {
    dispatcher: &'a SessionTrackingProbeDispatcher,
}

#[async_trait]
impl commit::AudioResultProbeDispatcher for SessionTrackingProbeSession<'_> {
    async fn dispatch_result_probe(
        &self,
        cp: &crate::ControlPlane,
        request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        self.dispatcher.dispatch_result_probe(cp, request).await
    }
}

#[async_trait]
impl commit::AudioResultProbeSession for SessionTrackingProbeSession<'_> {
    async fn shutdown(self: Box<Self>) {
        self.dispatcher.shutdowns.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct SessionTrackingUncalledProbeDispatcher {
    sessions: AtomicUsize,
    dispatches: AtomicUsize,
    shutdowns: AtomicUsize,
}

#[async_trait]
impl commit::AudioResultProbeDispatcher for SessionTrackingUncalledProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        _cp: &crate::ControlPlane,
        _request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        panic!("probe dispatcher should not be called")
    }

    fn start_session(&self) -> Box<dyn commit::AudioResultProbeSession + '_> {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        Box::new(SessionTrackingUncalledProbeSession { dispatcher: self })
    }
}

struct SessionTrackingUncalledProbeSession<'a> {
    dispatcher: &'a SessionTrackingUncalledProbeDispatcher,
}

#[async_trait]
impl commit::AudioResultProbeDispatcher for SessionTrackingUncalledProbeSession<'_> {
    async fn dispatch_result_probe(
        &self,
        cp: &crate::ControlPlane,
        request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<commit::ProbedAudioResult, VoomError> {
        self.dispatcher.dispatch_result_probe(cp, request).await
    }
}

#[async_trait]
impl commit::AudioResultProbeSession for SessionTrackingUncalledProbeSession<'_> {
    async fn shutdown(self: Box<Self>) {
        self.dispatcher.shutdowns.fetch_add(1, Ordering::Relaxed);
    }
}

struct SynthesisProbeDispatcher;
struct PluralSynthesisProbeDispatcher;

#[async_trait]
impl commit::AudioResultProbeDispatcher for SynthesisProbeDispatcher {
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
                    "container": "mkv",
                    "streams": [
                        {
                            "index": 0,
                            "kind": "video",
                            "codec_name": "h264"
                        },
                        {
                            "index": 1,
                            "kind": "audio",
                            "codec_name": "aac",
                            "channels": 6,
                            "language": "eng",
                            "title": "Main",
                            "disposition": {
                                "default": true,
                                "forced": false,
                                "commentary": false
                            }
                        },
                        {
                            "index": 2,
                            "kind": "audio",
                            "codec_name": "aac",
                            "channels": 2,
                            "language": "eng",
                            "title": "Main",
                            "disposition": {
                                "default": true,
                                "forced": false,
                                "commentary": false
                            }
                        }
                    ]
                }),
            },
        })
    }
}

#[async_trait]
impl commit::AudioResultProbeDispatcher for PluralSynthesisProbeDispatcher {
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
                    "container": "mkv",
                    "streams": [
                        {"index": 0, "kind": "video", "codec_name": "h264"},
                        {
                            "index": 1, "kind": "audio", "codec_name": "ac3", "channels": 6,
                            "language": "eng", "title": "Main",
                            "disposition": {
                                "default": true, "forced": false, "commentary": false
                            }
                        },
                        {
                            "index": 2, "kind": "audio", "codec_name": "ac3", "channels": 6,
                            "language": "jpn", "title": "Secondary",
                            "disposition": {
                                "default": false, "forced": false, "commentary": false
                            }
                        },
                        {
                            "index": 3, "kind": "audio", "codec_name": "aac", "channels": 2,
                            "language": "eng", "title": "Main",
                            "disposition": {
                                "default": true, "forced": false, "commentary": false
                            }
                        },
                        {
                            "index": 4, "kind": "audio", "codec_name": "aac", "channels": 2,
                            "language": "jpn", "title": "Secondary",
                            "disposition": {
                                "default": false, "forced": false, "commentary": false
                            }
                        }
                    ]
                }),
            },
        })
    }
}

struct WritingTranscodeDispatcher {
    output_bytes: Vec<u8>,
}

struct WritingSynthesisDispatcher {
    output_bytes: Vec<u8>,
}

struct ExpectedKeySynthesisDispatcher {
    expected_key: String,
    expected_dispatch_lease_id: LeaseId,
    output_bytes: Vec<u8>,
}

struct PartialSynthesisDispatcher;

struct CrashingSynthesisDispatcher;

#[async_trait]
impl TranscodeAudioDispatcher for CrashingSynthesisDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        _dispatch_lease_id: LeaseId,
        _idempotency_key: &str,
        _request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        Err(VoomError::WorkerCrash("injected worker crash".to_owned()))
    }
}

#[async_trait]
impl TranscodeAudioDispatcher for PartialSynthesisDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        _dispatch_lease_id: LeaseId,
        _idempotency_key: &str,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        let bytes = b"partial";
        tokio::fs::write(&request.output.path, bytes).await.unwrap();
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
            output: observed(u64::try_from(bytes.len()).unwrap(), &blake3_checksum(bytes)),
            output_container: "mkv".to_owned(),
            selected_snapshot_stream_ids: request
                .selection
                .selected_streams
                .iter()
                .map(|stream| stream.snapshot_stream_id.clone())
                .collect(),
            output_audio_codecs: vec!["aac".to_owned()],
            selected_output_streams: Vec::new(),
        })
    }
}

#[async_trait]
impl TranscodeAudioDispatcher for WritingSynthesisDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        _dispatch_lease_id: LeaseId,
        _idempotency_key: &str,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        tokio::fs::write(&request.output.path, &self.output_bytes)
            .await
            .unwrap();
        let base_index = u32::try_from(request.selection.selected_streams.len() + 1).unwrap();
        let output_streams = request
            .selection
            .selected_streams
            .iter()
            .enumerate()
            .map(|(ordinal, selected)| {
                let (language, title, default) = if ordinal == 0 {
                    ("eng", "Main", true)
                } else {
                    ("jpn", "Secondary", false)
                };
                AudioOutputStreamFact {
                    snapshot_stream_id: selected.snapshot_stream_id.clone(),
                    output_provider_stream_index: base_index + u32::try_from(ordinal).unwrap(),
                    codec: "aac".to_owned(),
                    language: Some(language.to_owned()),
                    title: Some(title.to_owned()),
                    default: Some(default),
                    disposition: Some(voom_worker_protocol::AudioDispositionFact {
                        default: Some(default),
                        forced: Some(false),
                        commentary: Some(false),
                    }),
                    channels: Some(2),
                }
            })
            .collect::<Vec<_>>();
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
                &blake3_checksum(&self.output_bytes),
            ),
            output_container: "mkv".to_owned(),
            selected_snapshot_stream_ids: output_streams
                .iter()
                .map(|stream| stream.snapshot_stream_id.clone())
                .collect(),
            output_audio_codecs: vec!["aac".to_owned(); output_streams.len()],
            selected_output_streams: output_streams,
        })
    }
}

#[async_trait]
impl TranscodeAudioDispatcher for ExpectedKeySynthesisDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        dispatch_lease_id: LeaseId,
        idempotency_key: &str,
        request: TranscodeAudioRequest,
    ) -> Result<TranscodeAudioResult, VoomError> {
        assert_eq!(idempotency_key, self.expected_key);
        assert_eq!(dispatch_lease_id, self.expected_dispatch_lease_id);
        WritingSynthesisDispatcher {
            output_bytes: self.output_bytes.clone(),
        }
        .dispatch_transcode_audio(dispatch_lease_id, idempotency_key, request)
        .await
    }
}

#[async_trait]
impl TranscodeAudioDispatcher for WritingTranscodeDispatcher {
    async fn dispatch_transcode_audio(
        &self,
        _dispatch_lease_id: LeaseId,
        _idempotency_key: &str,
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

struct ReplayedExtractDispatcher {
    expected_idempotency_key: String,
    expected_paths: Vec<String>,
    output_bytes: Vec<u8>,
    worker_already_responded: bool,
}

struct MissingOutputsExtractDispatcher {
    output_bytes: Vec<u8>,
}

struct PartialOutputsExtractDispatcher {
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
        idempotency_key: &str,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let mut result = WritingExtractDispatcher {
            output_bytes: self.output_bytes.clone(),
        }
        .dispatch_extract_audio(idempotency_key, request)
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
        idempotency_key: &str,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let mut result = WritingExtractDispatcher {
            output_bytes: self.output_bytes.clone(),
        }
        .dispatch_extract_audio(idempotency_key, request)
        .await?;
        result.outputs = None;
        Ok(result)
    }
}

#[async_trait]
impl ExtractAudioDispatcher for PartialOutputsExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        idempotency_key: &str,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        let mut result = WritingExtractDispatcher {
            output_bytes: self.output_bytes.clone(),
        }
        .dispatch_extract_audio(idempotency_key, request.clone())
        .await?;
        let outputs = request.outputs.as_ref().unwrap();
        for output in &outputs[1..] {
            tokio::fs::remove_file(&output.output.path).await.unwrap();
        }
        result.outputs.as_mut().unwrap().truncate(1);
        Ok(result)
    }
}

#[async_trait]
impl ExtractAudioDispatcher for WritingExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        _idempotency_key: &str,
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
        Ok(extract_result_for_request(&request, &self.output_bytes))
    }
}

#[async_trait]
impl ExtractAudioDispatcher for ReplayedExtractDispatcher {
    async fn dispatch_extract_audio(
        &self,
        idempotency_key: &str,
        request: ExtractAudioRequest,
    ) -> Result<ExtractAudioResult, VoomError> {
        assert_eq!(idempotency_key, self.expected_idempotency_key);
        let paths = request
            .outputs
            .as_ref()
            .unwrap()
            .iter()
            .map(|output| output.output.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(paths, self.expected_paths);
        for path in &paths {
            if self.worker_already_responded {
                assert_eq!(tokio::fs::read(path).await.unwrap(), self.output_bytes);
            } else {
                tokio::fs::write(path, &self.output_bytes).await.unwrap();
            }
        }
        Ok(extract_result_for_request(&request, &self.output_bytes))
    }
}

fn extract_result_for_request(
    request: &ExtractAudioRequest,
    output_bytes: &[u8],
) -> ExtractAudioResult {
    let output_hash = blake3_checksum(output_bytes);
    let output = observed(u64::try_from(output_bytes.len()).unwrap(), &output_hash);
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
    ExtractAudioResult {
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
