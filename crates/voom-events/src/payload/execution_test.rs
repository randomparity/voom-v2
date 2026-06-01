use super::*;
use crate::payload::Event;
use time::OffsetDateTime;
use voom_core::FailureClass;
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
