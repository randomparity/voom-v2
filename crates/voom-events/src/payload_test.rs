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
        Event::WorkerRegistered(WorkerRegisteredPayload {
            worker_id: 1,
            name: "w".to_owned(),
            kind: "synthetic".to_owned(),
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
