use serde::Serialize;
use serde::de::DeserializeOwned;
use voom_core::{ScanSessionId, ScanSessionStatus, StorageRootId};

use super::*;
use crate::{Event, EventKind};

fn rejects_unknown<T: Serialize + DeserializeOwned>(value: &T) {
    let mut json = serde_json::to_value(value).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("unknown".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<T>(json).is_err());
}

#[test]
fn scan_session_events_use_exact_kinds_and_round_trip_json() {
    let lifecycle = ScanSessionLifecyclePayload {
        scan_session_id: ScanSessionId(1),
        storage_root_id: StorageRootId(2),
        status: ScanSessionStatus::Requested,
    };
    let events = [
        (
            Event::ScanSessionRequested(lifecycle.clone()),
            "scan_session.requested",
        ),
        (
            Event::ScanSessionStarted(ScanSessionLifecyclePayload {
                status: ScanSessionStatus::Running,
                ..lifecycle.clone()
            }),
            "scan_session.started",
        ),
        (
            Event::ScanObservationBatchAccepted(ScanObservationBatchAcceptedPayload {
                scan_session_id: ScanSessionId(1),
                sequence: 2,
                batch_observation_count: 3,
                cumulative_observation_count: 5,
            }),
            "scan_session.observation_batch_accepted",
        ),
        (
            Event::ScanSessionSucceeded(ScanSessionSucceededPayload {
                scan_session_id: ScanSessionId(1),
                storage_root_id: StorageRootId(2),
                observation_count: 5,
                retired_location_count: 1,
            }),
            "scan_session.succeeded",
        ),
        (
            Event::ScanSessionFailed(ScanSessionLifecyclePayload {
                status: ScanSessionStatus::Failed,
                ..lifecycle.clone()
            }),
            "scan_session.failed",
        ),
        (
            Event::ScanSessionCancelled(ScanSessionLifecyclePayload {
                status: ScanSessionStatus::Cancelled,
                ..lifecycle.clone()
            }),
            "scan_session.cancelled",
        ),
        (
            Event::ScanSessionStale(ScanSessionLifecyclePayload {
                status: ScanSessionStatus::Stale,
                ..lifecycle
            }),
            "scan_session.stale",
        ),
    ];

    for (event, wire) in events {
        assert_eq!(event.kind().as_str(), wire);
        assert_eq!(EventKind::from_str(wire).unwrap(), event.kind());
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], wire);
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
    }
}

#[test]
fn scan_payloads_reject_unknown_fields() {
    rejects_unknown(&ScanSessionLifecyclePayload {
        scan_session_id: ScanSessionId(1),
        storage_root_id: StorageRootId(2),
        status: ScanSessionStatus::Running,
    });
    rejects_unknown(&ScanObservationBatchAcceptedPayload {
        scan_session_id: ScanSessionId(1),
        sequence: 2,
        batch_observation_count: 3,
        cumulative_observation_count: 5,
    });
    rejects_unknown(&ScanSessionSucceededPayload {
        scan_session_id: ScanSessionId(1),
        storage_root_id: StorageRootId(2),
        observation_count: 5,
        retired_location_count: 1,
    });
}
