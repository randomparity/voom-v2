use super::*;

use time::OffsetDateTime;

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
        next_eligible_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&Event::TicketFailedRetriable(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::TicketFailedRetriable(p), back);
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
            next_eligible_at: OffsetDateTime::UNIX_EPOCH,
        }),
        Event::TicketFailedTerminal(TicketFailedTerminalPayload {
            ticket_id: 1,
            attempt: 3,
            max_attempts: 3,
            reason: "r".to_owned(),
        }),
        Event::TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload {
            ticket_id: 1,
            lease_id: 1,
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
