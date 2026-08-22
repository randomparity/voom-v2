use super::*;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use time::OffsetDateTime;
use voom_core::ids::{ArtifactCommitIntentId, ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, ErrorCode, FailureClass, FileLocationId, FileVersionId, NodeId,
    StorageRootId, VoomError, rng_test_support::FrozenRng,
};
use voom_events::EventKind;
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page};
use voom_store::repo::media::artifacts::{
    ArtifactCommitFailure, ArtifactCommitState, ArtifactLocationKind, NewArtifactCommitRecord,
    NewArtifactLocation,
};
use voom_store::repo::media::identity::{
    DiscoveredFile, FileLocationRepo, FileVersionRepo, IngestOutcome, NewFileLocation,
    NewFileVersion, ProducedBy,
};

use crate::ControlPlane;
use crate::artifact::stage::{StageCopyInput, StageCopyReport};
use crate::artifact::verify::{
    NoVerifyArtifactHooks, VerifyArtifactDispatcher, VerifyArtifactInput,
    verify_artifact_with_dispatcher,
};
use voom_worker_protocol::{
    VerifyArtifactObservedFacts, VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

use secrecy::ExposeSecret;
use voom_test_support::commit_node::SimulatedOwnerNode;

use crate::artifact::commit::intent::{
    AppliedEvidence, CommitOutcomeEvidence, MismatchedEvidence, OutcomeUnknownEvidence,
    RESOLVED_NOT_APPLIED_REASON,
};

#[tokio::test]
async fn unverified_commit_is_rejected_before_pending_record() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_eq!(err.pre_mutation_report().unwrap().verification_id, None);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
    assert!(!target.exists());
}

#[tokio::test]
async fn stale_verification_for_retired_or_different_staging_location_is_rejected() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    cp.artifacts()
        .retire_location(staged.artifact_location_id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let replacement_staging = dir.path().join("replacement-staged.bin");
    std::fs::write(&replacement_staging, b"source bytes").unwrap();
    cp.artifacts()
        .record_location(NewArtifactLocation {
            artifact_handle_id: staged.artifact_handle_id,
            kind: ArtifactLocationKind::Staging,
            value: replacement_staging.display().to_string(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: dir.path().join("target.bin"),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
}

#[tokio::test]
async fn staged_byte_drift_is_detected_by_the_node_and_requires_recovery() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let _driver = crate::artifact::commit::commit_test_support::spawn_auto_driver(&cp, &node);
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    std::fs::write(&staged.staging_path, b"changed bytes").unwrap();
    let target = dir.path().join("target.bin");

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap_err();

    // The control plane is byte-blind at prepare (ADR 0074): the drift is
    // caught by the node's staging re-observation and journaled as a
    // mismatched receipt.
    assert_eq!(err.code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(!target.exists());
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::RecoveryRequired);
}

#[tokio::test]
async fn existing_target_is_rejected_before_pending_record() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    std::fs::write(&target, b"already here").unwrap();

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
    assert_eq!(std::fs::read(&target).unwrap(), b"already here");
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitFailedPreMutation).await,
        1
    );
}

#[tokio::test]
async fn conflicting_target_is_reported_mismatched_and_requires_recovery() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    // The node finds a concurrent writer's bytes already at the target,
    // reports typed mismatch evidence, and stops before completing.
    let task_cp = cp.clone();
    let task_target = target.clone();
    let driver = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: staged.artifact_handle_id,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    node_authorize(&cp, &node, intent_id).await.unwrap();
    node_report_applying(&cp, &node, intent_id).await.unwrap();
    std::fs::write(&target, b"concurrent writer").unwrap();
    let conflicting = std::fs::read(&target).unwrap();
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
            reason: "target already exists with different bytes".to_owned(),
            observed: Some(voom_test_support::commit_node::observed_facts(&conflicting)),
        }),
    )
    .await
    .unwrap();

    let err = driver.await.unwrap().unwrap_err();
    assert_eq!(err.code(), ErrorCode::ArtifactChecksumMismatch);
    assert_eq!(std::fs::read(&target).unwrap(), b"concurrent writer");
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::RecoveryRequired);
    assert_eq!(report.result_file_version_id, None);
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitRecoveryRequired).await,
        1
    );

    // Mismatched evidence is unresolved: recovery requires an operator.
    let recovery_error = cp
        .recover_commit(staged.artifact_handle_id)
        .await
        .unwrap_err();
    assert_eq!(recovery_error.error_code(), ErrorCode::Conflict);
    assert_eq!(
        record_state(&cp, report.commit_record_id).await,
        "recovery_required"
    );
}

#[tokio::test]
async fn successful_commit_promotes_target_records_identity_retires_staging_and_emits_events() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let report = commit_with_node(
        &cp,
        &node,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"source bytes");
    assert_eq!(report.artifact_handle_id, staged.artifact_handle_id);
    assert_eq!(report.state, ArtifactCommitState::Committed);
    assert_eq!(report.target_path, target.canonicalize().unwrap());
    assert_eq!(report.recovery_required, None);
    let result_version_id = report.result_file_version_id.unwrap();
    let result_location_id = report.result_file_location_id.unwrap();

    let version = cp
        .identity()
        .get_file_version(result_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.produced_by, ProducedBy::StagedCommit);
    assert_eq!(
        version.produced_from_version_id,
        Some(staged.source_file_version_id)
    );
    assert_eq!(version.content_hash, blake3_checksum(b"source bytes"));
    assert_eq!(version.size_bytes, 12);

    let location = cp
        .identity()
        .get_file_location(result_location_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(location.file_version_id, result_version_id);
    assert_eq!(
        location.rooted_address().unwrap(),
        (
            voom_store::test_support::TEST_STORAGE_ROOT_ID,
            &voom_store::test_support::test_relative_locator(
                &target.canonicalize().unwrap().display().to_string()
            )
        )
    );

    let locations = cp
        .artifacts()
        .list_locations_for_handle(staged.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(locations.len(), 0);
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM artifact_locations WHERE id = ?")
            .bind(i64::try_from(staged.artifact_location_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert!(retired_at.is_some());

    let records = cp
        .artifacts()
        .list_commit_records(staged.artifact_handle_id)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, report.commit_record_id);
    assert_eq!(records[0].state, ArtifactCommitState::Committed);
    assert_eq!(records[0].result_file_version_id, Some(result_version_id));
    assert_eq!(records[0].result_file_location_id, Some(result_location_id));

    assert_eq!(count_events(&cp, EventKind::ArtifactCommitStarted).await, 1);
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitCompleted).await,
        1
    );
}

#[tokio::test]
async fn commit_accepts_relative_provider_locator_for_rooted_target() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let current_dir = std::env::current_dir().unwrap();
    let relative_root = dir.path().strip_prefix(&current_dir).unwrap();
    let relative_root_id = StorageRootId(9_000_002);
    insert_active_test_root(&cp, relative_root_id, relative_root).await;
    set_test_default_output_root(&cp, relative_root_id).await;
    let target = dir.path().join("relative-root-target.bin");

    let report = commit_with_node(
        &cp,
        &node,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
    )
    .await
    .unwrap();

    assert_eq!(report.state, ArtifactCommitState::Committed);
    assert_eq!(std::fs::read(target).unwrap(), b"source bytes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_independent_commits_all_complete() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let cp = std::sync::Arc::new(cp);
    let inputs = [
        b"concurrent source a".as_slice(),
        b"concurrent source b".as_slice(),
        b"concurrent source c".as_slice(),
        b"concurrent source d".as_slice(),
        b"concurrent source e".as_slice(),
        b"concurrent source f".as_slice(),
    ];
    let mut commits = Vec::new();
    for (index, bytes) in inputs.iter().enumerate() {
        let staged = stage_and_verify_bytes(&cp, dir.path(), bytes).await;
        commits.push((
            staged.artifact_handle_id,
            dir.path().join(format!("concurrent-target-{index}.bin")),
        ));
    }

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(commits.len()));
    let mut tasks = tokio::task::JoinSet::new();
    for (artifact_handle_id, target_path) in commits {
        let cp = cp.clone();
        let node = node.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            let driver_cp = cp.clone();
            let driver_node = node.clone();
            let driver = tokio::spawn(async move {
                drive_pending_commit_local(&driver_cp, &driver_node, artifact_handle_id).await
            });
            let outcome = cp
                .commit_artifact(CommitArtifactInput {
                    artifact_handle_id,
                    target_path,
                })
                .await;
            driver.await.unwrap().unwrap();
            outcome
        });
    }

    let mut reports = Vec::new();
    while let Some(result) = tasks.join_next().await {
        reports.push(result.unwrap().unwrap());
    }
    assert_eq!(reports.len(), inputs.len());
    assert!(
        reports
            .iter()
            .all(|report| report.state == ArtifactCommitState::Committed)
    );
}

#[tokio::test]
async fn injected_failure_after_prepare_terminates_cleanly_without_recovery() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = commit_artifact_with_hooks(
        &cp,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        },
        &FailAfterPrepare,
    )
    .await
    .unwrap_err();

    // Nothing has mutated and the intent is still pending, so the hook
    // failure terminates both rows cleanly instead of requiring recovery.
    assert_eq!(err.code(), ErrorCode::CommitFailure);
    assert!(!target.exists());
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::Failed);
    assert_eq!(report.recovery_required, None);
}

#[tokio::test]
async fn staged_drift_is_reported_mismatched_without_promotion() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let task_cp = cp.clone();
    let task_target = target.clone();
    let driver = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: staged.artifact_handle_id,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;

    // The staged bytes drift after prepare; the node observes the pinned
    // facts no longer hold and reports mismatched without promoting.
    std::fs::write(&staged.staging_path, b"mutated staging bytes").unwrap();
    node_authorize(&cp, &node, intent_id).await.unwrap();
    node_report_applying(&cp, &node, intent_id).await.unwrap();
    let drifted = std::fs::read(&staged.staging_path).unwrap();
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
            reason: "staged bytes do not match the pinned expected facts".to_owned(),
            observed: Some(voom_test_support::commit_node::observed_facts(&drifted)),
        }),
    )
    .await
    .unwrap();

    let err = driver.await.unwrap().unwrap_err();
    assert_eq!(err.code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(!target.exists());
    let report = err.commit_report().unwrap();
    assert_eq!(report.state, ArtifactCommitState::RecoveryRequired);
}

#[tokio::test]
async fn recover_commit_finalizes_directly_from_matching_applied_receipt() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    // The node promoted matching bytes but crashed before completing: the
    // applied receipt survives and recovery finalizes from it.
    let task_cp = cp.clone();
    let task_target = target.clone();
    let driver = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: staged.artifact_handle_id,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    drive_to_applied_not_completed(&cp, &node, intent_id, &target, &staged.staging_path).await;
    driver.abort();

    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();

    assert_eq!(report.state, ArtifactCommitState::Committed);
    assert!(report.result_file_version_id.is_some());
    assert_eq!(std::fs::read(&target).unwrap(), b"source bytes");
    // The single owner record was finalized, not duplicated.
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );
    assert_eq!(
        count_events(&cp, EventKind::ArtifactCommitCompleted).await,
        1
    );
}

#[tokio::test]
async fn recovery_uses_prepared_rooted_target_after_default_changes() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let overlap = dir.path().join("overlap");
    std::fs::create_dir(&overlap).unwrap();
    let prepared_root_id = StorageRootId(9_000_002);
    let replacement_root_id = StorageRootId(9_000_003);
    insert_active_test_root(&cp, prepared_root_id, dir.path()).await;
    insert_active_test_root(&cp, replacement_root_id, &overlap).await;
    set_test_default_output_root(&cp, prepared_root_id).await;

    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = overlap.join("target.bin");
    spawn_and_drive_to_applied_not_completed(&cp, &node, staged.artifact_handle_id, &target).await;

    // The default output root changed after prepare; recovery must finalize
    // into the pinned target root, not the new default.
    set_test_default_output_root(&cp, replacement_root_id).await;
    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();
    let location_id = report.result_file_location_id.unwrap();
    let stored_root_id: i64 =
        sqlx::query_scalar("SELECT storage_root_id FROM file_locations WHERE id = ?")
            .bind(i64::try_from(location_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();

    assert_eq!(report.state, ArtifactCommitState::Committed);
    assert_eq!(u64::try_from(stored_root_id).unwrap(), prepared_root_id.0);
    assert_eq!(std::fs::read(target).unwrap(), b"source bytes");
}

#[tokio::test]
async fn recover_commit_aborts_receiptless_authorized_and_reprepares() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    // Authorized but receipt-less: the node never mutated, so recovery may
    // safely abort and prepare a fresh successor generation.
    let original_record_id =
        spawn_and_drive_authorize_only(&cp, &node, staged.artifact_handle_id, &target).await;
    assert!(!target.exists());

    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();

    assert_eq!(report.state, ArtifactCommitState::Pending);
    assert_ne!(report.commit_record_id, original_record_id);
    assert!(!target.exists());
}

#[tokio::test]
async fn recovery_abort_fails_closed_when_a_receipt_lands_after_classification() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    // Classification snapshot: authorized, receipt-less, at its current epoch.
    let task_cp = cp.clone();
    let task_target = target.clone();
    let driver = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: staged.artifact_handle_id,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    node_authorize(&cp, &node, intent_id).await.unwrap();
    let record = cp
        .artifacts
        .list_commit_records(staged.artifact_handle_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let classified = cp
        .artifact_commit_intents
        .require_intent(intent_id)
        .await
        .unwrap();

    // A node journals its mutation gate AFTER the classification snapshot:
    // the abort must fail closed instead of overriding the live journal.
    node_report_applying(&cp, &node, intent_id).await.unwrap();

    let error = super::recovery::abort_and_reprepare_report(
        &cp,
        &record,
        &classified,
        classified.intent_epoch,
    )
    .await
    .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::Conflict);
    assert!(
        error
            .to_string()
            .contains("changed under recovery classification")
    );
    driver.abort();
}

#[tokio::test]
async fn recover_commit_requires_operator_when_target_already_exists() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    // Authorized but receipt-less, yet the target already exists (a crashed
    // node wrote it before journaling): the fresh successor prepare fails
    // closed instead of clobbering the occupying file.
    spawn_and_drive_authorize_only(&cp, &node, staged.artifact_handle_id, &target).await;
    std::fs::write(&target, b"occupying bytes").unwrap();

    let err = cp
        .recover_commit(staged.artifact_handle_id)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::CommitFailure);
    assert_eq!(std::fs::read(&target).unwrap(), b"occupying bytes");
}

#[tokio::test]
async fn duplicate_pending_committed_and_recovery_owners_are_rejected_by_repo_constraints() {
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let verification_id = cp
        .artifacts()
        .list_verifications(staged.artifact_handle_id)
        .await
        .unwrap()[0]
        .id;
    let target_a = dir.path().join("target-a.bin").display().to_string();
    let target_b = dir.path().join("target-b.bin").display().to_string();

    let pending = create_pending_commit(&cp, &staged, verification_id, &target_a).await;
    let duplicate_pending = create_pending_commit_result(&cp, &staged, verification_id, &target_b)
        .await
        .unwrap_err();
    assert_eq!(duplicate_pending.error_code(), ErrorCode::Conflict);

    mark_pending_committed(&cp, pending.id, &staged, &target_a).await;
    let duplicate_committed =
        create_pending_commit_result(&cp, &staged, verification_id, &target_b)
            .await
            .unwrap_err();
    assert_eq!(duplicate_committed.error_code(), ErrorCode::Conflict);

    let second = stage_and_verify_bytes(&cp, dir.path(), b"second bytes").await;
    let second_verification_id = cp
        .artifacts()
        .list_verifications(second.artifact_handle_id)
        .await
        .unwrap()[0]
        .id;
    let recovery = create_pending_commit(
        &cp,
        &second,
        second_verification_id,
        &dir.path().join("target-c.bin").display().to_string(),
    )
    .await;
    mark_pending_recovery(&cp, recovery.id).await;
    let duplicate_recovery = create_pending_commit_result(
        &cp,
        &second,
        second_verification_id,
        &dir.path().join("target-d.bin").display().to_string(),
    )
    .await
    .unwrap_err();
    assert_eq!(duplicate_recovery.error_code(), ErrorCode::Conflict);
}

#[tokio::test]
async fn blocking_use_lease_blocks_prepare_before_pending_record() {
    use voom_store::repo::media::use_leases::{
        BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind,
    };
    let (cp, _db, dir) = fixture().await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;

    // A blocking lease on the affected scope is refused at prepare, before
    // any durable record exists.
    cp.use_leases()
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(staged.source_file_version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "watcher".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(time::Duration::seconds(3600)),
            acquired_at: OffsetDateTime::now_utc(),
        })
        .await
        .unwrap();

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: dir.path().join("target.bin"),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::BlockedByUseLease);
    assert_no_commit_records(&cp, staged.artifact_handle_id).await;
}

#[tokio::test]
async fn authorize_rejects_out_of_band_blocking_lease_and_aborts_intent() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let intent_id =
        spawn_and_wait_pending_intent(&cp, staged.artifact_handle_id, dir.path().join("t.bin"))
            .await;

    // A lease that appears after prepare can only come from out of band
    // (another plane instance); simulate it at the storage layer. The
    // authorize-time gate re-run must still fail closed.
    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_version_id, issuer_kind, issuer_ref, blocking_mode, ttl_bound, \
          acquired_at, expires_at, clock_source) \
         VALUES ('playback', ?, 'user', 'out-of-band', 'blocking', 1, \
                 '1970-01-01T00:00:00Z', '9999-01-01T00:00:00Z', 'control_plane')",
    )
    .bind(i64::try_from(staged.source_file_version_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let err = node_authorize(&cp, &node, intent_id).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::BlockedByUseLease);
    assert_eq!(intent_state(&cp, intent_id).await, "aborted");
}

#[tokio::test]
async fn authorize_rejects_wrong_node_and_aborts_intent() {
    let (cp, _db, dir) = fixture().await;
    let wrong = install_second_remote_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let intent_id =
        spawn_and_wait_pending_intent(&cp, staged.artifact_handle_id, dir.path().join("t.bin"))
            .await;

    let err = node_authorize(&cp, &wrong, intent_id).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    // Drift aborts the still-pending intent fail-closed.
    assert_eq!(intent_state(&cp, intent_id).await, "aborted");
}

#[tokio::test]
async fn authorize_rejects_staging_location_epoch_bump_and_aborts_intent() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let intent_id =
        spawn_and_wait_pending_intent(&cp, staged.artifact_handle_id, dir.path().join("t.bin"))
            .await;

    sqlx::query(
        "UPDATE file_locations SET epoch = epoch + 1 WHERE id = \
                 (SELECT staging_location_id FROM artifact_commit_intents WHERE id = ?)",
    )
    .bind(i64::try_from(intent_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let err = node_authorize(&cp, &node, intent_id).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(intent_state(&cp, intent_id).await, "aborted");
}

#[tokio::test]
async fn authorize_rejects_root_reassignment_and_aborts_intent() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let other = install_second_remote_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let intent_id =
        spawn_and_wait_pending_intent(&cp, staged.artifact_handle_id, dir.path().join("t.bin"))
            .await;

    sqlx::query("UPDATE library_roots SET owner_node_id = ? WHERE id = 9000001")
        .bind(i64::try_from(other.node_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let err = node_authorize(&cp, &node, intent_id).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(intent_state(&cp, intent_id).await, "aborted");
}

#[tokio::test]
async fn authorize_rejects_stale_incarnation_without_touching_intent() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let stale = SimulatedOwnerNode {
        incarnation_id: "fedcba9876543210fedcba9876543210".parse().unwrap(),
        ..node.clone()
    };
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let intent_id =
        spawn_and_wait_pending_intent(&cp, staged.artifact_handle_id, dir.path().join("t.bin"))
            .await;

    // The incarnation fence fails before any reservation: unauthenticated
    // callers never mutate the intent.
    let err = node_authorize(&cp, &stale, intent_id).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(intent_state(&cp, intent_id).await, "pending");
}

// --- open intent listing (node pull) ------------------------------------------

#[expect(
    clippy::too_many_lines,
    reason = "the open-listing lifecycle drive reads linearly; splitting scatters the sequence"
)]
#[tokio::test]
async fn remote_open_commit_intents_lists_caller_owned_open_intents_only() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let handle = staged.artifact_handle_id;
    let target = dir.path().join("target.bin");

    // Prepare one fenced intent and leave it undriven until the listing
    // assertions below have run.
    let task_cp = cp.clone();
    let task_target = target.clone();
    let driver = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: handle,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&cp, handle).await;

    let open_input = crate::artifact::commit::intent::RemoteCommitIntentsOpenInput {
        node_id: node.node_id,
        token: node.token.clone(),
        incarnation_id: node.incarnation_id,
    };
    let listing = cp
        .remote_open_commit_intents(open_input.clone())
        .await
        .unwrap();
    assert_eq!(listing.intents.len(), 1);
    assert_eq!(listing.intents[0].id, intent_id);
    assert_eq!(listing.intents[0].state, "pending");
    assert_eq!(listing.intents[0].artifact_handle_id, handle);
    // The fence value never travels in the listing projection.
    let wire = serde_json::to_string(&listing).unwrap();
    assert!(
        !wire.contains("fence"),
        "open listing leaked fence material: {wire}"
    );

    // A different authenticated remote node owns no roots and sees nothing.
    let other = install_second_remote_node(&cp).await;
    let other_listing = cp
        .remote_open_commit_intents(
            crate::artifact::commit::intent::RemoteCommitIntentsOpenInput {
                node_id: other.node_id,
                token: other.token.clone(),
                incarnation_id: other.incarnation_id,
            },
        )
        .await
        .unwrap();
    assert!(other_listing.intents.is_empty());

    // Bad credentials are rejected before any listing work happens.
    let unauthorized = cp
        .remote_open_commit_intents(
            crate::artifact::commit::intent::RemoteCommitIntentsOpenInput {
                token: secrecy::SecretString::from("not-the-token".to_owned()),
                ..open_input.clone()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(unauthorized.code(), ErrorCode::Unauthorized.as_str());

    // After authorization the listing reflects the new state and the same
    // pinned scope the fenced outcome carries.
    let authorized = node_authorize(&cp, &node, intent_id).await.unwrap();
    let listing = cp
        .remote_open_commit_intents(open_input.clone())
        .await
        .unwrap();
    assert_eq!(listing.intents.len(), 1);
    assert_eq!(listing.intents[0].state, "authorized");
    assert_eq!(
        listing.intents[0].staging_storage_root_id,
        authorized.staging_storage_root_id
    );
    assert_eq!(
        listing.intents[0].staging_provider_relative_locator,
        authorized.staging_provider_relative_locator
    );
    assert_eq!(
        listing.intents[0].target_storage_root_id,
        authorized.target_storage_root_id
    );
    assert_eq!(
        listing.intents[0].target_provider_relative_locator,
        authorized.target_provider_relative_locator
    );
    assert_eq!(
        listing.intents[0].expected_facts.size_bytes,
        authorized.expected_size_bytes
    );
    assert_eq!(
        listing.intents[0].expected_facts.content_hash,
        authorized.expected_content_hash
    );

    // Converge the intent; completed intents drop out of the listing.
    node_report_applying(&cp, &node, intent_id).await.unwrap();
    let staged_path = rooted_path(
        &cp,
        authorized.staging_storage_root_id.0,
        &authorized.staging_provider_relative_locator,
    )
    .await;
    let bytes = std::fs::read(&staged_path).unwrap();
    std::fs::copy(&staged_path, &target).unwrap();
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap();
    node_complete(&cp, &node, intent_id, &authorized.fence_hex)
        .await
        .unwrap();
    let final_listing = cp.remote_open_commit_intents(open_input).await.unwrap();
    assert!(final_listing.intents.is_empty());

    // The waiting prepare leg sees the converged committed record.
    let report = driver.await.unwrap().unwrap();
    assert_eq!(report.state, ArtifactCommitState::Committed);
}
// --- receipt ordering and fence ----------------------------------------------

#[tokio::test]
async fn applied_receipt_before_applying_journal_is_rejected() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    node_authorize(&cp, &node, intent_id).await.unwrap();

    // The applying journal is the sole mutation gate: an outcome receipt
    // without it is an ordering violation.
    let bytes = std::fs::read(&staged.staging_path).unwrap();
    let err = node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);

    node_report_applying(&cp, &node, intent_id).await.unwrap();
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap();
    assert_eq!(intent_receipt_kind(&cp, intent_id).await, "applied");
}

#[tokio::test]
async fn complete_rejects_fence_mismatch_then_accepts_exact_fence() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    let outcome = node_authorize(&cp, &node, intent_id).await.unwrap();
    node_report_applying(&cp, &node, intent_id).await.unwrap();
    std::fs::copy(&staged.staging_path, &target).unwrap();
    let bytes = std::fs::read(&staged.staging_path).unwrap();
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap();

    let wrong_fence = format!("{:064}", 0);
    let err = node_complete(&cp, &node, intent_id, &wrong_fence)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);

    let completed = node_complete(&cp, &node, intent_id, &outcome.fence_hex)
        .await
        .unwrap();
    assert_eq!(completed.intent_id, intent_id);
    assert_eq!(
        record_state(&cp, completed.commit_record_id).await,
        "committed"
    );
}
/// The incarnation-pin policy of ADR 0074 (issue #524): post-authorization
/// mutations revalidate the caller's token against its currently-active
/// incarnation plus the pinned epochs — never against the pinned
/// `owner_incarnation_id` — while the one-time fence stays bound to the
/// authorizing incarnation's replay slot. A storage owner that re-registered
/// under a fresh incarnation mid-promotion therefore resumes and converges
/// the intent instead of wedging it into operator-required recovery.
#[tokio::test]
async fn post_authorization_mutations_follow_the_active_incarnation_not_the_pin() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    let outcome = node_authorize(&cp, &node, intent_id).await.unwrap();

    // The node re-registers: a fresh incarnation becomes the active fence.
    let fresh = SimulatedOwnerNode::new().unwrap();
    // Re-registration supersedes the prior active incarnation, then points
    // the node's active-incarnation fence at the new row.
    sqlx::query(
        "UPDATE node_incarnations SET status = 'superseded', \
         ended_at = '1970-01-01T00:01:00Z', end_reason = 'superseded' \
         WHERE node_id = ? AND status = 'active'",
    )
    .bind(i64::try_from(node.node_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    fresh
        .install_for(cp.pool_for_test(), node.node_id)
        .await
        .unwrap();

    // A fresh authorize is refused fail-closed: the intent is no longer
    // pending, so no second fence can be minted for it.
    let err = node_authorize(&cp, &fresh, intent_id).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(intent_state(&cp, intent_id).await, "authorized");

    // Receipts from the fresh active incarnation are accepted.
    node_report_applying(&cp, &fresh, intent_id).await.unwrap();
    std::fs::copy(&staged.staging_path, &target).unwrap();
    let bytes = std::fs::read(&staged.staging_path).unwrap();
    node_report_outcome(
        &cp,
        &fresh,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap();

    // Completion with the exact fence converges rather than routing the
    // resumed promotion into recovery.
    let completed = node_complete(&cp, &fresh, intent_id, &outcome.fence_hex)
        .await
        .unwrap();
    assert_eq!(completed.intent_id, intent_id);
    assert_eq!(
        record_state(&cp, completed.commit_record_id).await,
        "committed"
    );
}

#[test]
fn authorize_outcome_debug_redacts_fence_hex() {
    let outcome = crate::artifact::commit::intent::AuthorizeCommitOutcome {
        intent_id: ArtifactCommitIntentId(1),
        commit_record_id: ArtifactCommitRecordId(1),
        staging_storage_root_id: StorageRootId(1),
        staging_provider_relative_locator: "staging/a.bin".to_owned(),
        target_storage_root_id: StorageRootId(1),
        target_provider_relative_locator: "committed/a.bin".to_owned(),
        expected_size_bytes: 1,
        expected_content_hash: "blake3:x".to_owned(),
        fence_hex: "deadbeef".to_owned(),
    };
    let rendered = format!("{outcome:?}");
    assert!(!rendered.contains("deadbeef"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
}

#[test]
fn complete_input_debug_redacts_fence_hex() {
    let input = crate::artifact::commit::intent::RemoteCommitCompleteInput {
        intent_id: ArtifactCommitIntentId(1),
        node_id: NodeId(1),
        token: secrecy::SecretString::from("node-token".to_owned()),
        incarnation_id: "fedcba9876543210fedcba9876543210".parse().unwrap(),
        idempotency_key: "key".to_owned(),
        request_hash: "hash".to_owned(),
        fence_hex: "deadbeef".to_owned(),
    };
    let rendered = format!("{input:?}");
    assert!(!rendered.contains("deadbeef"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
}

// --- idempotent replays (G3/G4) -----------------------------------------------

#[tokio::test]
async fn replayed_authorize_returns_identical_stored_outcome() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let intent_id =
        spawn_and_wait_pending_intent(&cp, staged.artifact_handle_id, dir.path().join("t.bin"))
            .await;

    let first = cp
        .remote_authorize_commit_intent(
            crate::artifact::commit::intent::RemoteCommitAuthorizeInput {
                intent_id,
                node_id: node.node_id,
                token: node.token.clone(),
                incarnation_id: node.incarnation_id,
                idempotency_key: "sim-authorize-replay".to_owned(),
                request_hash: "sim-authorize-replay-hash".to_owned(),
            },
        )
        .await
        .unwrap();
    let second = cp
        .remote_authorize_commit_intent(
            crate::artifact::commit::intent::RemoteCommitAuthorizeInput {
                intent_id,
                node_id: node.node_id,
                token: node.token.clone(),
                incarnation_id: node.incarnation_id,
                idempotency_key: "sim-authorize-replay".to_owned(),
                request_hash: "sim-authorize-replay-hash".to_owned(),
            },
        )
        .await
        .unwrap();

    assert_eq!(first, second);
    assert!(!second.fence_hex.is_empty());
}

#[tokio::test]
async fn replayed_complete_returns_identical_stored_outcome() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    let authorized = node_authorize(&cp, &node, intent_id).await.unwrap();
    node_report_applying(&cp, &node, intent_id).await.unwrap();
    std::fs::copy(&staged.staging_path, &target).unwrap();
    let bytes = std::fs::read(&staged.staging_path).unwrap();
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap();

    let complete_input = crate::artifact::commit::intent::RemoteCommitCompleteInput {
        intent_id,
        node_id: node.node_id,
        token: node.token.clone(),
        incarnation_id: node.incarnation_id,
        idempotency_key: "sim-complete-replay".to_owned(),
        request_hash: "sim-complete-replay-hash".to_owned(),
        fence_hex: authorized.fence_hex.clone(),
    };
    let first = cp
        .remote_complete_commit_intent(complete_input.clone())
        .await
        .unwrap();
    let second = cp
        .remote_complete_commit_intent(complete_input)
        .await
        .unwrap();
    assert_eq!(first, second);
    // The fence was consumed once; a second generation was never created.
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );
}

// --- recovery classification (spec step 7) -------------------------------------

#[tokio::test]
async fn recover_commit_redrives_after_supplemental_resolved_not_applied() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    drive_authorize_and_applying(&cp, &node, intent_id).await;
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::OutcomeUnknown(OutcomeUnknownEvidence {
            reason: "node crashed mid-promotion".to_owned(),
        }),
    )
    .await
    .unwrap();

    // The owner's read-only re-observation finds no target and no temp
    // sibling: positive evidence promotion never happened.
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::OutcomeUnknown(OutcomeUnknownEvidence {
            reason: RESOLVED_NOT_APPLIED_REASON.to_owned(),
        }),
    )
    .await
    .unwrap();

    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();

    assert_eq!(report.state, ArtifactCommitState::Pending);
    assert!(!target.exists());
    assert_eq!(intent_state(&cp, intent_id).await, "aborted");
}

#[tokio::test]
async fn recover_commit_requires_operator_for_mismatched_receipt() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    drive_authorize_and_applying(&cp, &node, intent_id).await;
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
            reason: "target exists with different bytes".to_owned(),
            observed: None,
        }),
    )
    .await
    .unwrap();

    let err = cp
        .recover_commit(staged.artifact_handle_id)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    // Operator-required evidence keeps the stuck rows in place.
    assert_eq!(intent_state(&cp, intent_id).await, "recovery_required");
}

#[tokio::test]
async fn recover_commit_requires_operator_for_unresolved_outcome_unknown() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    drive_authorize_and_applying(&cp, &node, intent_id).await;
    node_report_outcome(
        &cp,
        &node,
        intent_id,
        CommitOutcomeEvidence::OutcomeUnknown(OutcomeUnknownEvidence {
            reason: "node crashed mid-promotion".to_owned(),
        }),
    )
    .await
    .unwrap();

    let err = cp
        .recover_commit(staged.artifact_handle_id)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(intent_state(&cp, intent_id).await, "recovery_required");
}

#[tokio::test]
async fn recover_commit_without_non_terminal_commit_is_conflict() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    commit_with_node(
        &cp,
        &node,
        CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: dir.path().join("target.bin"),
        },
    )
    .await
    .unwrap();

    let err = cp
        .recover_commit(staged.artifact_handle_id)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
}

// --- convergence deadline ------------------------------------------------------

#[tokio::test]
async fn convergence_deadline_names_pending_intent_and_keeps_record_recoverable() {
    let (cp, _db, dir) = fixture().await;
    // No simulated node drives the pending intent.
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");

    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::CommitFailure);
    assert!(
        err.to_string().contains("artifact_commit_intent"),
        "deadline error must name the pending intent: {err}"
    );
    assert!(!target.exists());
    // The record stays pending and recoverable.
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );
    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();
    assert_eq!(report.state, ArtifactCommitState::Pending);
}

#[derive(Debug, Clone)]
struct VerifiedStage {
    artifact_handle_id: ArtifactHandleId,
    artifact_location_id: voom_core::ArtifactLocationId,
    source_file_version_id: FileVersionId,
    staging_path: PathBuf,
}

async fn fixture() -> (
    ControlPlane,
    voom_test_support::TempDatabase,
    tempfile::TempDir,
) {
    let db = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, db, artifact_tempdir())
}

async fn insert_active_test_root(cp: &ControlPlane, id: StorageRootId, path: &Path) {
    sqlx::query(
        "INSERT INTO library_roots \
         (id, library_id, owner_node_id, provider_kind, provider_locator, display_locator, state, \
          root_epoch, activation_identity, include_globs, exclude_globs, extension_allowlist, \
          scan_mode, symlink_policy, hidden_file_policy, stability_seconds, debounce_seconds, \
          enabled, created_at, updated_at) \
         VALUES (?, 9000001, 9000001, 'local_filesystem', ?, ?, 'active', 1, 'test-owner', \
                 '[]', '[]', '[]', 'manual_recursive', 'reject', 'ignore', 0, 0, 1, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(id.0).unwrap())
    .bind(path.display().to_string())
    .bind(path.display().to_string())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

async fn set_test_default_output_root(cp: &ControlPlane, id: StorageRootId) {
    sqlx::query("UPDATE library_roots SET default_output_root_id = ? WHERE id = 9000001")
        .bind(i64::try_from(id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

fn artifact_tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

async fn stage_bytes(cp: &ControlPlane, dir: &Path, bytes: &[u8]) -> StageCopyReport {
    let source = unique_path(dir, "source.bin");
    let staging = unique_path(dir, "staged.bin");
    std::fs::write(&source, bytes).unwrap();
    let seeded = seed_source(cp, &source, bytes).await;
    cp.stage_copy(StageCopyInput {
        file_version_id: seeded.file_version_id,
        source_location_id: Some(seeded.file_location_id),
        staging_path: staging,
    })
    .await
    .unwrap()
}

async fn stage_and_verify_bytes(cp: &ControlPlane, dir: &Path, bytes: &[u8]) -> VerifiedStage {
    let staged = stage_bytes(cp, dir, bytes).await;
    verify_artifact_with_dispatcher(
        cp,
        VerifyArtifactInput::for_staged_file(staged.artifact_handle_id, &staged.staging_path),
        &StaticDispatcher::success(bytes.to_vec()),
        &NoVerifyArtifactHooks,
    )
    .await
    .unwrap();
    VerifiedStage {
        artifact_handle_id: staged.artifact_handle_id,
        artifact_location_id: staged.artifact_location_id,
        source_file_version_id: staged.source_file_version_id,
        staging_path: staged.staging_path,
    }
}

#[derive(Debug, Clone, Copy)]
struct SeededSource {
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
}

async fn seed_source(cp: &ControlPlane, path: &Path, bytes: &[u8]) -> SeededSource {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &path.display().to_string(),
                ),
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
        panic!("seed_source should create a new file asset");
    };
    SeededSource {
        file_version_id,
        file_location_id,
    }
}

async fn create_pending_commit(
    cp: &ControlPlane,
    staged: &VerifiedStage,
    verification_id: ArtifactVerificationId,
    target_path: &str,
) -> voom_store::repo::media::artifacts::ArtifactCommitRecord {
    create_pending_commit_result(cp, staged, verification_id, target_path)
        .await
        .unwrap()
}

async fn create_pending_commit_result(
    cp: &ControlPlane,
    staged: &VerifiedStage,
    verification_id: ArtifactVerificationId,
    target_path: &str,
) -> Result<voom_store::repo::media::artifacts::ArtifactCommitRecord, VoomError> {
    let target_relative_locator =
        voom_store::test_support::test_relative_locator(target_path).into_inner();
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let result = cp
        .artifacts()
        .create_pending_commit_in_tx(
            &mut tx,
            NewArtifactCommitRecord {
                artifact_handle_id: staged.artifact_handle_id,
                source_file_version_id: staged.source_file_version_id,
                verification_id,
                target_path: target_path.to_owned(),
                temp_path: Some(format!("{target_path}.tmp")),
                report: serde_json::json!({
                    "test": true,
                    "rooted_target": {
                        "storage_root_id": voom_store::test_support::TEST_STORAGE_ROOT_ID.0,
                        "provider_relative_locator": target_relative_locator,
                    },
                }),
                started_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await;
    match result {
        Ok(record) => {
            tx.commit().await.unwrap();
            Ok(record)
        }
        Err(err) => Err(err),
    }
}

// The in-crate test build compiles a distinct `voom-control-plane` instance
// from the one voom-test-support links, so the case calls are driven through
// these thin local wrappers instead of the shared helper's methods.

fn spawn_commit_task(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
    target_path: &Path,
) -> tokio::task::JoinHandle<Result<CommitArtifactReport, CommitArtifactCommandError>> {
    let task_cp = cp.clone();
    let task_target = target_path.to_path_buf();
    tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id,
                target_path: task_target,
            })
            .await
    })
}

/// Spawn `commit_artifact` and wait until its pending intent row appears.
async fn spawn_and_wait_pending_intent(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
    target_path: PathBuf,
) -> ArtifactCommitIntentId {
    let _task = spawn_commit_task(cp, artifact_handle_id, &target_path);
    wait_pending_intent_id(cp, artifact_handle_id).await
}

/// Spawn a commit, authorize it, and leave the commit task waiting with an
/// authorized receipt-less intent. Returns the stuck record id.
async fn spawn_and_drive_authorize_only(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    artifact_handle_id: ArtifactHandleId,
    target_path: &Path,
) -> ArtifactCommitRecordId {
    let task = spawn_commit_task(cp, artifact_handle_id, target_path);
    let intent_id = wait_pending_intent_id(cp, artifact_handle_id).await;
    node_authorize(cp, node, intent_id).await.unwrap();
    let record_id = latest_record_id(cp, artifact_handle_id).await;
    task.abort();
    record_id
}

/// Spawn a commit and drive the node half to "applied but not completed":
/// matching bytes promoted, applied receipt reported, no completion.
async fn spawn_and_drive_to_applied_not_completed(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    artifact_handle_id: ArtifactHandleId,
    target_path: &Path,
) {
    let task = spawn_commit_task(cp, artifact_handle_id, target_path);
    let intent_id = wait_pending_intent_id(cp, artifact_handle_id).await;
    // The staged bytes live beside the target in the fixture directory.
    let staging_path = staging_path_for(cp, artifact_handle_id).await;
    drive_to_applied_not_completed(cp, node, intent_id, target_path, &staging_path).await;
    task.abort();
}

/// Resolve the staged bytes' path for the handle's live staging location.
async fn staging_path_for(cp: &ControlPlane, artifact_handle_id: ArtifactHandleId) -> PathBuf {
    let report: String = sqlx::query_scalar(
        "SELECT report FROM artifact_commit_records WHERE artifact_handle_id = ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(i64::try_from(artifact_handle_id.0).unwrap())
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&report).unwrap();
    PathBuf::from(value["staging_path"].as_str().unwrap().to_owned())
}
async fn mark_pending_committed(
    cp: &ControlPlane,
    commit_id: ArtifactCommitRecordId,
    staged: &VerifiedStage,
    target_path: &str,
) {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    let source = cp
        .identity()
        .get_file_version_in_tx(&mut tx, staged.source_file_version_id)
        .await
        .unwrap()
        .unwrap();
    let version = cp
        .identity()
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: source.file_asset_id,
                content_hash: blake3_checksum(b"source bytes"),
                size_bytes: 12,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(source.id),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let location = cp
        .identity()
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    target_path,
                ),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    cp.artifacts()
        .mark_commit_committed_in_tx(
            &mut tx,
            commit_id,
            version.id,
            location.id,
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn mark_pending_recovery(cp: &ControlPlane, commit_id: ArtifactCommitRecordId) {
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    cp.artifacts()
        .mark_commit_recovery_required_in_tx(
            &mut tx,
            commit_id,
            ArtifactCommitFailure {
                failure_class: FailureClass::CommitFailure,
                error_code: ErrorCode::CommitFailure,
                message: "injected".to_owned(),
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
            "injected".to_owned(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn assert_no_commit_records(cp: &ControlPlane, handle_id: ArtifactHandleId) {
    assert_eq!(count_commit_records(cp, handle_id).await, 0);
}

async fn count_commit_records(cp: &ControlPlane, handle_id: ArtifactHandleId) -> usize {
    cp.artifacts()
        .list_commit_records(handle_id)
        .await
        .unwrap()
        .len()
}

async fn count_events(cp: &ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    dir.join(format!(
        "{}-{file_name}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[derive(Debug)]
struct StaticDispatcher {
    bytes: Vec<u8>,
}

impl StaticDispatcher {
    fn success(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

#[async_trait]
impl VerifyArtifactDispatcher for StaticDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "test-dispatcher".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: u64::try_from(self.bytes.len()).unwrap(),
                content_hash: blake3_checksum(&self.bytes),
                modified_at: None,
                local_file_key: None,
            },
        })
    }
}

// --- simulated storage-owner node -------------------------------------------

pub(crate) fn unique_key(label: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("sim-{label}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub(crate) async fn node_authorize(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
) -> Result<crate::artifact::commit::intent::AuthorizeCommitOutcome, VoomError> {
    cp.remote_authorize_commit_intent(
        crate::artifact::commit::intent::RemoteCommitAuthorizeInput {
            intent_id,
            node_id: node.node_id,
            token: node.token.clone(),
            incarnation_id: node.incarnation_id,
            idempotency_key: unique_key("authorize"),
            request_hash: unique_key("authorize-hash"),
        },
    )
    .await
}

pub(crate) async fn node_report_applying(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
) -> Result<(), VoomError> {
    cp.remote_report_commit_applying(crate::artifact::commit::intent::RemoteCommitApplyingInput {
        intent_id,
        node_id: node.node_id,
        token: node.token.clone(),
        incarnation_id: node.incarnation_id,
        idempotency_key: unique_key("applying"),
        request_hash: unique_key("applying-hash"),
    })
    .await
    .map(|_| ())
}

pub(crate) async fn node_report_outcome(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
    evidence: CommitOutcomeEvidence,
) -> Result<(), VoomError> {
    cp.remote_report_commit_outcome(crate::artifact::commit::intent::RemoteCommitOutcomeInput {
        intent_id,
        node_id: node.node_id,
        token: node.token.clone(),
        incarnation_id: node.incarnation_id,
        idempotency_key: unique_key("outcome"),
        request_hash: unique_key("outcome-hash"),
        evidence,
    })
    .await
    .map(|_| ())
}

pub(crate) async fn node_complete(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
    fence_hex: &str,
) -> Result<crate::artifact::commit::intent::RemoteCommitCompleteOutcome, VoomError> {
    cp.remote_complete_commit_intent(crate::artifact::commit::intent::RemoteCommitCompleteInput {
        intent_id,
        node_id: node.node_id,
        token: node.token.clone(),
        incarnation_id: node.incarnation_id,
        idempotency_key: unique_key("complete"),
        request_hash: unique_key("complete-hash"),
        fence_hex: fence_hex.to_owned(),
    })
    .await
}

/// Flip the seeded root-owner node into the simulated remote node and return
/// its principal. Every test that drives a commit to convergence needs this.
pub(crate) async fn simulated_node(cp: &ControlPlane) -> SimulatedOwnerNode {
    let node = SimulatedOwnerNode::new().unwrap();
    node.install(cp.pool_for_test()).await.unwrap();
    node
}

/// Wait for the newest pending intent of an artifact handle (the driver half
/// of a spawned `commit_artifact` prepares it asynchronously).
pub(crate) async fn wait_pending_intent_id(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
) -> ArtifactCommitIntentId {
    for _ in 0..200 {
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM artifact_commit_intents \
             WHERE artifact_handle_id = ? AND state = 'pending' ORDER BY id DESC LIMIT 1",
        )
        .bind(i64::try_from(artifact_handle_id.0).unwrap())
        .fetch_optional(cp.pool_for_test())
        .await
        .unwrap();
        if let Some(id) = pending {
            return ArtifactCommitIntentId(u64::try_from(id).unwrap());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no pending commit intent appeared for handle {artifact_handle_id}");
}

/// Spawn `commit_artifact` concurrently with a simulated-node driver so the
/// bounded convergence wait sees the fenced intent reach a terminal state.
pub(crate) async fn commit_with_node(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    input: CommitArtifactInput,
) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
    let driver_cp = cp.clone();
    let driver_node = node.clone();
    let driver_handle = input.artifact_handle_id;
    let driver = tokio::spawn(async move {
        drive_pending_commit_local(&driver_cp, &driver_node, driver_handle).await
    });
    let outcome = cp.commit_artifact(input).await;
    driver.await.unwrap().unwrap();
    outcome
}

/// Drive the pending intent for a handle through authorize -> applying ->
/// no-replace promotion -> applied evidence -> fenced completion (the same
/// steps a real storage-owner node performs).
pub(crate) async fn drive_pending_commit_local(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    artifact_handle_id: ArtifactHandleId,
) -> Result<(), VoomError> {
    let intent_id = wait_pending_intent_id(cp, artifact_handle_id).await;
    let outcome = node_authorize(cp, node, intent_id).await?;
    node_report_applying(cp, node, intent_id).await?;
    let staging_path = rooted_path(
        cp,
        outcome.staging_storage_root_id.0,
        &outcome.staging_provider_relative_locator,
    )
    .await;
    let target_path = rooted_path(
        cp,
        outcome.target_storage_root_id.0,
        &outcome.target_provider_relative_locator,
    )
    .await;
    let staged_bytes = std::fs::read(&staging_path).unwrap();
    let staged_facts = voom_test_support::commit_node::observed_facts(&staged_bytes);
    let expected = crate::artifact::commit::intent::AuthorizeCommitOutcome {
        expected_size_bytes: outcome.expected_size_bytes,
        expected_content_hash: outcome.expected_content_hash.clone(),
        ..outcome.clone()
    };
    let matches_expected = staged_facts.size_bytes == expected.expected_size_bytes
        && staged_facts.content_hash == expected.expected_content_hash;
    let evidence = if !matches_expected {
        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
            reason: "staged bytes do not match the pinned expected facts".to_owned(),
            observed: Some(staged_facts),
        })
    } else if target_path.exists() {
        let existing = std::fs::read(&target_path).unwrap();
        let existing_facts = voom_test_support::commit_node::observed_facts(&existing);
        if existing_facts == staged_facts {
            CommitOutcomeEvidence::Applied(AppliedEvidence {
                observed: existing_facts,
            })
        } else {
            CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
                reason: "target already exists with different bytes".to_owned(),
                observed: Some(existing_facts),
            })
        }
    } else {
        std::fs::write(&target_path, &staged_bytes).unwrap();
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: staged_facts,
        })
    };
    let is_mismatch = matches!(evidence, CommitOutcomeEvidence::Mismatched(_));
    node_report_outcome(cp, node, intent_id, evidence).await?;
    if is_mismatch {
        return Ok(());
    }
    node_complete(cp, node, intent_id, &outcome.fence_hex).await?;
    Ok(())
}

pub(crate) async fn rooted_path(
    cp: &ControlPlane,
    storage_root_id: u64,
    relative_locator: &str,
) -> PathBuf {
    let locator: String =
        sqlx::query_scalar("SELECT provider_locator FROM library_roots WHERE id = ?")
            .bind(i64::try_from(storage_root_id).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    PathBuf::from(locator).join(relative_locator)
}

pub(crate) async fn intent_state(cp: &ControlPlane, intent_id: ArtifactCommitIntentId) -> String {
    sqlx::query_scalar("SELECT state FROM artifact_commit_intents WHERE id = ?")
        .bind(i64::try_from(intent_id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

pub(crate) async fn intent_receipt_kind(
    cp: &ControlPlane,
    intent_id: ArtifactCommitIntentId,
) -> String {
    let receipt: String =
        sqlx::query_scalar("SELECT receipt FROM artifact_commit_intents WHERE id = ?")
            .bind(i64::try_from(intent_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let value: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    value["kind"].as_str().unwrap().to_owned()
}

pub(crate) async fn record_state(cp: &ControlPlane, record_id: ArtifactCommitRecordId) -> String {
    sqlx::query_scalar("SELECT state FROM artifact_commit_records WHERE id = ?")
        .bind(i64::try_from(record_id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

pub(crate) async fn latest_record_id(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
) -> ArtifactCommitRecordId {
    cp.artifacts()
        .list_commit_records(artifact_handle_id)
        .await
        .unwrap()
        .last()
        .map(|record| record.id)
        .unwrap()
}

/// Insert a second, separately authenticated remote node for wrong-node
/// drift tests.
pub(crate) async fn install_second_remote_node(cp: &ControlPlane) -> SimulatedOwnerNode {
    let node = SimulatedOwnerNode::new().unwrap();
    let pool = cp.pool_for_test();
    let wrong_id = NodeId(9_000_042);
    sqlx::query(
        "INSERT INTO nodes (id, name, kind, status, registered_at, last_seen_at, \
         heartbeat_ttl_seconds, auth_token_hash, auth_token_hint, metadata, epoch) \
         VALUES (?, 'wrong-node', 'remote', 'active', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 60, ?, 'wrong', '{}', 0)",
    )
    .bind(i64::try_from(wrong_id.0).unwrap())
    .bind(crate::workers::hash_node_token(node.token.expose_secret()))
    .execute(pool)
    .await
    .unwrap();
    let incarnation = node.incarnation_id.to_string();
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, ?, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(&incarnation)
    .bind(i64::try_from(wrong_id.0).unwrap())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET active_incarnation_id = ? WHERE id = ?")
        .bind(&incarnation)
        .bind(i64::try_from(wrong_id.0).unwrap())
        .execute(pool)
        .await
        .unwrap();
    SimulatedOwnerNode {
        node_id: wrong_id,
        token: node.token,
        incarnation_id: node.incarnation_id,
    }
}

/// Drive one prepared generation through authorize + applying only, leaving
/// it receipt-bearing but never promoted.
pub(crate) async fn drive_authorize_and_applying(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
) {
    node_authorize(cp, node, intent_id).await.unwrap();
    node_report_applying(cp, node, intent_id).await.unwrap();
}

/// Drive one prepared generation to "applied but not completed": the node
/// promoted matching bytes and reported them, then the generation is left for
/// recovery to finalize.
pub(crate) async fn drive_to_applied_not_completed(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
    target_path: &Path,
    staged_path: &Path,
) {
    drive_authorize_and_applying(cp, node, intent_id).await;
    std::fs::copy(staged_path, target_path).unwrap();
    let bytes = std::fs::read(staged_path).unwrap();
    node_report_outcome(
        cp,
        node,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: voom_test_support::commit_node::observed_facts(&bytes),
        }),
    )
    .await
    .unwrap();
}

struct FailAfterPrepare;

impl CommitArtifactHooks for FailAfterPrepare {
    fn after_prepare(&self, _context: CommitArtifactPreparedContext<'_>) -> Result<(), VoomError> {
        Err(VoomError::CommitFailure(
            "injected failure after durable prepare".to_owned(),
        ))
    }
}
