use super::*;
use voom_core::{FailureClass, NodeKind, NodeStatus, TicketOperation, WorkerKind};
fn issue_payload(status: &str) -> IssueLifecyclePayload {
    IssueLifecyclePayload {
        issue_id: voom_core::IssueId(7),
        kind: "policy_noncompliant".to_owned(),
        status: status.to_owned(),
        dedupe_key: Some(
            "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=a".to_owned(),
        ),
        policy_version_id: Some(voom_core::PolicyVersionId(2)),
        report_id: Some("report_abc".to_owned()),
    }
}

/// Compile-time exhaustiveness guard for the serde-tag test below.
///
/// This match names every `Event` variant, so adding a new variant is a
/// compile error here until the arm is added. When you add an arm, you
/// MUST also add a matching sample to the `events` list in
/// `event_kind_matches_serde_tag` so the new variant's serde tag is
/// actually asserted — the guard proves the list *should* cover every
/// variant, but only the list's per-variant assertion proves it *does*.
#[expect(
    clippy::match_same_arms,
    reason = "one arm per Event variant — a new variant must fail to compile here; \
              identical empty bodies are intentional, never collapse them"
)]
fn _event_variants_are_exhaustive(e: &Event) {
    match e {
        Event::SchemaInitialized(_) => {}
        Event::JobOpened(_) => {}
        Event::JobSucceeded(_) => {}
        Event::JobFailed(_) => {}
        Event::JobCancelled(_) => {}
        Event::TicketCreated(_) => {}
        Event::TicketReady(_) => {}
        Event::TicketLeased(_) => {}
        Event::TicketSucceeded(_) => {}
        Event::TicketFailedRetriable(_) => {}
        Event::TicketFailedTerminal(_) => {}
        Event::TicketRequeuedAfterLeaseExpiry(_) => {}
        Event::TicketRequeuedAfterForceRelease(_) => {}
        Event::LeaseAcquired(_) => {}
        Event::LeaseReleased(_) => {}
        Event::LeaseExpired(_) => {}
        Event::LeaseForceReleased(_) => {}
        Event::NodeRegistered(_) => {}
        Event::NodeHeartbeatRecorded(_) => {}
        Event::NodeMarkedStale(_) => {}
        Event::NodeRetired(_) => {}
        Event::WorkerRegistered(_) => {}
        Event::WorkerLinkedToNode(_) => {}
        Event::WorkerCapabilityRecorded(_) => {}
        Event::WorkerGrantRecorded(_) => {}
        Event::WorkerRetired(_) => {}
        Event::ArtifactHandleCreated(_) => {}
        Event::ArtifactLocationRecorded(_) => {}
        Event::ArtifactLocationRetired(_) => {}
        Event::ArtifactLineageRecorded(_) => {}
        Event::ArtifactStaged(_) => {}
        Event::ArtifactVerificationStarted(_) => {}
        Event::ArtifactVerificationSucceeded(_) => {}
        Event::ArtifactVerificationFailed(_) => {}
        Event::ArtifactCommitStarted(_) => {}
        Event::ArtifactCommitCompleted(_) => {}
        Event::ArtifactCommitFailedPreMutation(_) => {}
        Event::ArtifactCommitRecoveryRequired(_) => {}
        Event::ArtifactTranscodeStarted(_) => {}
        Event::ArtifactTranscodeProgress(_) => {}
        Event::ArtifactTranscodeSucceeded(_) => {}
        Event::ArtifactTranscodeFailed(_) => {}
        Event::ArtifactRemuxStarted(_) => {}
        Event::ArtifactRemuxProgress(_) => {}
        Event::ArtifactRemuxSucceeded(_) => {}
        Event::ArtifactRemuxFailed(_) => {}
        Event::ArtifactAudioTranscodeStarted(_) => {}
        Event::ArtifactAudioTranscodeProgress(_) => {}
        Event::ArtifactAudioTranscodeSucceeded(_) => {}
        Event::ArtifactAudioTranscodeFailed(_) => {}
        Event::ArtifactAudioExtractStarted(_) => {}
        Event::ArtifactAudioExtractProgress(_) => {}
        Event::ArtifactAudioExtractSucceeded(_) => {}
        Event::ArtifactAudioExtractFailed(_) => {}
        Event::IssueOpened(_) => {}
        Event::IssueUpdated(_) => {}
        Event::IssueResolved(_) => {}
        Event::MediaWorkCreated(_) => {}
        Event::MediaVariantCreated(_) => {}
        Event::AssetBundleCreated(_) => {}
        Event::AssetBundleMemberAdded(_) => {}
        Event::AssetBundleMemberRemoved(_) => {}
        Event::FileAssetCreated(_) => {}
        Event::FileVersionCreated(_) => {}
        Event::FileLocationRecorded(_) => {}
        Event::FileLocationAliased(_) => {}
        Event::FileLocationRetiredByMove(_) => {}
        Event::FileLocationRecordedByMove(_) => {}
        Event::IdentityEvidenceRecorded(_) => {}
        Event::IdentityEvidenceAccepted(_) => {}
        Event::IdentityEvidenceSuperseded(_) => {}
        Event::MediaSnapshotRecorded(_) => {}
        Event::UseLeaseAcquired(_) => {}
        Event::UseLeaseReleased(_) => {}
        Event::UseLeaseExpired(_) => {}
        Event::UseLeaseForceReleased(_) => {}
        Event::UseLeaseRecoveredStaleIssuer(_) => {}
        Event::UseLeaseReanchoredByMove(_) => {}
        Event::CommitIntentRecorded(_) => {}
        Event::CommitAbortedByUseLease(_) => {}
        Event::CommitAbortedByStaleEvidence(_) => {}
        Event::CommitAbortedByClosureIncomplete(_) => {}
        Event::CommitAbortedByPendingCommit(_) => {}
        Event::CommitAuthorized(_) => {}
        Event::CommitAbortedByClosureGrew(_) => {}
        Event::CommitCompleted(_) => {}
        Event::CommitAbortedPreMutation(_) => {}
        Event::CommitAbortedPostMutation(_) => {}
        Event::CommitRecoveryRequired(_) => {}
        Event::CommitForcedOverride(_) => {}
        Event::ExternalSystemRegistered(_) => {}
        Event::ExternalSystemHealthChanged(_) => {}
        Event::ExternalSystemLinked(_) => {}
        Event::ExternalSystemUnlinked(_) => {}
        Event::ExternalSystemSynced(_) => {}
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one sample per Event variant — the list must stay exhaustive"
)]
fn event_kind_matches_serde_tag() {
    use time::OffsetDateTime;

    // Exactly one constructed sample per Event variant. For each sample we
    // assert the serde tag (`to_value(..)["kind"]`) equals
    // `event.kind().as_str()`, catching any drift between the per-variant
    // `#[serde(rename = "...")]` table and `Event::kind()`.
    //
    // The Vec literal does NOT enforce exhaustiveness on its own — a
    // missing variant simply goes untested. The compile-time guard
    // `_event_variants_are_exhaustive` provides that check: a new variant
    // fails to compile there, prompting a maintainer to add both an arm
    // and a sample here.
    let events: Vec<Event> = vec![
        Event::SchemaInitialized(SchemaInitializedPayload {
            migrations_applied: 1,
            schema_init_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::JobOpened(JobOpenedPayload {
            job_id: 1,
            kind: "k".to_owned(),
            priority: 0,
        }),
        Event::JobSucceeded(JobSucceededPayload { job_id: 1 }),
        Event::JobFailed(JobFailedPayload {
            job_id: 1,
            reason: "r".to_owned(),
        }),
        Event::JobCancelled(JobCancelledPayload {
            job_id: 1,
            reason: "r".to_owned(),
        }),
        Event::TicketCreated(TicketCreatedPayload {
            ticket_id: 1,
            job_id: None,
            kind: TicketOperation::new("k").unwrap(),
            priority: 0,
            max_attempts: 1,
        }),
        Event::TicketReady(TicketReadyPayload { ticket_id: 1 }),
        Event::TicketLeased(TicketLeasedPayload {
            ticket_id: 1,
            lease_id: 1,
            worker_id: 1,
            attempt: 1,
        }),
        Event::TicketSucceeded(TicketSucceededPayload {
            ticket_id: 1,
            lease_id: 1,
        }),
        Event::TicketFailedRetriable(TicketFailedRetriablePayload {
            ticket_id: 1,
            attempt: 1,
            max_attempts: 3,
            reason: "r".to_owned(),
            class: FailureClass::WorkerTimeout,
            next_eligible_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::TicketFailedTerminal(TicketFailedTerminalPayload {
            ticket_id: 1,
            attempt: 3,
            max_attempts: 3,
            reason: "r".to_owned(),
            class: FailureClass::MalformedWorkerResult,
            issue_id: None,
        }),
        Event::TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload {
            ticket_id: 1,
            lease_id: 1,
        }),
        Event::TicketRequeuedAfterForceRelease(TicketRequeuedAfterForceReleasePayload {
            ticket_id: 1,
            lease_id: 1,
            actor: "op".to_owned(),
            reason: "test".to_owned(),
        }),
        Event::LeaseAcquired(LeaseAcquiredPayload {
            lease_id: 1,
            ticket_id: 1,
            worker_id: 1,
            ttl_seconds: 60,
            expires_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::LeaseReleased(LeaseReleasedPayload {
            lease_id: 1,
            ticket_id: 1,
            release_reason: "released".to_owned(),
        }),
        Event::LeaseExpired(LeaseExpiredPayload {
            lease_id: 1,
            ticket_id: 1,
        }),
        Event::LeaseForceReleased(LeaseForceReleasedPayload {
            lease_id: 1,
            ticket_id: 1,
            actor: "a".to_owned(),
            reason: "r".to_owned(),
            also_requeue: false,
        }),
        Event::NodeRegistered(NodeRegisteredPayload {
            node_id: 1,
            name: "n".to_owned(),
            kind: NodeKind::Local,
            status: NodeStatus::Active,
            heartbeat_ttl_seconds: 60,
        }),
        Event::NodeHeartbeatRecorded(NodeHeartbeatRecordedPayload {
            node_id: 1,
            status: NodeStatus::Active,
            last_seen_at: OffsetDateTime::UNIX_EPOCH,
            epoch: 1,
        }),
        Event::NodeMarkedStale(NodeMarkedStalePayload {
            node_id: 1,
            marked_stale_at: OffsetDateTime::UNIX_EPOCH,
            epoch: 2,
        }),
        Event::NodeRetired(NodeRetiredPayload {
            node_id: 1,
            retired_at: OffsetDateTime::UNIX_EPOCH,
            epoch: 3,
        }),
        Event::WorkerRegistered(WorkerRegisteredPayload {
            worker_id: 1,
            name: "w".to_owned(),
            kind: WorkerKind::Synthetic,
        }),
        Event::WorkerLinkedToNode(WorkerLinkedToNodePayload {
            worker_id: 1,
            node_id: 1,
        }),
        Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
            worker_id: 1,
            capability_id: 1,
            operation: TicketOperation::new("op").unwrap(),
        }),
        Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
            worker_id: 1,
            grant_id: 1,
        }),
        Event::WorkerRetired(WorkerRetiredPayload { worker_id: 1 }),
        Event::ArtifactHandleCreated(ArtifactHandleCreatedPayload {
            artifact_handle_id: 1,
            privacy_class: "internal".to_owned(),
            durability_class: "durable".to_owned(),
            mutability: "immutable".to_owned(),
        }),
        Event::ArtifactLocationRecorded(ArtifactLocationRecordedPayload {
            artifact_location_id: 1,
            artifact_handle_id: 1,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
        }),
        Event::ArtifactLocationRetired(ArtifactLocationRetiredPayload {
            artifact_location_id: 1,
            artifact_handle_id: 1,
        }),
        Event::ArtifactLineageRecorded(ArtifactLineageRecordedPayload {
            artifact_lineage_id: 1,
            parent_artifact_id: 1,
            child_artifact_id: 2,
            operation: "transcode".to_owned(),
        }),
        Event::ArtifactStaged(ArtifactStagedPayload {
            artifact_handle_id: 1,
            artifact_location_id: 1,
            source_file_version_id: 1,
            source_file_location_id: None,
            staging_path: "/staging/1".to_owned(),
            size_bytes: 1,
            checksum: "blake3:1".to_owned(),
        }),
        Event::ArtifactVerificationStarted(ArtifactVerificationStartedPayload {
            artifact_handle_id: 1,
            artifact_location_id: 1,
            worker_id: 1,
            path: "/staging/1".to_owned(),
        }),
        Event::ArtifactVerificationSucceeded(ArtifactVerificationSucceededPayload {
            verification_id: 1,
            artifact_handle_id: 1,
            artifact_location_id: 1,
            worker_id: 1,
            observed_size_bytes: 1,
            observed_checksum: "blake3:1".to_owned(),
        }),
        Event::ArtifactVerificationFailed(ArtifactVerificationFailedPayload {
            verification_id: 2,
            artifact_handle_id: 1,
            artifact_location_id: 1,
            worker_id: 1,
            error_code: "VERIFY_FAILED".to_owned(),
        }),
        Event::ArtifactCommitStarted(ArtifactCommitStartedPayload {
            commit_record_id: 1,
            artifact_handle_id: 1,
            source_file_version_id: 1,
            verification_id: 1,
            target_path: "/target".to_owned(),
            temp_path: "/.target.tmp".to_owned(),
        }),
        Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
            commit_record_id: 1,
            artifact_handle_id: 1,
            result_file_version_id: 2,
            result_file_location_id: 2,
            target_path: "/target".to_owned(),
            gate_evaluated_lease_ids: Vec::new(),
        }),
        Event::ArtifactCommitFailedPreMutation(ArtifactCommitFailedPreMutationPayload {
            artifact_handle_id: 1,
            commit_record_id: None,
            target_path: "/target".to_owned(),
            error_code: "VERIFY_REQUIRED".to_owned(),
            message: "verification required".to_owned(),
        }),
        Event::ArtifactCommitRecoveryRequired(ArtifactCommitRecoveryRequiredPayload {
            commit_record_id: 1,
            artifact_handle_id: 1,
            target_path: "/target".to_owned(),
            temp_path: "/.target.tmp".to_owned(),
            recovery_reason: "promotion_failed".to_owned(),
            error_code: "PROMOTION_FAILED".to_owned(),
            message: "promotion failed".to_owned(),
        }),
        Event::ArtifactTranscodeStarted(ArtifactTranscodeStartedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            profile_name: "default-hevc".to_owned(),
            encoder: "libx265".to_owned(),
            target_codec: "hevc".to_owned(),
            output_container: "mkv".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactTranscodeProgress(ArtifactTranscodeProgressPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            staging_path: "/staging/x".to_owned(),
            profile_name: "default-hevc".to_owned(),
            encoder: "libx265".to_owned(),
            target_codec: "hevc".to_owned(),
            output_container: "mkv".to_owned(),
            percent_bps: None,
            message: None,
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactTranscodeSucceeded(ArtifactTranscodeSucceededPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            artifact_handle_id: 1,
            artifact_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            profile_name: "default-hevc".to_owned(),
            encoder: "libx265".to_owned(),
            target_codec: "hevc".to_owned(),
            output_container: "mkv".to_owned(),
            output_video_codec: "hevc".to_owned(),
            copied_video: false,
            output_width: 1920,
            output_height: 1080,
            output_pixel_format: "yuv420p".to_owned(),
            provider: "ffmpeg".to_owned(),
            provider_version: "1".to_owned(),
        }),
        Event::ArtifactTranscodeFailed(ArtifactTranscodeFailedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: None,
            staging_path: None,
            profile_name: "default-hevc".to_owned(),
            encoder: "libx265".to_owned(),
            target_codec: "hevc".to_owned(),
            output_container: "mkv".to_owned(),
            failure_class: FailureClass::WorkerCrash,
            error_code: "TRANSCODE_FAILED".to_owned(),
            message: "m".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactRemuxStarted(ArtifactRemuxStartedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_streams: Vec::new(),
            default_streams: Vec::new(),
            clear_default_streams: Vec::new(),
            track_order: Vec::new(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactRemuxProgress(ArtifactRemuxProgressPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_streams: Vec::new(),
            default_streams: Vec::new(),
            clear_default_streams: Vec::new(),
            percent_bps: None,
            message: None,
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactRemuxSucceeded(ArtifactRemuxSucceededPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            artifact_handle_id: 1,
            artifact_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_streams: Vec::new(),
            default_streams: Vec::new(),
            clear_default_streams: Vec::new(),
            kept_snapshot_stream_ids: Vec::new(),
            default_snapshot_stream_ids: Vec::new(),
            output_container: "mkv".to_owned(),
            provider: "ffmpeg".to_owned(),
            provider_version: "1".to_owned(),
        }),
        Event::ArtifactRemuxFailed(ArtifactRemuxFailedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: None,
            artifact_handle_id: None,
            artifact_location_id: None,
            staging_path: None,
            selected_streams: Vec::new(),
            default_streams: Vec::new(),
            clear_default_streams: Vec::new(),
            failure_class: FailureClass::WorkerCrash,
            error_code: "REMUX_FAILED".to_owned(),
            message: "m".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactAudioTranscodeStarted(ArtifactAudioTranscodeStartedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            source_media_snapshot_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_streams: Vec::new(),
            target_codec: "aac".to_owned(),
            output_container: "mka".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactAudioTranscodeProgress(ArtifactAudioTranscodeProgressPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            source_media_snapshot_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_streams: Vec::new(),
            percent_bps: None,
            message: None,
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactAudioTranscodeSucceeded(ArtifactAudioTranscodeSucceededPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            source_media_snapshot_id: 1,
            artifact_handle_id: 1,
            artifact_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_streams: Vec::new(),
            selected_snapshot_stream_ids: Vec::new(),
            selected_output_streams: Vec::new(),
            output_container: "mka".to_owned(),
            output_audio_codecs: Vec::new(),
            provider: "ffmpeg".to_owned(),
            provider_version: "1".to_owned(),
        }),
        Event::ArtifactAudioTranscodeFailed(ArtifactAudioTranscodeFailedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: None,
            source_media_snapshot_id: None,
            artifact_handle_id: None,
            artifact_location_id: None,
            staging_path: None,
            selected_streams: Vec::new(),
            selected_output_streams: Vec::new(),
            failure_class: FailureClass::WorkerCrash,
            error_code: "AUDIO_TRANSCODE_FAILED".to_owned(),
            message: "m".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactAudioExtractStarted(ArtifactAudioExtractStartedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            source_media_snapshot_id: 1,
            source_bundle_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_stream: ArtifactAudioStreamPayload {
                snapshot_stream_id: "s1".to_owned(),
                provider_stream_index: 1,
            },
            role: "primary".to_owned(),
            target_codec: "aac".to_owned(),
            output_container: "mka".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactAudioExtractProgress(ArtifactAudioExtractProgressPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            source_media_snapshot_id: 1,
            source_bundle_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_stream: ArtifactAudioStreamPayload {
                snapshot_stream_id: "s1".to_owned(),
                provider_stream_index: 1,
            },
            percent_bps: None,
            message: None,
            provider: None,
            provider_version: None,
        }),
        Event::ArtifactAudioExtractSucceeded(ArtifactAudioExtractSucceededPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: 1,
            source_media_snapshot_id: 1,
            source_bundle_id: 1,
            artifact_handle_id: 1,
            artifact_location_id: 1,
            staging_path: "/staging/x".to_owned(),
            selected_stream: ArtifactAudioStreamPayload {
                snapshot_stream_id: "s1".to_owned(),
                provider_stream_index: 1,
            },
            selected_snapshot_stream_id: "s1".to_owned(),
            role: "primary".to_owned(),
            output_container: "mka".to_owned(),
            output_audio_codec: "aac".to_owned(),
            provider: "ffmpeg".to_owned(),
            provider_version: "1".to_owned(),
        }),
        Event::ArtifactAudioExtractFailed(ArtifactAudioExtractFailedPayload {
            job_id: 1,
            ticket_id: 1,
            lease_id: None,
            source_file_version_id: 1,
            source_file_location_id: None,
            source_media_snapshot_id: None,
            source_bundle_id: 1,
            artifact_handle_id: None,
            artifact_location_id: None,
            staging_path: None,
            selected_stream: None,
            role: None,
            failure_class: FailureClass::WorkerCrash,
            error_code: "AUDIO_EXTRACT_FAILED".to_owned(),
            message: "m".to_owned(),
            provider: None,
            provider_version: None,
        }),
        Event::IssueOpened(issue_payload("planned")),
        Event::IssueUpdated(issue_payload("open")),
        Event::IssueResolved(issue_payload("resolved")),
        Event::MediaWorkCreated(MediaWorkCreatedPayload {
            media_work_id: 1,
            kind: "movie".to_owned(),
            display_title: "x".to_owned(),
            provisional: false,
        }),
        Event::MediaVariantCreated(MediaVariantCreatedPayload {
            media_variant_id: 1,
            media_work_id: 1,
            label: "x".to_owned(),
            provisional: false,
        }),
        Event::AssetBundleCreated(AssetBundleCreatedPayload {
            bundle_id: 1,
            media_variant_id: 1,
            display_name: "x".to_owned(),
        }),
        Event::AssetBundleMemberAdded(AssetBundleMemberAddedPayload {
            bundle_id: 1,
            file_asset_id: 1,
            role: "primary".to_owned(),
        }),
        Event::AssetBundleMemberRemoved(AssetBundleMemberRemovedPayload {
            bundle_id: 1,
            file_asset_id: 1,
            role: "primary".to_owned(),
        }),
        Event::FileAssetCreated(FileAssetCreatedPayload { file_asset_id: 1 }),
        Event::FileVersionCreated(FileVersionCreatedPayload {
            file_version_id: 1,
            file_asset_id: 1,
            content_hash: "blake3:1".to_owned(),
            size_bytes: 1,
            produced_by: "x".to_owned(),
            produced_from_version_id: None,
        }),
        Event::FileLocationRecorded(FileLocationRecordedPayload {
            file_location_id: 1,
            file_version_id: 1,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
        }),
        Event::FileLocationAliased(FileLocationAliasedPayload {
            file_location_id: 1,
            file_version_id: 1,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
        }),
        Event::FileLocationRetiredByMove(FileLocationRetiredByMovePayload {
            file_location_id: 1,
            file_version_id: 1,
            retired_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::FileLocationRecordedByMove(FileLocationRecordedByMovePayload {
            retired_file_location_id: 1,
            new_file_location_id: 2,
            file_version_id: 1,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload {
            evidence_id: 1,
            target_type: "file_version".to_owned(),
            target_id: 1,
            assertion_type: "hash".to_owned(),
            provider: "x".to_owned(),
            provider_version: "1".to_owned(),
            confidence: 1.0,
            observed_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload {
            evidence_id: 1,
            target_type: "file_version".to_owned(),
            target_id: 1,
            accepted_user_id: None,
            accepted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::IdentityEvidenceSuperseded(IdentityEvidenceSupersededPayload {
            superseded_evidence_id: 1,
            superseded_by_evidence_id: 2,
            target_type: "file_version".to_owned(),
            target_id: 1,
            superseded_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::MediaSnapshotRecorded(MediaSnapshotRecordedPayload {
            media_snapshot_id: 1,
            file_version_id: 1,
            probed_by_worker_id: None,
            probed_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::UseLeaseAcquired(UseLeaseAcquiredPayload {
            lease_id: 1,
            kind: "playback".to_owned(),
            scope_type: "asset".to_owned(),
            scope_id: 1,
            issuer_kind: "user".to_owned(),
            issuer_ref: "u1".to_owned(),
            blocking_mode: "blocking".to_owned(),
            ttl_bound: false,
            acquired_at: OffsetDateTime::UNIX_EPOCH,
            expires_at: None,
        }),
        Event::UseLeaseReleased(UseLeaseReleasedPayload {
            lease_id: 1,
            release_reason: "released".to_owned(),
            released_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::UseLeaseExpired(UseLeaseExpiredPayload {
            lease_id: 1,
            released_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::UseLeaseForceReleased(UseLeaseForceReleasedPayload {
            lease_id: 1,
            actor: "a".to_owned(),
            reason: "r".to_owned(),
            released_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::UseLeaseRecoveredStaleIssuer(UseLeaseRecoveredStaleIssuerPayload {
            lease_id: 1,
            actor: "a".to_owned(),
            reason: "r".to_owned(),
            released_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::UseLeaseReanchoredByMove(UseLeaseReanchoredByMovePayload {
            lease_id: 1,
            retired_location_id: 1,
            new_location_id: 2,
            reanchored_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitIntentRecorded(CommitIntentRecordedPayload {
            commit_id: voom_core::CommitId(1),
            target_kind: "delete_file_location".to_owned(),
            closure_asset_count: 1,
            closure_bundle_count: 1,
            closure_version_count: 1,
            closure_location_count: 1,
            accepted_evidence_count: 1,
            started_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedByUseLease(CommitAbortedByUseLeasePayload {
            commit_id: voom_core::CommitId(1),
            lease_id: voom_core::UseLeaseId(1),
            lease_scope_type: "asset".to_owned(),
            lease_scope_id: 1,
            phase: "prepare".to_owned(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload {
            commit_id: voom_core::CommitId(1),
            evidence_id: voom_core::EvidenceId(1),
            drift_kind: "pinned_hash_differs".to_owned(),
            phase: "prepare".to_owned(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload {
            commit_id: voom_core::CommitId(1),
            phase: "prepare".to_owned(),
            message: "m".to_owned(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedByPendingCommit(CommitAbortedByPendingCommitPayload {
            commit_id: voom_core::CommitId(1),
            pending_commit_id: voom_core::CommitId(2),
            scope_type: "asset".to_owned(),
            scope_id: 1,
            phase: "prepare".to_owned(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAuthorized(CommitAuthorizedPayload {
            commit_id: voom_core::CommitId(1),
            closure_asset_count: 1,
            closure_bundle_count: 1,
            closure_version_count: 1,
            closure_location_count: 1,
            target_row_epoch_count: 1,
            authorized_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedByClosureGrew(CommitAbortedByClosureGrewPayload {
            commit_id: voom_core::CommitId(1),
            added_asset_count: 1,
            added_bundle_count: 0,
            added_version_count: 0,
            added_location_count: 0,
            removed_asset_count: 0,
            removed_bundle_count: 0,
            removed_version_count: 0,
            removed_location_count: 0,
            phase: "authorize".to_owned(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitCompleted(CommitCompletedPayload {
            commit_id: voom_core::CommitId(1),
            target_kind: "delete_file_location".to_owned(),
            closure_asset_count: 1,
            closure_bundle_count: 1,
            closure_version_count: 1,
            closure_location_count: 1,
            finalized_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedPreMutation(CommitAbortedPreMutationPayload {
            commit_id: voom_core::CommitId(1),
            prior_state: "pending".to_owned(),
            reason: "operator_cancel".to_owned(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitAbortedPostMutation(CommitAbortedPostMutationPayload {
            commit_id: voom_core::CommitId(1),
            reason: "closure_grew".to_owned(),
            added_asset_count: 1,
            added_bundle_count: 0,
            added_version_count: 0,
            added_location_count: 0,
            removed_asset_count: 0,
            removed_bundle_count: 0,
            removed_version_count: 0,
            removed_location_count: 0,
            fresh_lease_ids: Vec::new(),
            target_epoch_drift: Vec::new(),
            aborted_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitRecoveryRequired(CommitRecoveryRequiredPayload {
            commit_id: voom_core::CommitId(1),
            recovery_reason: "closure_grew".to_owned(),
            added_asset_count: 1,
            added_bundle_count: 0,
            added_version_count: 0,
            added_location_count: 0,
            removed_asset_count: 0,
            removed_bundle_count: 0,
            removed_version_count: 0,
            removed_location_count: 0,
            fresh_lease_ids: Vec::new(),
            target_epoch_drift: Vec::new(),
            recorded_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::CommitForcedOverride(CommitForcedOverridePayload {
            commit_id: voom_core::CommitId(1),
            actor: "a".to_owned(),
            reason: "r".to_owned(),
            bypass: Vec::new(),
            recorded_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::ExternalSystemRegistered(ExternalSystemRegisteredPayload {
            external_system_id: 1,
            kind: "filesystem".to_owned(),
            display_name: "local".to_owned(),
            health_status: "unknown".to_owned(),
        }),
        Event::ExternalSystemHealthChanged(ExternalSystemHealthChangedPayload {
            external_system_id: 1,
            previous: "unknown".to_owned(),
            current: "healthy".to_owned(),
        }),
        Event::ExternalSystemLinked(ExternalSystemLinkedPayload {
            external_system_id: 1,
            link_id: 1,
            target_type: "media_work".to_owned(),
            target_id: 1,
            external_ref: "ref".to_owned(),
        }),
        Event::ExternalSystemUnlinked(ExternalSystemUnlinkedPayload {
            external_system_id: 1,
            link_id: 1,
            target_type: "media_work".to_owned(),
            target_id: 1,
            external_ref: "ref".to_owned(),
        }),
        Event::ExternalSystemSynced(ExternalSystemSyncedPayload {
            external_system_id: 1,
            outcome: "ok".to_owned(),
            links_recorded: 0,
            links_retired: 0,
            started_at: OffsetDateTime::UNIX_EPOCH,
            finished_at: OffsetDateTime::UNIX_EPOCH,
        }),
    ];

    for event in events {
        let json = serde_json::to_value(&event).expect("event serializes");
        let tag = json
            .as_object()
            .expect("event is JSON object")
            .get("kind")
            .expect("serialized event has kind tag")
            .as_str()
            .expect("kind tag is string");
        assert_eq!(
            tag,
            event.kind().as_str(),
            "serde tag drift for variant {event:?}"
        );
    }
}
