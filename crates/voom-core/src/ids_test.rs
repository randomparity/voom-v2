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
