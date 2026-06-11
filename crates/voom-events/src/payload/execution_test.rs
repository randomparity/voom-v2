use super::*;
use crate::payload::Event;
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use voom_core::FailureClass;

/// Assert that `valid` round-trips and that injecting a top-level unknown field
/// is rejected by `#[serde(deny_unknown_fields)]`.
fn assert_rejects_unknown<T: Serialize + DeserializeOwned>(valid: &T) {
    let base = serde_json::to_value(valid).unwrap();
    assert!(
        serde_json::from_value::<T>(base.clone()).is_ok(),
        "base instance should deserialize: {base}"
    );
    let mut tampered = base;
    tampered
        .as_object_mut()
        .expect("payload struct serializes to a JSON object")
        .insert("__unknown".to_owned(), serde_json::json!(true));
    assert!(
        serde_json::from_value::<T>(tampered).is_err(),
        "unknown top-level field must be rejected"
    );
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
fn job_opened_payload_rejects_unknown_field() {
    assert_rejects_unknown(&JobOpenedPayload {
        job_id: 1,
        kind: "transcode".to_owned(),
        priority: 5,
    });
}

#[test]
fn job_succeeded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&JobSucceededPayload { job_id: 1 });
}

#[test]
fn job_failed_payload_rejects_unknown_field() {
    assert_rejects_unknown(&JobFailedPayload {
        job_id: 1,
        reason: "worker crashed".to_owned(),
    });
}

#[test]
fn job_cancelled_payload_rejects_unknown_field() {
    assert_rejects_unknown(&JobCancelledPayload {
        job_id: 1,
        reason: "operator cancel".to_owned(),
    });
}

#[test]
fn ticket_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketCreatedPayload {
        ticket_id: 1,
        job_id: Some(2),
        kind: voom_core::TicketOperation::new("synthetic.workflow.operation.hash_file").unwrap(),
        priority: 5,
        max_attempts: 3,
    });
}

#[test]
fn ticket_ready_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketReadyPayload { ticket_id: 1 });
}

#[test]
fn ticket_leased_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketLeasedPayload {
        ticket_id: 1,
        lease_id: 2,
        worker_id: 3,
        attempt: 1,
    });
}

#[test]
fn ticket_succeeded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketSucceededPayload {
        ticket_id: 1,
        lease_id: 2,
    });
}

#[test]
fn ticket_failed_retriable_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketFailedRetriablePayload {
        ticket_id: 5,
        attempt: 1,
        max_attempts: 3,
        reason: "transient sqlite lock".to_owned(),
        class: FailureClass::WorkerTimeout,
        next_eligible_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn ticket_failed_terminal_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketFailedTerminalPayload {
        ticket_id: 7,
        attempt: 3,
        max_attempts: 3,
        reason: "retries exhausted".to_owned(),
        class: FailureClass::WorkerCrash,
        issue_id: None,
    });
}

#[test]
fn ticket_requeued_after_lease_expiry_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketRequeuedAfterLeaseExpiryPayload {
        ticket_id: 1,
        lease_id: 2,
    });
}

#[test]
fn ticket_requeued_after_force_release_payload_rejects_unknown_field() {
    assert_rejects_unknown(&TicketRequeuedAfterForceReleasePayload {
        ticket_id: 3,
        lease_id: 9,
        actor: "alice".to_owned(),
        reason: "stuck worker".to_owned(),
    });
}

#[test]
fn lease_acquired_payload_rejects_unknown_field() {
    assert_rejects_unknown(&LeaseAcquiredPayload {
        lease_id: 1,
        ticket_id: 2,
        worker_id: 3,
        ttl_seconds: 60,
        expires_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn lease_released_payload_rejects_unknown_field() {
    assert_rejects_unknown(&LeaseReleasedPayload {
        lease_id: 1,
        ticket_id: 2,
        release_reason: "completed".to_owned(),
    });
}

#[test]
fn lease_expired_payload_rejects_unknown_field() {
    assert_rejects_unknown(&LeaseExpiredPayload {
        lease_id: 1,
        ticket_id: 2,
    });
}

#[test]
fn lease_force_released_payload_rejects_unknown_field() {
    assert_rejects_unknown(&LeaseForceReleasedPayload {
        lease_id: 9,
        ticket_id: 5,
        actor: "alice".to_owned(),
        reason: "clearing stuck lease".to_owned(),
        also_requeue: true,
    });
}
