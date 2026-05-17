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
