use super::*;
use crate::payload::{Event, EventKind};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use voom_core::{NodeKind, NodeStatus, TicketOperation, WorkerKind};

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
fn node_registered_payload_round_trip() {
    let p = NodeRegisteredPayload {
        node_id: 42,
        name: "node-a".to_owned(),
        kind: NodeKind::Local,
        status: NodeStatus::Active,
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
        status: NodeStatus::Active,
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
fn node_registered_payload_rejects_unknown_field() {
    assert_rejects_unknown(&NodeRegisteredPayload {
        node_id: 42,
        name: "node-a".to_owned(),
        kind: NodeKind::Local,
        status: NodeStatus::Active,
        heartbeat_ttl_seconds: 30,
    });
}

#[test]
fn node_heartbeat_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&NodeHeartbeatRecordedPayload {
        node_id: 42,
        status: NodeStatus::Active,
        last_seen_at: OffsetDateTime::UNIX_EPOCH,
        epoch: 7,
    });
}

#[test]
fn node_marked_stale_payload_rejects_unknown_field() {
    assert_rejects_unknown(&NodeMarkedStalePayload {
        node_id: 42,
        marked_stale_at: OffsetDateTime::UNIX_EPOCH,
        epoch: 8,
    });
}

#[test]
fn node_retired_payload_rejects_unknown_field() {
    assert_rejects_unknown(&NodeRetiredPayload {
        node_id: 42,
        retired_at: OffsetDateTime::UNIX_EPOCH,
        epoch: 9,
    });
}

#[test]
fn worker_registered_payload_rejects_unknown_field() {
    assert_rejects_unknown(&WorkerRegisteredPayload {
        worker_id: 7,
        name: "worker-a".to_owned(),
        kind: WorkerKind::Local,
    });
}

#[test]
fn worker_linked_to_node_payload_rejects_unknown_field() {
    assert_rejects_unknown(&WorkerLinkedToNodePayload {
        worker_id: 7,
        node_id: 42,
    });
}

#[test]
fn worker_capability_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&WorkerCapabilityRecordedPayload {
        worker_id: 7,
        capability_id: 3,
        operation: TicketOperation::new("synthetic.workflow.operation.hash_file").unwrap(),
    });
}

#[test]
fn worker_grant_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&WorkerGrantRecordedPayload {
        worker_id: 7,
        grant_id: 5,
    });
}

#[test]
fn worker_retired_payload_rejects_unknown_field() {
    assert_rejects_unknown(&WorkerRetiredPayload { worker_id: 7 });
}
