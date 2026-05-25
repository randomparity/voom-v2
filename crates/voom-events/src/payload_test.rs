use super::*;

use time::OffsetDateTime;
use voom_core::FailureClass;

#[test]
fn schema_initialized_payload_round_trip() {
    let p = SchemaInitializedPayload {
        migrations_applied: 2,
        schema_init_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&Event::SchemaInitialized(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::SchemaInitialized(p), back);
}

#[test]
fn ticket_failed_retriable_payload_round_trip() {
    let p = TicketFailedRetriablePayload {
        ticket_id: 5,
        attempt: 1,
        max_attempts: 3,
        reason: "transient sqlite lock".to_owned(),
        class: FailureClass::WorkerTimeout,
        next_eligible_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&Event::TicketFailedRetriable(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::TicketFailedRetriable(p), back);
}

#[test]
fn ticket_failed_terminal_payload_round_trip_carries_class_and_null_issue_id() {
    let p = TicketFailedTerminalPayload {
        ticket_id: 7,
        attempt: 3,
        max_attempts: 3,
        reason: "retries exhausted".to_owned(),
        class: FailureClass::WorkerCrash,
        issue_id: None,
    };
    let json = serde_json::to_string(&Event::TicketFailedTerminal(p.clone())).unwrap();
    // M1 wire format: issue_id is always present (serialized as `null`)
    // so the M3 migration that flips it to `Some(IssueId(_))` does not
    // change the JSON shape.
    assert!(json.contains("\"issue_id\":null"));
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::TicketFailedTerminal(p), back);
}

#[test]
fn ticket_requeued_after_force_release_payload_round_trip() {
    let p = TicketRequeuedAfterForceReleasePayload {
        ticket_id: 3,
        lease_id: 9,
        actor: "alice".to_owned(),
        reason: "stuck worker".to_owned(),
    };
    let json = serde_json::to_string(&Event::TicketRequeuedAfterForceRelease(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::TicketRequeuedAfterForceRelease(p), back);
}

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

#[test]
fn issue_opened_payload_round_trip() {
    let p = issue_payload("planned");
    let json = serde_json::to_string(&Event::IssueOpened(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::IssueOpened(p), back);
}

#[test]
fn issue_updated_payload_round_trip() {
    let p = issue_payload("open");
    let json = serde_json::to_string(&Event::IssueUpdated(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::IssueUpdated(p), back);
}

#[test]
fn issue_resolved_payload_round_trip() {
    let p = issue_payload("resolved");
    let json = serde_json::to_string(&Event::IssueResolved(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::IssueResolved(p), back);
}

#[test]
fn lease_force_released_payload_carries_actor_and_reason() {
    let p = LeaseForceReleasedPayload {
        lease_id: 9,
        ticket_id: 5,
        actor: "alice".to_owned(),
        reason: "clearing stuck lease".to_owned(),
        also_requeue: true,
    };
    let json = serde_json::to_value(Event::LeaseForceReleased(p)).unwrap();
    let obj = json.as_object().unwrap();
    // Sum type uses #[serde(tag = "kind", content = "payload")], so the
    // typed payload object lives under the `payload` key.
    let payload_value = &obj["payload"];
    assert_eq!(payload_value["actor"], "alice");
    assert_eq!(payload_value["reason"], "clearing stuck lease");
    assert_eq!(payload_value["also_requeue"], true);
}

#[test]
fn node_registered_payload_round_trip() {
    let p = NodeRegisteredPayload {
        node_id: 42,
        name: "node-a".to_owned(),
        kind: "local".to_owned(),
        status: "active".to_owned(),
        heartbeat_ttl_seconds: 30,
    };
    let json = serde_json::to_value(Event::NodeRegistered(p.clone())).unwrap();
    assert_eq!(json["kind"], "node.registered");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::NodeRegistered(q) if q == p));
    assert_eq!(Event::NodeRegistered(p).kind(), EventKind::NodeRegistered);
}

#[test]
fn node_heartbeat_recorded_payload_round_trip() {
    let p = NodeHeartbeatRecordedPayload {
        node_id: 42,
        status: "active".to_owned(),
        last_seen_at: OffsetDateTime::UNIX_EPOCH,
        epoch: 7,
    };
    let json = serde_json::to_value(Event::NodeHeartbeatRecorded(p.clone())).unwrap();
    assert_eq!(json["kind"], "node.heartbeat_recorded");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::NodeHeartbeatRecorded(q) if q == p));
    assert_eq!(
        Event::NodeHeartbeatRecorded(p).kind(),
        EventKind::NodeHeartbeatRecorded
    );
}

#[test]
fn node_marked_stale_payload_round_trip() {
    let p = NodeMarkedStalePayload {
        node_id: 42,
        marked_stale_at: OffsetDateTime::UNIX_EPOCH,
        epoch: 8,
    };
    let json = serde_json::to_value(Event::NodeMarkedStale(p.clone())).unwrap();
    assert_eq!(json["kind"], "node.marked_stale");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::NodeMarkedStale(q) if q == p));
    assert_eq!(Event::NodeMarkedStale(p).kind(), EventKind::NodeMarkedStale);
}

#[test]
fn node_retired_payload_round_trip() {
    let p = NodeRetiredPayload {
        node_id: 42,
        retired_at: OffsetDateTime::UNIX_EPOCH,
        epoch: 9,
    };
    let json = serde_json::to_value(Event::NodeRetired(p.clone())).unwrap();
    assert_eq!(json["kind"], "node.retired");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::NodeRetired(q) if q == p));
    assert_eq!(Event::NodeRetired(p).kind(), EventKind::NodeRetired);
}

#[test]
fn worker_linked_to_node_payload_round_trip() {
    let p = WorkerLinkedToNodePayload {
        worker_id: 7,
        node_id: 42,
    };
    let json = serde_json::to_value(Event::WorkerLinkedToNode(p.clone())).unwrap();
    assert_eq!(json["kind"], "worker.linked_to_node");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::WorkerLinkedToNode(q) if q == p));
    assert_eq!(
        Event::WorkerLinkedToNode(p).kind(),
        EventKind::WorkerLinkedToNode
    );
}

#[test]
fn artifact_staged_payload_round_trip() {
    let p = ArtifactStagedPayload {
        artifact_handle_id: 10,
        artifact_location_id: 11,
        source_file_version_id: 12,
        source_file_location_id: Some(13),
        staging_path: "/var/lib/voom/staging/10".to_owned(),
        size_bytes: 4096,
        checksum: "blake3:abc123".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactStaged(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.staged");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactStaged(q) if q == p));
    assert_eq!(Event::ArtifactStaged(p).kind(), EventKind::ArtifactStaged);
}

#[test]
fn artifact_verification_started_payload_round_trip() {
    let p = ArtifactVerificationStartedPayload {
        artifact_handle_id: 10,
        artifact_location_id: 11,
        worker_id: 12,
        path: "/var/lib/voom/staging/10".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactVerificationStarted(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.verification_started");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactVerificationStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactVerificationStarted(p).kind(),
        EventKind::ArtifactVerificationStarted
    );
}

#[test]
fn artifact_verification_succeeded_payload_round_trip() {
    let p = ArtifactVerificationSucceededPayload {
        verification_id: 20,
        artifact_handle_id: 10,
        artifact_location_id: 11,
        worker_id: 12,
        observed_size_bytes: 4096,
        observed_checksum: "blake3:abc123".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactVerificationSucceeded(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.verification_succeeded");
    assert_eq!(json["payload"]["observed_size_bytes"], 4096);
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactVerificationSucceeded(q) if q == p));
    assert_eq!(
        Event::ArtifactVerificationSucceeded(p).kind(),
        EventKind::ArtifactVerificationSucceeded
    );
}

#[test]
fn artifact_verification_failed_payload_round_trip() {
    let p = ArtifactVerificationFailedPayload {
        verification_id: 20,
        artifact_handle_id: 10,
        artifact_location_id: 11,
        worker_id: 12,
        error_code: "ARTIFACT_CHECKSUM_MISMATCH".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactVerificationFailed(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.verification_failed");
    assert_eq!(json["payload"]["error_code"], "ARTIFACT_CHECKSUM_MISMATCH");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactVerificationFailed(q) if q == p));
    assert_eq!(
        Event::ArtifactVerificationFailed(p).kind(),
        EventKind::ArtifactVerificationFailed
    );
}

#[test]
fn artifact_transcode_started_payload_serializes_correlation_fields() {
    let p = ArtifactTranscodeStartedPayload {
        job_id: 1,
        ticket_id: 2,
        lease_id: Some(3),
        source_file_version_id: 4,
        source_file_location_id: 5,
        staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
        provider: Some("ffmpeg".to_owned()),
        provider_version: None,
    };

    let json = serde_json::to_value(Event::ArtifactTranscodeStarted(p.clone())).unwrap();

    assert_eq!(json["kind"], "artifact.transcode_started");
    assert_eq!(json["payload"]["job_id"], 1);
    assert_eq!(json["payload"]["ticket_id"], 2);
    assert_eq!(json["payload"]["lease_id"], 3);
    assert_eq!(json["payload"]["source_file_version_id"], 4);
    assert_eq!(json["payload"]["source_file_location_id"], 5);
    assert_eq!(
        json["payload"]["staging_path"],
        "/tmp/voom-stage/2/3/out.mkv"
    );
    assert_eq!(json["payload"]["provider"], "ffmpeg");
    assert_eq!(json["payload"]["provider_version"], serde_json::Value::Null);

    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactTranscodeStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactTranscodeStarted(p).kind(),
        EventKind::ArtifactTranscodeStarted
    );
}

#[test]
fn artifact_verification_succeeded_rejects_failure_shape() {
    let raw = serde_json::json!({
        "kind": "artifact.verification_succeeded",
        "payload": {
            "verification_id": 20,
            "artifact_handle_id": 10,
            "artifact_location_id": 11,
            "worker_id": 12,
            "error_code": "ARTIFACT_CHECKSUM_MISMATCH"
        }
    });
    let err = serde_json::from_value::<Event>(raw).unwrap_err();
    assert!(
        err.to_string().contains("observed_size_bytes"),
        "missing success facts should reject: {err}"
    );
}

#[test]
fn artifact_commit_started_payload_round_trip() {
    let p = ArtifactCommitStartedPayload {
        commit_record_id: 30,
        artifact_handle_id: 10,
        source_file_version_id: 12,
        verification_id: 20,
        target_path: "/media/final.bin".to_owned(),
        temp_path: "/media/.final.bin.tmp".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitStarted(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_started");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitStarted(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitStarted(p).kind(),
        EventKind::ArtifactCommitStarted
    );
}

#[test]
fn artifact_commit_completed_payload_round_trip() {
    let p = ArtifactCommitCompletedPayload {
        commit_record_id: 30,
        artifact_handle_id: 10,
        result_file_version_id: 31,
        result_file_location_id: 32,
        target_path: "/media/final.bin".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitCompleted(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_completed");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitCompleted(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitCompleted(p).kind(),
        EventKind::ArtifactCommitCompleted
    );
}

#[test]
fn artifact_commit_failed_pre_mutation_payload_round_trip() {
    let p = ArtifactCommitFailedPreMutationPayload {
        artifact_handle_id: 10,
        commit_record_id: None,
        target_path: "/media/final.bin".to_owned(),
        error_code: "ARTIFACT_NOT_VERIFIED".to_owned(),
        message: "staged artifact has no successful verification".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitFailedPreMutation(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_failed_pre_mutation");
    assert_eq!(json["payload"]["commit_record_id"], serde_json::Value::Null);
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitFailedPreMutation(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitFailedPreMutation(p).kind(),
        EventKind::ArtifactCommitFailedPreMutation
    );
}

#[test]
fn artifact_commit_recovery_required_payload_round_trip() {
    let p = ArtifactCommitRecoveryRequiredPayload {
        commit_record_id: 30,
        artifact_handle_id: 10,
        target_path: "/media/final.bin".to_owned(),
        temp_path: "/media/.final.bin.tmp".to_owned(),
        recovery_reason: "target_appeared_after_prepare".to_owned(),
        error_code: "TARGET_EXISTS".to_owned(),
        message: "target path appeared during promotion".to_owned(),
    };
    let json = serde_json::to_value(Event::ArtifactCommitRecoveryRequired(p.clone())).unwrap();
    assert_eq!(json["kind"], "artifact.commit_recovery_required");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::ArtifactCommitRecoveryRequired(q) if q == p));
    assert_eq!(
        Event::ArtifactCommitRecoveryRequired(p).kind(),
        EventKind::ArtifactCommitRecoveryRequired
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive Event list — a new variant must fail to compile here"
)]
fn event_kind_matches_serde_tag() {
    use time::OffsetDateTime;

    // Exhaustive list of Event variants. The compiler enforces this stays
    // in sync with `Event::kind()` — a new variant breaks the match there
    // and surfaces in CI. Any drift between `Event::kind().as_str()` and
    // the per-variant `#[serde(rename = "...")]` table also breaks here.
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
            kind: "k".to_owned(),
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
            kind: "local".to_owned(),
            status: "active".to_owned(),
            heartbeat_ttl_seconds: 60,
        }),
        Event::NodeHeartbeatRecorded(NodeHeartbeatRecordedPayload {
            node_id: 1,
            status: "active".to_owned(),
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
            kind: "synthetic".to_owned(),
        }),
        Event::WorkerLinkedToNode(WorkerLinkedToNodePayload {
            worker_id: 1,
            node_id: 1,
        }),
        Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
            worker_id: 1,
            capability_id: 1,
            operation: "op".to_owned(),
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
        Event::IssueOpened(issue_payload("planned")),
        Event::IssueUpdated(issue_payload("open")),
        Event::IssueResolved(issue_payload("resolved")),
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

#[test]
fn media_work_created_round_trips() {
    let p = MediaWorkCreatedPayload {
        media_work_id: 9,
        kind: "movie".to_owned(),
        display_title: "Solaris".to_owned(),
        provisional: true,
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: MediaWorkCreatedPayload = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn file_location_recorded_by_move_round_trips() {
    let p = FileLocationRecordedByMovePayload {
        retired_file_location_id: 1,
        new_file_location_id: 2,
        file_version_id: 3,
        kind: "local_path".to_owned(),
        value: "/srv/new".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: FileLocationRecordedByMovePayload = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn event_kind_matches_payload_for_identity_variants() {
    let e = Event::FileAssetCreated(FileAssetCreatedPayload { file_asset_id: 1 });
    assert_eq!(e.kind(), EventKind::FileAssetCreated);

    let e = Event::IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload {
        evidence_id: 99,
        target_type: "file_asset".to_owned(),
        target_id: 1,
        accepted_user_id: Some("alice".to_owned()),
        accepted_at: OffsetDateTime::UNIX_EPOCH,
    });
    assert_eq!(e.kind(), EventKind::IdentityEvidenceAccepted);
}

#[test]
fn event_dotted_tag_matches_event_kind_as_str_for_identity_variants() {
    let cases = [
        (
            Event::MediaWorkCreated(MediaWorkCreatedPayload {
                media_work_id: 1,
                kind: "movie".to_owned(),
                display_title: "X".to_owned(),
                provisional: true,
            }),
            "media_work.created",
        ),
        (
            Event::FileLocationAliased(FileLocationAliasedPayload {
                file_location_id: 1,
                file_version_id: 1,
                kind: "local_path".to_owned(),
                value: "/x".to_owned(),
            }),
            "file_location.aliased",
        ),
    ];
    for (event, expected_tag) in cases {
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], expected_tag);
    }
}

#[test]
fn use_lease_acquired_round_trip() {
    let p = UseLeaseAcquiredPayload {
        lease_id: 42,
        kind: "playback".to_owned(),
        scope_type: "asset".to_owned(),
        scope_id: 9,
        issuer_kind: "user".to_owned(),
        issuer_ref: "alice".to_owned(),
        blocking_mode: "blocking".to_owned(),
        ttl_bound: true,
        acquired_at: OffsetDateTime::UNIX_EPOCH,
        expires_at: Some(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60)),
    };
    let json = serde_json::to_value(Event::UseLeaseAcquired(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseAcquired(q) if q == p));
    assert_eq!(
        Event::UseLeaseAcquired(p).kind(),
        EventKind::UseLeaseAcquired
    );
}

#[test]
fn use_lease_released_round_trip() {
    let p = UseLeaseReleasedPayload {
        lease_id: 7,
        release_reason: "released".to_owned(),
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseReleased(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseReleased(q) if q == p));
    assert_eq!(
        Event::UseLeaseReleased(p).kind(),
        EventKind::UseLeaseReleased
    );
}

#[test]
fn use_lease_expired_round_trip() {
    let p = UseLeaseExpiredPayload {
        lease_id: 7,
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseExpired(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseExpired(q) if q == p));
    assert_eq!(Event::UseLeaseExpired(p).kind(), EventKind::UseLeaseExpired);
}

#[test]
fn use_lease_force_released_round_trip() {
    let p = UseLeaseForceReleasedPayload {
        lease_id: 7,
        actor: "operator-jane".to_owned(),
        reason: "stuck blocking commit".to_owned(),
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseForceReleased(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseForceReleased(q) if q == p));
    assert_eq!(
        Event::UseLeaseForceReleased(p).kind(),
        EventKind::UseLeaseForceReleased
    );
}

#[test]
fn use_lease_recovered_stale_issuer_round_trip() {
    let p = UseLeaseRecoveredStaleIssuerPayload {
        lease_id: 7,
        actor: "operator-jane".to_owned(),
        reason: "worker host gone".to_owned(),
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseRecoveredStaleIssuer(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseRecoveredStaleIssuer(q) if q == p));
    assert_eq!(
        Event::UseLeaseRecoveredStaleIssuer(p).kind(),
        EventKind::UseLeaseRecoveredStaleIssuer
    );
}

#[test]
fn commit_intent_recorded_round_trip() {
    let p = CommitIntentRecordedPayload {
        commit_id: voom_core::CommitId(11),
        target_kind: "delete_file_location".to_owned(),
        closure_asset_count: 1,
        closure_bundle_count: 0,
        closure_version_count: 1,
        closure_location_count: 1,
        accepted_evidence_count: 0,
        started_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitIntentRecorded(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.intent_recorded");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitIntentRecorded(q) if q == p));
    assert_eq!(
        Event::CommitIntentRecorded(p).kind(),
        EventKind::CommitIntentRecorded
    );
}

#[test]
fn commit_aborted_by_use_lease_round_trip() {
    let p = CommitAbortedByUseLeasePayload {
        commit_id: voom_core::CommitId(12),
        lease_id: voom_core::UseLeaseId(3),
        lease_scope_type: "version".to_owned(),
        lease_scope_id: 99,
        phase: "prepare".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByUseLease(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_use_lease");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByUseLease(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByUseLease(p).kind(),
        EventKind::CommitAbortedByUseLease
    );
}

#[test]
fn commit_aborted_by_stale_evidence_round_trip() {
    let p = CommitAbortedByStaleEvidencePayload {
        commit_id: voom_core::CommitId(13),
        evidence_id: voom_core::EvidenceId(7),
        drift_kind: "pinned_hash_differs".to_owned(),
        phase: "prepare".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByStaleEvidence(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_stale_evidence");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByStaleEvidence(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByStaleEvidence(p).kind(),
        EventKind::CommitAbortedByStaleEvidence
    );
}

#[test]
fn commit_aborted_by_closure_incomplete_round_trip() {
    let p = CommitAbortedByClosureIncompletePayload {
        commit_id: voom_core::CommitId(14),
        phase: "prepare".to_owned(),
        message: "mount /srv/media offline".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByClosureIncomplete(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_closure_incomplete");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByClosureIncomplete(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByClosureIncomplete(p).kind(),
        EventKind::CommitAbortedByClosureIncomplete
    );
}

#[test]
fn commit_aborted_by_pending_commit_round_trip() {
    let p = CommitAbortedByPendingCommitPayload {
        commit_id: voom_core::CommitId(21),
        pending_commit_id: voom_core::CommitId(20),
        scope_type: "location".to_owned(),
        scope_id: 99,
        phase: "prepare".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByPendingCommit(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_pending_commit");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByPendingCommit(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByPendingCommit(p).kind(),
        EventKind::CommitAbortedByPendingCommit
    );
}

#[test]
fn commit_authorized_round_trip() {
    let p = CommitAuthorizedPayload {
        commit_id: voom_core::CommitId(21),
        closure_asset_count: 1,
        closure_bundle_count: 0,
        closure_version_count: 1,
        closure_location_count: 2,
        target_row_epoch_count: 4,
        authorized_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAuthorized(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.authorized");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAuthorized(q) if q == p));
    assert_eq!(
        Event::CommitAuthorized(p).kind(),
        EventKind::CommitAuthorized
    );
}

#[test]
fn commit_aborted_by_closure_grew_round_trip() {
    let p = CommitAbortedByClosureGrewPayload {
        commit_id: voom_core::CommitId(22),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 1,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 1,
        phase: "authorize".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedByClosureGrew(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_by_closure_grew");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedByClosureGrew(q) if q == p));
    assert_eq!(
        Event::CommitAbortedByClosureGrew(p).kind(),
        EventKind::CommitAbortedByClosureGrew
    );
}

#[test]
fn commit_completed_round_trip() {
    let p = CommitCompletedPayload {
        commit_id: voom_core::CommitId(31),
        target_kind: "delete_file_location".to_owned(),
        closure_asset_count: 1,
        closure_bundle_count: 0,
        closure_version_count: 1,
        closure_location_count: 1,
        finalized_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitCompleted(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.completed");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitCompleted(q) if q == p));
    assert_eq!(Event::CommitCompleted(p).kind(), EventKind::CommitCompleted);
}

#[test]
fn commit_aborted_pre_mutation_round_trip_carries_prior_state() {
    // Two emission sites — `prior_state` distinguishes them so a single
    // event kind covers both abort entry points.
    let p_pending = CommitAbortedPreMutationPayload {
        commit_id: voom_core::CommitId(32),
        prior_state: "pending".to_owned(),
        reason: "operator_cancel".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPreMutation(p_pending.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_pre_mutation");
    assert_eq!(json["payload"]["prior_state"], "pending");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPreMutation(q) if q == p_pending));

    let p_authorized = CommitAbortedPreMutationPayload {
        commit_id: voom_core::CommitId(33),
        prior_state: "authorized".to_owned(),
        reason: "operator_cancel".to_owned(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPreMutation(p_authorized.clone())).unwrap();
    assert_eq!(json["payload"]["prior_state"], "authorized");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPreMutation(q) if q == p_authorized));
    assert_eq!(
        Event::CommitAbortedPreMutation(p_authorized).kind(),
        EventKind::CommitAbortedPreMutation
    );
}

#[test]
fn commit_aborted_post_mutation_round_trip_unified_schema() {
    let p = CommitAbortedPostMutationPayload {
        commit_id: voom_core::CommitId(34),
        reason: "closure_grew_and_fresh_lease".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 1,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: vec![7, 9],
        target_epoch_drift: Vec::new(),
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPostMutation(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.aborted_post_mutation");
    // Both arrays must be present on every payload (unified schema) —
    // a closure-grew firing carries an empty `fresh_lease_ids`; a
    // fresh-lease firing carries empty `added_*`/`removed_*`.
    assert!(json["payload"]["fresh_lease_ids"].is_array());
    assert!(json["payload"]["target_epoch_drift"].is_array());
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPostMutation(q) if q == p));
    assert_eq!(
        Event::CommitAbortedPostMutation(p).kind(),
        EventKind::CommitAbortedPostMutation
    );
}

#[test]
fn commit_aborted_post_mutation_stale_target_epoch_carries_drift_array() {
    let p = CommitAbortedPostMutationPayload {
        commit_id: voom_core::CommitId(35),
        reason: "stale_target_epoch".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 0,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: Vec::new(),
        target_epoch_drift: vec![TargetEpochDriftWire {
            kind: "file_location".to_owned(),
            id: 17,
            expected: 4,
            observed: 5,
        }],
        aborted_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitAbortedPostMutation(p.clone())).unwrap();
    assert_eq!(json["payload"]["reason"], "stale_target_epoch");
    assert_eq!(
        json["payload"]["target_epoch_drift"][0]["kind"],
        "file_location"
    );
    assert_eq!(json["payload"]["target_epoch_drift"][0]["id"], 17);
    assert_eq!(json["payload"]["target_epoch_drift"][0]["expected"], 4);
    assert_eq!(json["payload"]["target_epoch_drift"][0]["observed"], 5);
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitAbortedPostMutation(q) if q == p));
}

#[test]
fn commit_forced_override_round_trip() {
    let p = CommitForcedOverridePayload {
        commit_id: voom_core::CommitId(40),
        actor: "ops@example.com".to_owned(),
        reason: "fs mount offline; out-of-band confirmed".to_owned(),
        bypass: vec!["closure_incomplete".to_owned()],
        recorded_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitForcedOverride(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.forced_override");
    assert_eq!(json["payload"]["actor"], "ops@example.com");
    assert_eq!(json["payload"]["bypass"][0], "closure_incomplete");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitForcedOverride(q) if q == p));
    assert_eq!(
        Event::CommitForcedOverride(p).kind(),
        EventKind::CommitForcedOverride
    );
}

#[test]
fn commit_recovery_required_round_trip_mirrors_post_mutation_fields() {
    let p = CommitRecoveryRequiredPayload {
        commit_id: voom_core::CommitId(36),
        recovery_reason: "stale_target_epoch".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 0,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: Vec::new(),
        target_epoch_drift: vec![TargetEpochDriftWire {
            kind: "file_version".to_owned(),
            id: 7,
            expected: 1,
            observed: 2,
        }],
        recorded_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::CommitRecoveryRequired(p.clone())).unwrap();
    assert_eq!(json["kind"], "commit.recovery_required");
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::CommitRecoveryRequired(q) if q == p));
    assert_eq!(
        Event::CommitRecoveryRequired(p).kind(),
        EventKind::CommitRecoveryRequired
    );
}

#[test]
fn use_lease_reanchored_by_move_round_trip() {
    let p = UseLeaseReanchoredByMovePayload {
        lease_id: 7,
        retired_location_id: 99,
        new_location_id: 100,
        reanchored_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseReanchoredByMove(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseReanchoredByMove(q) if q == p));
    assert_eq!(
        Event::UseLeaseReanchoredByMove(p).kind(),
        EventKind::UseLeaseReanchoredByMove
    );
}
