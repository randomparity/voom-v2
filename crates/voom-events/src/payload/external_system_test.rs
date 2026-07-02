use super::*;
use crate::payload::Event;
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

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
fn registered_payload_round_trip_through_event() {
    let p = ExternalSystemRegisteredPayload {
        external_system_id: 3,
        kind: "filesystem".to_owned(),
        display_name: "local media".to_owned(),
        health_status: "unknown".to_owned(),
    };
    let json = serde_json::to_string(&Event::ExternalSystemRegistered(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::ExternalSystemRegistered(p), back);
}

#[test]
fn health_changed_payload_round_trip_through_event() {
    let p = ExternalSystemHealthChangedPayload {
        external_system_id: 3,
        previous: "unknown".to_owned(),
        current: "healthy".to_owned(),
    };
    let json = serde_json::to_string(&Event::ExternalSystemHealthChanged(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::ExternalSystemHealthChanged(p), back);
}

#[test]
fn linked_and_unlinked_payloads_round_trip_through_event() {
    let linked = ExternalSystemLinkedPayload {
        external_system_id: 3,
        link_id: 8,
        target_type: "media_work".to_owned(),
        target_id: 42,
        external_ref: "plex://library/metadata/1".to_owned(),
    };
    let json = serde_json::to_string(&Event::ExternalSystemLinked(linked.clone())).unwrap();
    assert_eq!(
        serde_json::from_str::<Event>(&json).unwrap(),
        Event::ExternalSystemLinked(linked)
    );
    let unlinked = ExternalSystemUnlinkedPayload {
        external_system_id: 3,
        link_id: 8,
        target_type: "media_work".to_owned(),
        target_id: 42,
        external_ref: "plex://library/metadata/1".to_owned(),
    };
    let json = serde_json::to_string(&Event::ExternalSystemUnlinked(unlinked.clone())).unwrap();
    assert_eq!(
        serde_json::from_str::<Event>(&json).unwrap(),
        Event::ExternalSystemUnlinked(unlinked)
    );
}

#[test]
fn synced_payload_round_trip_through_event() {
    let p = ExternalSystemSyncedPayload {
        external_system_id: 3,
        outcome: "ok".to_owned(),
        links_recorded: 2,
        links_retired: 1,
        started_at: OffsetDateTime::UNIX_EPOCH,
        finished_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&Event::ExternalSystemSynced(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::ExternalSystemSynced(p), back);
}

#[test]
fn payloads_reject_unknown_fields() {
    assert_rejects_unknown(&ExternalSystemRegisteredPayload {
        external_system_id: 3,
        kind: "filesystem".to_owned(),
        display_name: "local media".to_owned(),
        health_status: "unknown".to_owned(),
    });
    assert_rejects_unknown(&ExternalSystemHealthChangedPayload {
        external_system_id: 3,
        previous: "unknown".to_owned(),
        current: "healthy".to_owned(),
    });
    assert_rejects_unknown(&ExternalSystemSyncedPayload {
        external_system_id: 3,
        outcome: "ok".to_owned(),
        links_recorded: 0,
        links_retired: 0,
        started_at: OffsetDateTime::UNIX_EPOCH,
        finished_at: OffsetDateTime::UNIX_EPOCH,
    });
}
