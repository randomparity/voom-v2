use time::OffsetDateTime;
use voom_control_plane::scan::{ScanReconciliationEvidence, ScanSession};
use voom_core::{
    FileLocationId, NodeId, NodeIncarnationId, ScanSessionId, ScanSessionStatus,
    ScanTerminalReason, StorageRootId,
};

use super::{ReconciliationData, SessionData};

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn session_data_exposes_complete_public_progress_with_iso_timestamps() {
    let session = session(ScanSessionStatus::Succeeded);

    let json = serde_json::to_value(SessionData::from(session)).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "id": 9,
            "storage_root_id": 7,
            "root_epoch": 3,
            "owner_node_id": 5,
            "owner_incarnation_id": INCARNATION,
            "status": "succeeded",
            "next_sequence": 2,
            "batch_count": 2,
            "observation_count": 4,
            "idle_timeout_seconds": 300,
            "progress_deadline_at": "1970-01-01T00:05:00.000000000Z",
            "location_high_watermark_id": 101,
            "requested_at": "1970-01-01T00:00:00.000000000Z",
            "started_at": "1970-01-01T00:00:10.000000000Z",
            "terminal_at": "1970-01-01T00:00:20.000000000Z",
            "terminal_reason": null,
            "retired_location_count": 2,
            "reconciliation_applied": true,
        })
    );
}

#[test]
fn reconciliation_applied_is_true_only_for_succeeded_sessions() {
    for status in [
        ScanSessionStatus::Requested,
        ScanSessionStatus::Running,
        ScanSessionStatus::Failed,
        ScanSessionStatus::Cancelled,
        ScanSessionStatus::Stale,
    ] {
        assert!(!SessionData::from(session(status)).reconciliation_applied);
    }
    assert!(SessionData::from(session(ScanSessionStatus::Succeeded)).reconciliation_applied);
}

#[test]
fn reconciliation_data_exposes_only_public_retirement_evidence() {
    let evidence = ScanReconciliationEvidence {
        file_location_id: FileLocationId(101),
        retired_at: OffsetDateTime::from_unix_timestamp(20).unwrap(),
        prior_epoch: 6,
        retired_epoch: 7,
    };

    let json = serde_json::to_value(ReconciliationData::from(evidence)).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "file_location_id": 101,
            "retired_at": "1970-01-01T00:00:20.000000000Z",
            "prior_epoch": 6,
            "retired_epoch": 7,
        })
    );
    assert!(json.get("provider_relative_locator").is_none());
    assert!(json.get("provider_object_identity").is_none());
}

fn session(status: ScanSessionStatus) -> ScanSession {
    let terminal = match status {
        ScanSessionStatus::Succeeded => (Some(at(20)), None),
        ScanSessionStatus::Failed | ScanSessionStatus::Cancelled | ScanSessionStatus::Stale => (
            Some(at(20)),
            Some(ScanTerminalReason::new("terminal reason").unwrap()),
        ),
        ScanSessionStatus::Requested | ScanSessionStatus::Running => (None, None),
    };
    let running = status != ScanSessionStatus::Requested;
    ScanSession {
        id: ScanSessionId(9),
        storage_root_id: StorageRootId(7),
        root_epoch: 3,
        owner_node_id: NodeId(5),
        owner_incarnation_id: running.then(|| INCARNATION.parse::<NodeIncarnationId>().unwrap()),
        status,
        next_sequence: 2,
        batch_count: 2,
        observation_count: 4,
        idle_timeout_seconds: 300,
        progress_deadline_at: at(300),
        location_high_watermark_id: running.then_some(FileLocationId(101)),
        requested_at: at(0),
        started_at: running.then(|| at(10)),
        terminal_at: terminal.0,
        terminal_reason: terminal.1,
        retired_location_count: u64::from(status == ScanSessionStatus::Succeeded) * 2,
    }
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).unwrap()
}
