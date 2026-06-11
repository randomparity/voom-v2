use super::*;
use crate::payload::{Event, EventKind};
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

#[test]
fn media_work_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&MediaWorkCreatedPayload {
        media_work_id: 1,
        kind: "movie".to_owned(),
        display_title: "Example".to_owned(),
        provisional: false,
    });
}

#[test]
fn media_variant_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&MediaVariantCreatedPayload {
        media_variant_id: 1,
        media_work_id: 2,
        label: "1080p".to_owned(),
        provisional: false,
    });
}

#[test]
fn asset_bundle_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&AssetBundleCreatedPayload {
        bundle_id: 1,
        media_variant_id: 2,
        display_name: "Main".to_owned(),
    });
}

#[test]
fn asset_bundle_member_added_payload_rejects_unknown_field() {
    assert_rejects_unknown(&AssetBundleMemberAddedPayload {
        bundle_id: 1,
        file_asset_id: 2,
        role: "video".to_owned(),
    });
}

#[test]
fn asset_bundle_member_removed_payload_rejects_unknown_field() {
    assert_rejects_unknown(&AssetBundleMemberRemovedPayload {
        bundle_id: 1,
        file_asset_id: 2,
        role: "video".to_owned(),
    });
}

#[test]
fn file_asset_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileAssetCreatedPayload { file_asset_id: 1 });
}

#[test]
fn file_version_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileVersionCreatedPayload {
        file_version_id: 1,
        file_asset_id: 2,
        content_hash: "blake3:abc".to_owned(),
        size_bytes: 4096,
        produced_by: "ingest".to_owned(),
        produced_from_version_id: None,
    });
}

#[test]
fn file_location_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationRecordedPayload {
        file_location_id: 1,
        file_version_id: 2,
        kind: "filesystem".to_owned(),
        value: "/media/x".to_owned(),
    });
}

#[test]
fn file_location_aliased_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationAliasedPayload {
        file_location_id: 1,
        file_version_id: 2,
        kind: "filesystem".to_owned(),
        value: "/media/y".to_owned(),
    });
}

#[test]
fn file_location_retired_by_move_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationRetiredByMovePayload {
        file_location_id: 1,
        file_version_id: 2,
        retired_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn file_location_recorded_by_move_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationRecordedByMovePayload {
        retired_file_location_id: 1,
        new_file_location_id: 2,
        file_version_id: 3,
        kind: "filesystem".to_owned(),
        value: "/media/z".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn identity_evidence_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&IdentityEvidenceRecordedPayload {
        evidence_id: 1,
        target_type: "file_version".to_owned(),
        target_id: 2,
        assertion_type: "checksum".to_owned(),
        provider: "ingest".to_owned(),
        provider_version: "1.0".to_owned(),
        confidence: 0.9,
        observed_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn identity_evidence_accepted_payload_rejects_unknown_field() {
    assert_rejects_unknown(&IdentityEvidenceAcceptedPayload {
        evidence_id: 1,
        target_type: "file_version".to_owned(),
        target_id: 2,
        accepted_user_id: Some("alice".to_owned()),
        accepted_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn identity_evidence_superseded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&IdentityEvidenceSupersededPayload {
        superseded_evidence_id: 1,
        superseded_by_evidence_id: 2,
        target_type: "file_version".to_owned(),
        target_id: 3,
        superseded_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn media_snapshot_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&MediaSnapshotRecordedPayload {
        media_snapshot_id: 1,
        file_version_id: 2,
        probed_by_worker_id: Some(3),
        probed_at: OffsetDateTime::UNIX_EPOCH,
    });
}
