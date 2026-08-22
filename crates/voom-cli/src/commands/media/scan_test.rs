//! Wire-shape tests for the request-and-wait scan command.

use serde_json::json;

use super::{BlockedData, ScanOutcomeData, ScanRequestData, is_terminal};
use voom_control_plane::scan::{RootBlockReason, RootScanBlocked};
use voom_core::{LibraryId, ScanSessionStatus, StorageRootId};

#[test]
fn scan_request_data_serializes_session_and_ticket_ids() {
    let value = serde_json::to_value(ScanRequestData {
        scan_session_id: 42,
        ticket_id: 7,
    })
    .unwrap();

    assert_eq!(
        value,
        json!({
            "scan_session_id": 42,
            "ticket_id": 7,
        })
    );
}

#[test]
fn scan_outcome_data_serializes_terminal_state_and_counters() {
    let value = serde_json::to_value(ScanOutcomeData {
        scan_session_id: 42,
        status: "succeeded".to_owned(),
        observation_count: 12,
        retired_location_count: 3,
    })
    .unwrap();

    assert_eq!(
        value,
        json!({
            "scan_session_id": 42,
            "status": "succeeded",
            "observation_count": 12,
            "retired_location_count": 3,
        })
    );
}

#[test]
fn blocked_data_serializes_the_refused_root() {
    let value = serde_json::to_value(BlockedData::from(RootScanBlocked {
        library_id: LibraryId(5),
        storage_root_id: StorageRootId(9),
        reason: RootBlockReason::RootDisabled,
        provider_locator: "/media/root".to_owned(),
    }))
    .unwrap();

    assert_eq!(
        value,
        json!({
            "status": "blocked",
            "reason": "root_disabled",
            "library_id": 5,
            "storage_root_id": 9,
            "provider_locator": "/media/root",
        })
    );
}

#[test]
fn only_terminal_statuses_end_the_wait_loop() {
    assert!(!is_terminal(ScanSessionStatus::Requested));
    assert!(!is_terminal(ScanSessionStatus::Running));
    assert!(is_terminal(ScanSessionStatus::Succeeded));
    assert!(is_terminal(ScanSessionStatus::Failed));
    assert!(is_terminal(ScanSessionStatus::Cancelled));
    assert!(is_terminal(ScanSessionStatus::Stale));
}
