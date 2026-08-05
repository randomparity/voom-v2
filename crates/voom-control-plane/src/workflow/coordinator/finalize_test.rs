use std::collections::BTreeMap;

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use voom_core::{FileLocationId, FileVersionId, MediaSnapshotId, TicketOperation, WorkerKind};
use voom_store::repo::execution::jobs::NewJob;
use voom_store::repo::execution::leases::NewLease;
use voom_store::repo::execution::tickets::NewTicket;
use voom_store::repo::execution::workers::{NewCapability, NewGrant};
use voom_store::repo::execution::workflow_progress::{FileAdmissionTier, NewFileProgress};
use voom_store::repo::execution::workflow_summaries::{FilePhaseOutcome, NewFileRunStart};
use voom_store::repo::media::artifacts::{
    ArtifactHandleAccessMode, ArtifactLocationKind, ArtifactVerificationStatus,
    NewArtifactCommitRecord, NewArtifactHandle, NewArtifactLocation, NewArtifactVerification,
    NewSidecarArtifactCommit,
};
use voom_store::repo::media::identity::{
    DiscoveredFile, FileLocationRepo, FileVersionRepo, IngestOutcome, MediaSnapshotRepo,
    NewFileLocation, NewFileVersion, ProducedBy,
};

use super::*;
use crate::workflow::coordinator::{Disposition, PhaseFile};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const NODE_ID: &str = "normalize";

fn committed_result_value() -> Value {
    json!({
        "job_id": 1,
        "ticket_id": 2,
        "lease_id": 3,
        "source_file_version_id": 4,
        "staged_artifact_handle_id": 5,
        "verification_id": 6,
        "commit_record_id": 7,
        "result_file_version_id": 8,
        "result_file_location_id": 9,
        "result_media_snapshot_id": 10,
        "worker_metadata": {"ignored": true},
    })
}

fn committed_result_decode_error(field: &str, invalid: Value) -> VoomError {
    let mut value = committed_result_value();
    value[field] = invalid;
    match CommittedResultFields::decode(voom_core::TicketId(2), &value) {
        Err(error) => error,
        Ok(_) => panic!("invalid committed result field `{field}` decoded successfully"),
    }
}

fn assert_positive_integer_database_error(error: &VoomError, field: &str) {
    assert!(matches!(error, VoomError::Database { .. }));
    assert!(
        error
            .to_string()
            .contains(&format!("field `{field}` must be a positive integer")),
        "{error}"
    );
}

#[test]
fn committed_result_decode_preserves_domain_id_types() {
    let result = CommittedResultFields::decode(voom_core::TicketId(2), &committed_result_value())
        .unwrap()
        .unwrap();

    assert_eq!(result.job_id, voom_core::JobId(1));
    assert_eq!(result.ticket_id, voom_core::TicketId(2));
    assert_eq!(result.lease_id, voom_core::LeaseId(3));
    assert_eq!(result.source_file_version_id, FileVersionId(4));
    assert_eq!(result.artifact_handle_id, voom_core::ArtifactHandleId(5));
    assert_eq!(
        result.verification_id,
        voom_core::ids::ArtifactVerificationId(6)
    );
    assert_eq!(
        result.commit_record_id,
        voom_core::ids::ArtifactCommitRecordId(7)
    );
    assert_eq!(result.result_file_version_id, FileVersionId(8));
    assert_eq!(result.result_file_location_id, FileLocationId(9));
    assert_eq!(result.result_media_snapshot_id, Some(MediaSnapshotId(10)));
}

#[test]
fn committed_result_decode_ignores_markerless_results() {
    let result = CommittedResultFields::decode(
        voom_core::TicketId(2),
        &json!({"job_id": null, "ticket_id": -1}),
    )
    .unwrap();

    assert!(result.is_none());
}

#[test]
fn committed_result_decode_rejects_zero_required_id_as_database_error() {
    let error = committed_result_decode_error("lease_id", json!(0));
    assert_positive_integer_database_error(&error, "lease_id");
}

#[test]
fn committed_result_decode_rejects_negative_required_id_as_database_error() {
    let error = committed_result_decode_error("lease_id", json!(-1));
    assert_positive_integer_database_error(&error, "lease_id");
}

#[test]
fn committed_result_decode_rejects_null_required_id_as_database_error() {
    let error = committed_result_decode_error("lease_id", Value::Null);
    assert_positive_integer_database_error(&error, "lease_id");
}

#[test]
fn committed_result_decode_rejects_zero_optional_id_as_database_error() {
    let error = committed_result_decode_error("result_media_snapshot_id", json!(0));
    assert_positive_integer_database_error(&error, "result_media_snapshot_id");
}

#[test]
fn committed_result_decode_rejects_negative_optional_id_as_database_error() {
    let error = committed_result_decode_error("result_media_snapshot_id", json!(-1));
    assert_positive_integer_database_error(&error, "result_media_snapshot_id");
}

#[test]
fn committed_result_decode_rejects_null_optional_id_as_database_error() {
    let error = committed_result_decode_error("result_media_snapshot_id", Value::Null);
    assert_positive_integer_database_error(&error, "result_media_snapshot_id");
}

#[test]
fn committed_result_decode_reports_job_before_later_invalid_fields() {
    let mut value = committed_result_value();
    value["job_id"] = Value::Null;
    value["ticket_id"] = Value::Null;
    value["result_media_snapshot_id"] = Value::Null;

    let Err(error) = CommittedResultFields::decode(voom_core::TicketId(2), &value) else {
        panic!("multi-invalid committed result decoded successfully");
    };

    assert_positive_integer_database_error(&error, "job_id");
    assert!(!error.to_string().contains("field `ticket_id`"));
}

#[tokio::test]
async fn finalization_attributes_exact_job_commit_when_unrelated_tip_is_newer() {
    let (cp, _tmp) = crate::cases::cp().await;
    let source = seed_version(&cp, "/library/movie.mkv", "source").await;
    let mut files = vec![phase_file(&cp, source, "movie").await];
    let job = cp
        .open_job(NewJob {
            kind: "policy_phase_barrier".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    activate_file_progress(&cp, job.id, &files[0]).await;
    let evidence = seed_committed_ticket_evidence(&cp, job.id, source, "movie").await;
    let unrelated =
        advance_chain_tip(&cp, evidence.version_id, "unrelated", ProducedBy::Transcode).await;

    let (rows, refreshed) = cp
        .finalize_phase(
            job.id,
            0,
            &mut files,
            &[Disposition::Planned {
                node_ids: vec![NODE_ID.to_owned()],
            }],
        )
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, FilePhaseOutcome::Committed);
    assert_eq!(rows[0].produced_file_version_id, Some(evidence.version_id));
    assert_eq!(
        rows[0].produced_file_location_id,
        Some(evidence.location_id)
    );
    assert_eq!(rows[0].reprobe_snapshot_id, Some(evidence.snapshot_id));
    assert_eq!(
        rows[0].artifact_handle_id,
        Some(evidence.artifact_handle_id)
    );
    assert_eq!(rows[0].ticket_ids, vec![evidence.ticket_id]);
    assert_eq!(refreshed[0].1.id, evidence.snapshot_id);
    assert_eq!(files[0].version_id, evidence.version_id);
    assert_eq!(
        files[0].phase_history.get(&0),
        Some(&FilePhaseOutcome::Committed)
    );
    assert_eq!(active_version_id(&cp, source).await, unrelated);
}

#[tokio::test]
async fn finalization_uses_latest_exact_commit_from_multi_operation_phase() {
    let (cp, _tmp) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let working = tmp.path().join("working");
    let output = tmp.path().join("output");
    tokio::fs::create_dir_all(&working).await.unwrap();
    let source = seed_version(&cp, "/library/multi-operation.mkv", "source").await;
    let mut files = vec![phase_file(&cp, source, "multi-operation").await];
    let job = open_policy_job(&cp).await;
    activate_file_progress(&cp, job.id, &files[0]).await;
    let first = seed_committed_ticket_evidence(&cp, job.id, source, "multi-operation").await;
    let terminal = seed_committed_ticket_evidence(&cp, job.id, source, "multi-operation").await;
    let first_path = working.join("first.mkv");
    let terminal_path = working.join("terminal.mkv");
    repoint_location(&cp, first.location_id, &first_path).await;
    repoint_location(&cp, terminal.location_id, &terminal_path).await;
    let unrelated =
        advance_chain_tip(&cp, terminal.version_id, "unrelated", ProducedBy::Transcode).await;

    let (rows, _) = cp
        .finalize_phase(
            job.id,
            0,
            &mut files,
            &[Disposition::Planned {
                node_ids: vec![NODE_ID.to_owned()],
            }],
        )
        .await
        .unwrap();

    assert_eq!(rows[0].produced_file_version_id, Some(terminal.version_id));
    assert_eq!(
        rows[0].produced_file_location_id,
        Some(terminal.location_id)
    );
    assert_eq!(
        rows[0].artifact_handle_id,
        Some(terminal.artifact_handle_id)
    );
    assert_eq!(
        rows[0].ticket_ids,
        vec![first.ticket_id, terminal.ticket_id]
    );
    assert_eq!(files[0].version_id, terminal.version_id);
    assert_eq!(active_version_id(&cp, source).await, unrelated);

    cp.reclaim_superseded_intermediates(
        &crate::cases::policy::compliance::PromotionPlan {
            pairs: vec![crate::cases::policy::compliance::PromotionPair {
                working_dir: working,
                output_dir: output,
            }],
        },
        &rows,
    )
    .await
    .unwrap();

    assert!(
        !first_path.exists(),
        "cleanup must reclaim an earlier commit from the same phase"
    );
    assert!(
        terminal_path.exists(),
        "cleanup must preserve the phase's terminal commit"
    );
    assert!(
        cp.identity()
            .get_file_location(first.location_id)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_some(),
        "cleanup must retire the earlier same-phase location"
    );
}

#[tokio::test]
async fn repeated_terminalization_cleanup_uses_carried_ticket_provenance() {
    let (cp, _tmp) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let working = tmp.path().join("working");
    let output = tmp.path().join("output");
    tokio::fs::create_dir_all(&working).await.unwrap();
    let source = seed_version(&cp, "/library/repeated-resume.mkv", "source").await;
    let first_job = open_policy_job(&cp).await;
    cp.workflow_summaries()
        .insert_file_run_starts(
            first_job.id,
            vec![NewFileRunStart {
                branch_id: "repeated-resume".to_owned(),
                starting_file_version_id: source,
                starting_phase_ordinal: 0,
            }],
        )
        .await
        .unwrap();
    let first = seed_committed_ticket_evidence(&cp, first_job.id, source, "repeated-resume").await;
    let terminal = seed_committed_ticket_evidence_for_phase(
        &cp,
        first_job.id,
        first.version_id,
        "repeated-resume",
        1,
    )
    .await;
    let first_path = working.join("first.mkv");
    let terminal_path = working.join("terminal.mkv");
    repoint_location(&cp, first.location_id, &first_path).await;
    repoint_location(&cp, terminal.location_id, &terminal_path).await;
    let second_job = open_policy_job(&cp).await;
    let third_job = open_policy_job(&cp).await;
    assert!(first_job.id < second_job.id && second_job.id < third_job.id);
    let phases = vec![
        phase_summary(first_job.id, 0, &first),
        phase_summary(first_job.id, 1, &terminal),
    ];

    cp.reclaim_superseded_intermediates(
        &crate::cases::policy::compliance::PromotionPlan {
            pairs: vec![crate::cases::policy::compliance::PromotionPair {
                working_dir: working,
                output_dir: output,
            }],
        },
        &phases,
    )
    .await
    .unwrap();

    assert!(
        !first_path.exists(),
        "a second resume must honor the carried first-job ticket provenance"
    );
    assert!(terminal_path.exists());
}

#[tokio::test]
async fn ordered_sidecar_outputs_require_and_return_each_commit_location() {
    let (cp, _tmp) = crate::cases::cp().await;
    let fixture = seed_ordered_sidecar_ticket(&cp).await;
    let validated = cp
        .validated_committed_location_ids_for_rows(&fixture.rows)
        .await
        .unwrap();

    assert_eq!(validated, fixture.locations);
}

#[tokio::test]
async fn ordered_sidecar_outputs_accept_missing_snapshots() {
    let (cp, _tmp) = crate::cases::cp().await;
    let fixture = seed_ordered_sidecar_ticket(&cp).await;
    let result: String = sqlx::query_scalar("SELECT result FROM tickets WHERE id = ?")
        .bind(i64::try_from(fixture.ticket_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let mut result: Value = serde_json::from_str(&result).unwrap();
    result
        .as_object_mut()
        .unwrap()
        .remove("result_media_snapshot_id");
    for output in result["outputs"].as_array_mut().unwrap() {
        output
            .as_object_mut()
            .unwrap()
            .remove("result_media_snapshot_id");
    }
    sqlx::query("UPDATE tickets SET result = ? WHERE id = ?")
        .bind(serde_json::to_string(&result).unwrap())
        .bind(i64::try_from(fixture.ticket_id.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();

    let validated = cp
        .validated_committed_location_ids_for_rows(&fixture.rows)
        .await
        .unwrap();

    assert_eq!(validated, fixture.locations);
}

#[tokio::test]
async fn ordered_sidecar_output_rejects_foreign_snapshot() {
    let (cp, _tmp) = crate::cases::cp().await;
    let fixture = seed_ordered_sidecar_ticket(&cp).await;
    let result: String = sqlx::query_scalar("SELECT result FROM tickets WHERE id = ?")
        .bind(i64::try_from(fixture.ticket_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let mut result: Value = serde_json::from_str(&result).unwrap();
    result["outputs"][1]["result_media_snapshot_id"] =
        result["outputs"][0]["result_media_snapshot_id"].clone();
    sqlx::query("UPDATE tickets SET result = ? WHERE id = ?")
        .bind(serde_json::to_string(&result).unwrap())
        .bind(i64::try_from(fixture.ticket_id.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();

    let error = cp
        .validated_committed_location_ids_for_rows(&fixture.rows)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("snapshot version"));
}

#[tokio::test]
async fn finalization_rejects_snapshotless_same_lineage_commit() {
    let (cp, _tmp) = crate::cases::cp().await;
    let source = seed_version(&cp, "/library/snapshotless.mkv", "source").await;
    let mut files = vec![phase_file(&cp, source, "snapshotless").await];
    let job = open_policy_job(&cp).await;
    activate_file_progress(&cp, job.id, &files[0]).await;
    let evidence = seed_committed_ticket_evidence(&cp, job.id, source, "snapshotless").await;
    let result: String = sqlx::query_scalar("SELECT result FROM tickets WHERE id = ?")
        .bind(i64::try_from(evidence.ticket_id.0).unwrap())
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let mut result: Value = serde_json::from_str(&result).unwrap();
    result
        .as_object_mut()
        .unwrap()
        .remove("result_media_snapshot_id");
    sqlx::query("UPDATE tickets SET result = ? WHERE id = ?")
        .bind(serde_json::to_string(&result).unwrap())
        .bind(i64::try_from(evidence.ticket_id.0).unwrap())
        .execute(&cp.pool)
        .await
        .unwrap();

    let error = cp
        .finalize_phase(
            job.id,
            0,
            &mut files,
            &[Disposition::Planned {
                node_ids: vec![NODE_ID.to_owned()],
            }],
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Conflict(_)));
    assert!(
        error
            .to_string()
            .contains("same-lineage result reprobe snapshot is missing")
    );
}

struct OrderedSidecarFixture {
    ticket_id: voom_core::TicketId,
    rows: [voom_store::repo::execution::workflow_summaries::FilePhaseSummary; 1],
    locations: Vec<FileLocationId>,
}

async fn seed_ordered_sidecar_ticket(cp: &crate::ControlPlane) -> OrderedSidecarFixture {
    let source = seed_version(cp, "/library/ordered-sidecars.mkv", "source").await;
    let job = open_policy_job(cp).await;
    cp.workflow_summaries()
        .insert_file_run_starts(
            job.id,
            vec![NewFileRunStart {
                branch_id: "ordered-sidecars".to_owned(),
                starting_file_version_id: source,
                starting_phase_ordinal: 0,
            }],
        )
        .await
        .unwrap();
    let operation = TicketOperation::new("transcode_video").unwrap();
    let worker = eligible_worker(cp, &operation).await;
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: TicketOperation::new("synthetic.workflow.operation.transcode_video").unwrap(),
            priority: 0,
            payload: json!({
                "workflow_id": format!("workflow-{}-phase-0", job.id.0),
                "node_id": format!("policy-node_{NODE_ID}"),
                "branch_id": "ordered-sidecars",
                "rendered_payload": {"source_file_version_id": source.0},
            }),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: Duration::minutes(1),
            now: T0,
        })
        .await
        .unwrap();
    let mut outputs = Vec::new();
    let mut locations = Vec::new();
    let context = SidecarTicketContext {
        source,
        worker_id: worker.id,
    };
    for ordinal in 0..2 {
        let (output, location) = create_sidecar_commit_result(cp, context, ordinal).await;
        outputs.push(output);
        locations.push(location);
    }
    let first = outputs[0].clone();
    cp.release_lease(
        lease.id,
        json!({
            "job_id": job.id.0,
            "ticket_id": ticket.id.0,
            "lease_id": lease.id.0,
            "source_file_version_id": first["source_file_version_id"],
            "staged_artifact_handle_id": first["staged_artifact_handle_id"],
            "staged_artifact_location_id": first["staged_artifact_location_id"],
            "verification_id": first["verification_id"],
            "commit_record_id": first["commit_record_id"],
            "result_file_version_id": first["result_file_version_id"],
            "result_file_location_id": first["result_file_location_id"],
            "result_media_snapshot_id": first["result_media_snapshot_id"],
            "outputs": outputs,
        }),
        T0,
    )
    .await
    .unwrap();
    OrderedSidecarFixture {
        ticket_id: ticket.id,
        rows: [
            voom_store::repo::execution::workflow_summaries::FilePhaseSummary {
                id: 1,
                job_id: job.id,
                phase_ordinal: 0,
                branch_id: "ordered-sidecars".to_owned(),
                ticket_ids: vec![ticket.id],
                produced_file_version_id: None,
                produced_file_location_id: None,
                artifact_handle_id: None,
                artifact_verification_id: None,
                reprobe_snapshot_id: None,
                outcome: FilePhaseOutcome::Skipped,
                created_at: T0,
            },
        ],
        locations,
    }
}

#[tokio::test]
async fn cleanup_rejects_foreign_carried_ticket_provenance() {
    let (cp, _tmp) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let working = tmp.path().join("working");
    let output = tmp.path().join("output");
    tokio::fs::create_dir_all(&working).await.unwrap();
    let source = seed_version(&cp, "/library/owned.mkv", "owned-source").await;
    let job = open_policy_job(&cp).await;
    let first = seed_committed_ticket_evidence(&cp, job.id, source, "owned").await;
    let terminal = seed_committed_ticket_evidence(&cp, job.id, first.version_id, "owned").await;
    let foreign_source = seed_version(&cp, "/library/foreign.mkv", "foreign-source").await;
    let foreign_job = open_policy_job(&cp).await;
    let foreign =
        seed_committed_ticket_evidence(&cp, foreign_job.id, foreign_source, "foreign").await;
    let terminal_path = working.join("terminal.mkv");
    let foreign_path = working.join("foreign.mkv");
    repoint_location(&cp, terminal.location_id, &terminal_path).await;
    repoint_location(&cp, foreign.location_id, &foreign_path).await;
    let mut phases = vec![
        phase_summary(job.id, 0, &first),
        phase_summary(job.id, 1, &terminal),
    ];
    phases[0].ticket_ids = vec![foreign.ticket_id];

    let error = cp
        .reclaim_superseded_intermediates(
            &crate::cases::policy::compliance::PromotionPlan {
                pairs: vec![crate::cases::policy::compliance::PromotionPair {
                    working_dir: working,
                    output_dir: output,
                }],
            },
            &phases,
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), "CONFLICT");
    assert!(foreign_path.exists(), "foreign bytes must not be deleted");
    assert!(
        cp.identity()
            .get_file_location(foreign.location_id)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none(),
        "foreign location must remain live"
    );
}

#[tokio::test]
async fn finalization_rejects_unrelated_tip_when_job_has_no_commit_evidence() {
    let (cp, _tmp) = crate::cases::cp().await;
    let source = seed_version(&cp, "/library/no-op.mkv", "source").await;
    let mut files = vec![phase_file(&cp, source, "no-op").await];
    let job = open_policy_job(&cp).await;
    seed_succeeded_ticket_without_commit(&cp, job.id, source, "no-op").await;
    let unrelated = advance_chain_tip(&cp, source, "unrelated", ProducedBy::Transcode).await;

    let error = cp
        .finalize_phase(
            job.id,
            0,
            &mut files,
            &[Disposition::Planned {
                node_ids: vec![NODE_ID.to_owned()],
            }],
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::StaleIdentityEvidence(_)));
    assert!(
        cp.workflow_summaries
            .file_phases_for_job(job.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(event_count(&cp, "lease.released").await, 1);
    assert_eq!(active_version_id(&cp, source).await, unrelated);
}

#[tokio::test]
async fn finalization_rejects_every_mismatched_result_provenance_field() {
    for field in [
        "job_id",
        "ticket_id",
        "lease_id",
        "source_file_version_id",
        "staged_artifact_handle_id",
        "verification_id",
        "commit_record_id",
        "result_file_version_id",
        "result_file_location_id",
        "result_media_snapshot_id",
    ] {
        let (cp, _tmp) = crate::cases::cp().await;
        let source = seed_version(&cp, "/library/mismatch.mkv", "source").await;
        let mut files = vec![phase_file(&cp, source, "mismatch").await];
        let job = open_policy_job(&cp).await;
        let evidence = seed_committed_ticket_evidence(&cp, job.id, source, "mismatch").await;
        sqlx::query("UPDATE tickets SET result = json_set(result, ?, 999999) WHERE id = ?")
            .bind(format!("$.{field}"))
            .bind(i64::try_from(evidence.ticket_id.0).unwrap())
            .execute(&cp.pool)
            .await
            .unwrap();

        let error = cp
            .finalize_phase(
                job.id,
                0,
                &mut files,
                &[Disposition::Planned {
                    node_ids: vec![NODE_ID.to_owned()],
                }],
            )
            .await
            .unwrap_err();

        assert!(matches!(error, VoomError::Conflict(_)), "{field}: {error}");
        assert!(
            cp.workflow_summaries
                .file_phases_for_job(job.id)
                .await
                .unwrap()
                .is_empty(),
            "{field}"
        );
        assert_eq!(event_count(&cp, "lease.released").await, 1, "{field}");
        assert_eq!(
            active_version_id(&cp, source).await,
            evidence.version_id,
            "{field}"
        );
    }
}

#[tokio::test]
async fn failed_phase_reports_only_exact_job_commit_evidence() {
    let (cp, _tmp) = crate::cases::cp().await;
    let produced_source = seed_version(&cp, "/library/produced.mkv", "produced-source").await;
    let unrelated_source = seed_version(&cp, "/library/unrelated.mkv", "unrelated-source").await;
    let files = vec![
        phase_file(&cp, produced_source, "produced").await,
        phase_file(&cp, unrelated_source, "unrelated").await,
    ];
    let job = open_policy_job(&cp).await;
    activate_file_progress(&cp, job.id, &files[0]).await;
    let evidence = seed_committed_ticket_evidence(&cp, job.id, produced_source, "produced").await;
    let later_tip = advance_chain_tip(
        &cp,
        evidence.version_id,
        "later-produced-tip",
        ProducedBy::Transcode,
    )
    .await;
    let unrelated_tip = advance_chain_tip(
        &cp,
        unrelated_source,
        "unrelated-tip",
        ProducedBy::Transcode,
    )
    .await;
    let disposition = Disposition::Planned {
        node_ids: vec![NODE_ID.to_owned()],
    };
    let mut file_phases = Vec::new();
    for file in &files {
        if let Some(row) = cp
            .finalize_failed_file_phase(job.id, 0, file, &disposition)
            .await
            .unwrap()
        {
            file_phases.push(row);
        }
    }

    assert_eq!(file_phases.len(), 1);
    assert_eq!(
        file_phases[0].produced_file_version_id,
        Some(evidence.version_id)
    );
    assert_eq!(
        file_phases[0].artifact_handle_id,
        Some(evidence.artifact_handle_id)
    );
    let durable = cp
        .workflow_summaries
        .file_phases_for_job(job.id)
        .await
        .unwrap();
    assert_eq!(durable, file_phases);
    assert_eq!(
        cp.workflow_progress
            .file_progress(job.id, "produced")
            .await
            .unwrap()
            .unwrap()
            .next_phase_ordinal,
        1,
        "abort-path commit evidence and the resume cursor advance atomically"
    );
    assert_eq!(active_version_id(&cp, produced_source).await, later_tip);
    assert_eq!(
        active_version_id(&cp, unrelated_source).await,
        unrelated_tip
    );
}

#[expect(
    clippy::struct_field_names,
    reason = "test fixture fields preserve the durable evidence entity names"
)]
struct CommittedEvidence {
    ticket_id: voom_core::TicketId,
    artifact_handle_id: voom_core::ArtifactHandleId,
    verification_id: voom_core::ids::ArtifactVerificationId,
    version_id: FileVersionId,
    location_id: FileLocationId,
    snapshot_id: MediaSnapshotId,
}

fn phase_summary(
    job_id: voom_core::JobId,
    phase_ordinal: u32,
    evidence: &CommittedEvidence,
) -> voom_store::repo::execution::workflow_summaries::FilePhaseSummary {
    voom_store::repo::execution::workflow_summaries::FilePhaseSummary {
        id: u64::from(phase_ordinal) + 1,
        job_id,
        phase_ordinal,
        branch_id: "repeated-resume".to_owned(),
        ticket_ids: vec![evidence.ticket_id],
        produced_file_version_id: Some(evidence.version_id),
        produced_file_location_id: Some(evidence.location_id),
        artifact_handle_id: Some(evidence.artifact_handle_id),
        artifact_verification_id: Some(evidence.verification_id),
        reprobe_snapshot_id: Some(evidence.snapshot_id),
        outcome: FilePhaseOutcome::Committed,
        created_at: T0,
    }
}

async fn repoint_location(
    cp: &crate::ControlPlane,
    location_id: FileLocationId,
    path: &std::path::Path,
) {
    tokio::fs::write(path, b"committed-bytes").await.unwrap();
    let location = cp
        .identity()
        .get_file_location(location_id)
        .await
        .unwrap()
        .unwrap();
    let mut tx = crate::cases::begin_tx(&cp.pool).await.unwrap();
    cp.identity()
        .update_file_location_address_in_tx(
            &mut tx,
            location_id,
            location.epoch,
            voom_store::test_support::TEST_STORAGE_ROOT_ID,
            voom_store::test_support::test_relative_locator(&path.display().to_string()),
            T0,
        )
        .await
        .unwrap();
    crate::cases::commit_tx(tx).await.unwrap();
}

async fn open_policy_job(cp: &crate::ControlPlane) -> voom_store::repo::execution::jobs::Job {
    cp.open_job(NewJob {
        kind: "policy_phase_barrier".to_owned(),
        priority: 0,
        created_at: T0,
    })
    .await
    .unwrap()
}

async fn activate_file_progress(
    cp: &crate::ControlPlane,
    job_id: voom_core::JobId,
    file: &PhaseFile,
) {
    cp.workflow_summaries
        .insert_file_run_starts(
            job_id,
            vec![NewFileRunStart {
                branch_id: file.branch_id.clone(),
                starting_file_version_id: file.version_id,
                starting_phase_ordinal: 0,
            }],
        )
        .await
        .unwrap();
    cp.workflow_progress
        .insert_file_window(
            job_id,
            1,
            vec![NewFileProgress {
                branch_id: file.branch_id.clone(),
                input_ordinal: file.ordinal,
                admission_tier: FileAdmissionTier::Pending,
                next_phase_ordinal: 0,
            }],
            T0,
        )
        .await
        .unwrap();
    cp.workflow_progress
        .admit_next_file(job_id, T0)
        .await
        .unwrap()
        .unwrap();
}

async fn seed_succeeded_ticket_without_commit(
    cp: &crate::ControlPlane,
    job_id: voom_core::JobId,
    source_version_id: FileVersionId,
    branch_id: &str,
) {
    let operation = TicketOperation::new("transcode_video").unwrap();
    let worker = eligible_worker(cp, &operation).await;
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job_id),
            kind: TicketOperation::new("synthetic.workflow.operation.transcode_video").unwrap(),
            priority: 0,
            payload: json!({
                "workflow_id": format!("workflow-{}-phase-0", job_id.0),
                "node_id": format!("policy-node_{NODE_ID}"),
                "branch_id": branch_id,
                "rendered_payload": {
                    "source_file_version_id": source_version_id.0,
                },
            }),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: Duration::minutes(1),
            now: T0,
        })
        .await
        .unwrap();
    cp.release_lease(lease.id, json!({"status": "no-op"}), T0)
        .await
        .unwrap();
}

async fn seed_committed_ticket_evidence(
    cp: &crate::ControlPlane,
    job_id: voom_core::JobId,
    source_version_id: FileVersionId,
    branch_id: &str,
) -> CommittedEvidence {
    seed_committed_ticket_evidence_for_phase(cp, job_id, source_version_id, branch_id, 0).await
}

async fn seed_committed_ticket_evidence_for_phase(
    cp: &crate::ControlPlane,
    job_id: voom_core::JobId,
    source_version_id: FileVersionId,
    branch_id: &str,
    phase_ordinal: u32,
) -> CommittedEvidence {
    let ticket_kind = TicketOperation::new("synthetic.workflow.operation.transcode_video").unwrap();
    let operation = TicketOperation::new("transcode_video").unwrap();
    let worker = eligible_worker(cp, &operation).await;
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: Some(job_id),
            kind: ticket_kind,
            priority: 0,
            payload: json!({
                "workflow_id": format!("workflow-{}-phase-{phase_ordinal}", job_id.0),
                "node_id": format!("policy-node_{NODE_ID}"),
                "branch_id": branch_id,
                "rendered_payload": {
                    "source_file_version_id": source_version_id.0,
                },
            }),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: Duration::minutes(1),
            now: T0,
        })
        .await
        .unwrap();
    let staged = create_verified_staging(
        cp,
        source_version_id,
        worker.id,
        Some((ticket.id, lease.id)),
    )
    .await;
    let commit = create_pending_commit(cp, source_version_id, &staged).await;
    let result_hash = format!("job-produced-{}", staged.handle_id.0);
    let version_id = advance_chain_tip(
        cp,
        source_version_id,
        &result_hash,
        ProducedBy::StagedCommit,
    )
    .await;
    let location_id = live_location_id(cp, version_id).await;
    let snapshot_id = latest_snapshot(cp, version_id).await.id;
    mark_commit_committed(cp, commit.id, version_id, location_id).await;
    cp.release_lease(
        lease.id,
        json!({
            "job_id": job_id.0,
            "ticket_id": ticket.id.0,
            "lease_id": lease.id.0,
            "source_file_version_id": source_version_id.0,
            "staged_artifact_handle_id": staged.handle_id.0,
            "staged_artifact_location_id": staged.location_id.0,
            "verification_id": staged.verification_id.0,
            "commit_record_id": commit.id.0,
            "result_file_version_id": version_id.0,
            "result_file_location_id": location_id.0,
            "result_media_snapshot_id": snapshot_id.0,
        }),
        T0,
    )
    .await
    .unwrap();
    CommittedEvidence {
        ticket_id: ticket.id,
        artifact_handle_id: staged.handle_id,
        verification_id: staged.verification_id,
        version_id,
        location_id,
        snapshot_id,
    }
}

#[expect(
    clippy::struct_field_names,
    reason = "test fixture fields preserve the staged evidence entity names"
)]
struct VerifiedStaging {
    handle_id: voom_core::ArtifactHandleId,
    location_id: voom_core::ArtifactLocationId,
    verification_id: voom_core::ids::ArtifactVerificationId,
}

#[derive(Clone, Copy)]
struct SidecarTicketContext {
    source: FileVersionId,
    worker_id: voom_core::WorkerId,
}

async fn create_sidecar_commit_result(
    cp: &crate::ControlPlane,
    context: SidecarTicketContext,
    ordinal: u32,
) -> (Value, FileLocationId) {
    let staged = create_verified_staging(cp, context.source, context.worker_id, None).await;
    let commit = create_pending_commit(cp, context.source, &staged).await;
    let mut tx = crate::cases::begin_tx(&cp.pool).await.unwrap();
    let committed = cp
        .artifacts()
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: commit.id,
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &commit.target_path.clone(),
                ),
                target_path: commit.target_path.clone(),
                content_hash: format!("sidecar-committed-{ordinal}"),
                size_bytes: 2048,
                observed_at: T0,
                finished_at: T0,
            },
        )
        .await
        .unwrap();
    crate::cases::commit_tx(tx).await.unwrap();
    let snapshot = cp
        .record_media_snapshot(
            committed.file_version_id,
            None,
            snapshot_payload("sidecar"),
            T0,
        )
        .await
        .unwrap();
    (
        json!({
            "source_file_version_id": context.source.0,
            "staged_artifact_handle_id": staged.handle_id.0,
            "staged_artifact_location_id": staged.location_id.0,
            "verification_id": staged.verification_id.0,
            "commit_record_id": commit.id.0,
            "result_file_version_id": committed.file_version_id.0,
            "result_file_location_id": committed.file_location_id.0,
            "result_media_snapshot_id": snapshot.id.0,
        }),
        committed.file_location_id,
    )
}

async fn create_verified_staging(
    cp: &crate::ControlPlane,
    source_version_id: FileVersionId,
    worker_id: voom_core::WorkerId,
    owner: Option<(voom_core::TicketId, voom_core::LeaseId)>,
) -> VerifiedStaging {
    let handle = cp
        .create_artifact_handle(NewArtifactHandle {
            size_bytes: Some(2048),
            checksum: Some("job-produced".to_owned()),
            privacy_class: "internal".to_owned(),
            durability_class: "staging".to_owned(),
            allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
            mutability: "immutable".to_owned(),
            source_lineage: Some(json!({"kind": "test"})),
            file_version_id: Some(source_version_id),
            created_at: T0,
        })
        .await
        .unwrap();
    let location = cp
        .record_artifact_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: ArtifactLocationKind::Staging,
            value: format!("/staging/{}.mkv", handle.id.0),
            observed_at: T0,
        })
        .await
        .unwrap();
    let mut tx = crate::cases::begin_tx(&cp.pool).await.unwrap();
    let verification = cp
        .artifacts()
        .record_verification_in_tx(
            &mut tx,
            NewArtifactVerification {
                artifact_handle_id: handle.id,
                artifact_location_id: location.id,
                path: location.value.clone(),
                worker_id,
                workflow_ticket_id: owner.map(|(ticket_id, _)| ticket_id),
                workflow_lease_id: owner.map(|(_, lease_id)| lease_id),
                status: ArtifactVerificationStatus::Succeeded,
                expected_size_bytes: 2048,
                expected_checksum: "job-produced".to_owned(),
                observed_size_bytes: Some(2048),
                observed_checksum: Some("job-produced".to_owned()),
                failure_class: None,
                error_code: None,
                message: None,
                report: json!({"status": "verified"}),
                started_at: T0,
                finished_at: T0,
            },
        )
        .await
        .unwrap();
    crate::cases::commit_tx(tx).await.unwrap();
    VerifiedStaging {
        handle_id: handle.id,
        location_id: location.id,
        verification_id: verification.id,
    }
}

async fn create_pending_commit(
    cp: &crate::ControlPlane,
    source_version_id: FileVersionId,
    staged: &VerifiedStaging,
) -> voom_store::repo::media::artifacts::ArtifactCommitRecord {
    let target_path = format!("/output/job-produced-{}.mkv", staged.handle_id.0);
    let provider_relative_locator = voom_store::test_support::test_relative_locator(&target_path);
    let mut tx = crate::cases::begin_tx(&cp.pool).await.unwrap();
    let record = cp
        .artifacts()
        .create_pending_commit_in_tx(
            &mut tx,
            NewArtifactCommitRecord {
                artifact_handle_id: staged.handle_id,
                source_file_version_id: source_version_id,
                verification_id: staged.verification_id,
                target_path,
                temp_path: None,
                report: json!({
                    "rooted_target": {
                        "storage_root_id": voom_store::test_support::TEST_STORAGE_ROOT_ID.0,
                        "provider_relative_locator": provider_relative_locator.as_str(),
                    },
                }),
                started_at: T0,
            },
        )
        .await
        .unwrap();
    crate::cases::commit_tx(tx).await.unwrap();
    record
}

async fn mark_commit_committed(
    cp: &crate::ControlPlane,
    commit_id: voom_core::ids::ArtifactCommitRecordId,
    version_id: FileVersionId,
    location_id: FileLocationId,
) {
    let mut tx = crate::cases::begin_tx(&cp.pool).await.unwrap();
    cp.artifacts()
        .mark_commit_committed_in_tx(&mut tx, commit_id, version_id, location_id, T0, T0)
        .await
        .unwrap();
    crate::cases::commit_tx(tx).await.unwrap();
}

async fn eligible_worker(
    cp: &crate::ControlPlane,
    operation: &TicketOperation,
) -> voom_store::repo::execution::workers::Worker {
    let ordinal: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let worker = cp
        .register_worker(crate::cases::workers::RegisterWorkerInput {
            name: format!("lineage-worker-{ordinal}"),
            kind: WorkerKind::Synthetic,
        })
        .await
        .unwrap();
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: vec!["local_path".to_owned()],
        extra: json!({}),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation.clone()],
        can_access_read: vec!["local_path".to_owned()],
        can_access_write: vec!["local_path".to_owned()],
        denies: Vec::new(),
        max_parallel: json!({}),
    })
    .await
    .unwrap();
    worker
}

async fn seed_version(cp: &crate::ControlPlane, path: &str, hash: &str) -> FileVersionId {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(path),
                content_hash: hash.to_owned(),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected a new file asset");
    };
    cp.record_media_snapshot(file_version_id, None, snapshot_payload(hash), T0)
        .await
        .unwrap();
    file_version_id
}

async fn advance_chain_tip(
    cp: &crate::ControlPlane,
    parent_id: FileVersionId,
    hash: &str,
    produced_by: ProducedBy,
) -> FileVersionId {
    let parent = cp
        .identity()
        .get_file_version(parent_id)
        .await
        .unwrap()
        .unwrap();
    let version = cp
        .create_file_version(NewFileVersion {
            file_asset_id: parent.file_asset_id,
            content_hash: hash.to_owned(),
            size_bytes: 2048,
            produced_by,
            produced_from_version_id: Some(parent_id),
            created_at: T0,
        })
        .await
        .unwrap();
    cp.create_file_location(NewFileLocation {
        file_version_id: version.id,
        storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
        provider_relative_locator: voom_store::test_support::test_relative_locator(&format!(
            "/output/{hash}.mkv"
        )),
        proof: None,
        observed_at: T0,
    })
    .await
    .unwrap();
    cp.record_media_snapshot(version.id, None, snapshot_payload(hash), T0)
        .await
        .unwrap();
    version.id
}

async fn phase_file(
    cp: &crate::ControlPlane,
    version_id: FileVersionId,
    branch_id: &str,
) -> PhaseFile {
    let version = cp
        .identity()
        .get_file_version(version_id)
        .await
        .unwrap()
        .unwrap();
    PhaseFile {
        asset_id: version.file_asset_id,
        version_id,
        snapshot: latest_snapshot(cp, version_id).await,
        branch_id: branch_id.to_owned(),
        ordinal: 1,
        admission_tier: FileAdmissionTier::Pending,
        resume_ordinal: 0,
        phase_history: BTreeMap::new(),
    }
}

async fn latest_snapshot(cp: &crate::ControlPlane, version_id: FileVersionId) -> MediaSnapshot {
    cp.identity()
        .list_media_snapshots_by_version(version_id)
        .await
        .unwrap()
        .into_iter()
        .max_by_key(|snapshot| snapshot.id.0)
        .unwrap()
}

async fn live_location_id(cp: &crate::ControlPlane, version_id: FileVersionId) -> FileLocationId {
    cp.identity()
        .list_live_file_locations_by_version(version_id)
        .await
        .unwrap()[0]
        .id
}

async fn active_version_id(
    cp: &crate::ControlPlane,
    lineage_version_id: FileVersionId,
) -> FileVersionId {
    let asset_id = cp
        .identity()
        .get_file_version(lineage_version_id)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    cp.identity()
        .get_active_version_with_snapshot(asset_id)
        .await
        .unwrap()
        .unwrap()
        .0
        .id
}

fn snapshot_payload(marker: &str) -> Value {
    json!({
        "container": {"format_name": "matroska"},
        "marker": marker,
        "streams": [],
    })
}

async fn event_count(cp: &crate::ControlPlane, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?")
        .bind(kind)
        .fetch_one(&cp.pool)
        .await
        .unwrap()
}
