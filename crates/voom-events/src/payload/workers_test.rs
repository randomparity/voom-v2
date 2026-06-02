use super::*;
use crate::payload::{Event, EventKind};
use time::OffsetDateTime;
use voom_core::{NodeKind, NodeStatus};
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
