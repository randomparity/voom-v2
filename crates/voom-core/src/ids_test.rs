use super::*;

#[test]
fn ids_serialize_as_bare_numbers() {
    let id = JobId(42);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "42");
}

#[test]
fn ids_round_trip_through_json() {
    let id = TicketId(7);
    let json = serde_json::to_string(&id).unwrap();
    let back: TicketId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn artifact_handle_id_displays_inner_u64() {
    let id = ArtifactHandleId(42);
    assert_eq!(id.to_string(), "42");
}

#[test]
fn artifact_location_id_displays_inner_u64() {
    let id = ArtifactLocationId(7);
    assert_eq!(id.to_string(), "7");
}

#[test]
fn media_work_id_displays_inner_u64() {
    assert_eq!(MediaWorkId(1).to_string(), "1");
}

#[test]
fn media_variant_id_displays_inner_u64() {
    assert_eq!(MediaVariantId(2).to_string(), "2");
}

#[test]
fn bundle_id_displays_inner_u64() {
    assert_eq!(BundleId(3).to_string(), "3");
}

#[test]
fn file_asset_id_displays_inner_u64() {
    assert_eq!(FileAssetId(4).to_string(), "4");
}

#[test]
fn file_version_id_displays_inner_u64() {
    assert_eq!(FileVersionId(5).to_string(), "5");
}

#[test]
fn file_location_id_displays_inner_u64() {
    assert_eq!(FileLocationId(6).to_string(), "6");
}

#[test]
fn evidence_id_displays_inner_u64() {
    assert_eq!(EvidenceId(7).to_string(), "7");
}

#[test]
fn media_snapshot_id_displays_inner_u64() {
    assert_eq!(MediaSnapshotId(8).to_string(), "8");
}

#[test]
fn use_lease_id_displays_inner_u64() {
    assert_eq!(UseLeaseId(42).to_string(), "42");
}
