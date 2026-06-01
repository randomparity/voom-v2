use super::*;
use crate::payload::{Event, EventKind};
use time::OffsetDateTime;
#[test]
fn media_work_created_round_trips() {
    let p = MediaWorkCreatedPayload {
        media_work_id: 9,
        kind: "movie".to_owned(),
        display_title: "Solaris".to_owned(),
        provisional: true,
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: MediaWorkCreatedPayload = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn file_location_recorded_by_move_round_trips() {
    let p = FileLocationRecordedByMovePayload {
        retired_file_location_id: 1,
        new_file_location_id: 2,
        file_version_id: 3,
        kind: "local_path".to_owned(),
        value: "/srv/new".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: FileLocationRecordedByMovePayload = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn event_kind_matches_payload_for_identity_variants() {
    let e = Event::FileAssetCreated(FileAssetCreatedPayload { file_asset_id: 1 });
    assert_eq!(e.kind(), EventKind::FileAssetCreated);

    let e = Event::IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload {
        evidence_id: 99,
        target_type: "file_asset".to_owned(),
        target_id: 1,
        accepted_user_id: Some("alice".to_owned()),
        accepted_at: OffsetDateTime::UNIX_EPOCH,
    });
    assert_eq!(e.kind(), EventKind::IdentityEvidenceAccepted);
}

#[test]
fn event_dotted_tag_matches_event_kind_as_str_for_identity_variants() {
    let cases = [
        (
            Event::MediaWorkCreated(MediaWorkCreatedPayload {
                media_work_id: 1,
                kind: "movie".to_owned(),
                display_title: "X".to_owned(),
                provisional: true,
            }),
            "media_work.created",
        ),
        (
            Event::FileLocationAliased(FileLocationAliasedPayload {
                file_location_id: 1,
                file_version_id: 1,
                kind: "local_path".to_owned(),
                value: "/x".to_owned(),
            }),
            "file_location.aliased",
        ),
    ];
    for (event, expected_tag) in cases {
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], expected_tag);
    }
}
