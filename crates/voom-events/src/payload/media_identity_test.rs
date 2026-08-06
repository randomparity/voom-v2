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
fn identity_evidence_uses_typed_ids_and_assertion_vocabulary() {
    let payload = IdentityEvidenceRecordedPayload {
        evidence_id: voom_core::EvidenceId(31),
        target_type: "file_version".to_owned(),
        target_id: 32,
        assertion_type: crate::AssertionKind::HashMatch,
        provider: "ingest".to_owned(),
        provider_version: "1.0".to_owned(),
        confidence: 0.9,
        observed_at: OffsetDateTime::UNIX_EPOCH,
    };

    let json = serde_json::to_value(&payload).unwrap();
    assert_eq!(json["evidence_id"], 31);
    assert_eq!(json["assertion_type"], "hash_match");
    assert_eq!(
        serde_json::from_value::<IdentityEvidenceRecordedPayload>(json).unwrap(),
        payload
    );
    assert!(
        serde_json::from_value::<IdentityEvidenceRecordedPayload>(serde_json::json!({
            "evidence_id": 31,
            "target_type": "file_version",
            "target_id": 32,
            "assertion_type": "not_an_assertion",
            "provider": "ingest",
            "provider_version": "1.0",
            "confidence": 0.9,
            "observed_at": "1970-01-01T00:00:00Z"
        }))
        .is_err()
    );
}

#[test]
fn media_work_created_round_trips() {
    let p = MediaWorkCreatedPayload {
        media_work_id: voom_core::MediaWorkId(9),
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
    let p = FileLocationRootedRecordedByMovePayload {
        retired_file_location_id: voom_core::FileLocationId(1),
        new_file_location_id: voom_core::FileLocationId(2),
        file_version_id: voom_core::FileVersionId(3),
        storage_root_id: voom_core::StorageRootId(4),
        provider_relative_locator: voom_core::ProviderRelativeLocator::new("new.mkv".to_owned())
            .unwrap(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: FileLocationRootedRecordedByMovePayload = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn event_kind_matches_payload_for_identity_variants() {
    let e = Event::FileAssetCreated(FileAssetCreatedPayload {
        file_asset_id: voom_core::FileAssetId(1),
    });
    assert_eq!(e.kind(), EventKind::FileAssetCreated);

    let e = Event::IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload {
        evidence_id: voom_core::EvidenceId(99),
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
                media_work_id: voom_core::MediaWorkId(1),
                kind: "movie".to_owned(),
                display_title: "X".to_owned(),
                provisional: true,
            }),
            "media_work.created",
        ),
        (
            Event::FileLocationRootedAliased(FileLocationRootedAliasedPayload {
                file_location_id: voom_core::FileLocationId(1),
                file_version_id: voom_core::FileVersionId(1),
                storage_root_id: voom_core::StorageRootId(2),
                provider_relative_locator: voom_core::ProviderRelativeLocator::new(
                    "x.mkv".to_owned(),
                )
                .unwrap(),
            }),
            "file_location.rooted_aliased",
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
        media_work_id: voom_core::MediaWorkId(1),
        kind: "movie".to_owned(),
        display_title: "Example".to_owned(),
        provisional: false,
    });
}

#[test]
fn media_variant_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&MediaVariantCreatedPayload {
        media_variant_id: voom_core::MediaVariantId(1),
        media_work_id: voom_core::MediaWorkId(2),
        label: "1080p".to_owned(),
        provisional: false,
    });
}

#[test]
fn asset_bundle_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&AssetBundleCreatedPayload {
        bundle_id: voom_core::BundleId(1),
        media_variant_id: voom_core::MediaVariantId(2),
        display_name: "Main".to_owned(),
    });
}

#[test]
fn asset_bundle_member_added_payload_rejects_unknown_field() {
    assert_rejects_unknown(&AssetBundleMemberAddedPayload {
        bundle_id: voom_core::BundleId(1),
        file_asset_id: voom_core::FileAssetId(2),
        role: "video".to_owned(),
    });
}

#[test]
fn asset_bundle_member_removed_payload_rejects_unknown_field() {
    assert_rejects_unknown(&AssetBundleMemberRemovedPayload {
        bundle_id: voom_core::BundleId(1),
        file_asset_id: voom_core::FileAssetId(2),
        role: "video".to_owned(),
    });
}

#[test]
fn file_asset_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileAssetCreatedPayload {
        file_asset_id: voom_core::FileAssetId(1),
    });
}

#[test]
fn file_version_created_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileVersionCreatedPayload {
        file_version_id: voom_core::FileVersionId(1),
        file_asset_id: voom_core::FileAssetId(2),
        content_hash: "blake3:abc".to_owned(),
        size_bytes: 4096,
        produced_by: "ingest".to_owned(),
        produced_from_version_id: None,
    });
}

#[test]
fn file_location_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationRecordedPayload {
        file_location_id: voom_core::FileLocationId(1),
        file_version_id: voom_core::FileVersionId(2),
        kind: "local_path".to_owned(),
        value: "/media/x.mkv".to_owned(),
    });
    assert_rejects_unknown(&FileLocationRootedRecordedPayload {
        file_location_id: voom_core::FileLocationId(1),
        file_version_id: voom_core::FileVersionId(2),
        storage_root_id: voom_core::StorageRootId(3),
        provider_relative_locator: voom_core::ProviderRelativeLocator::new("x.mkv".to_owned())
            .unwrap(),
    });
}

#[test]
fn file_location_aliased_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationAliasedPayload {
        file_location_id: voom_core::FileLocationId(1),
        file_version_id: voom_core::FileVersionId(2),
        kind: "local_path".to_owned(),
        value: "/media/y.mkv".to_owned(),
    });
    assert_rejects_unknown(&FileLocationRootedAliasedPayload {
        file_location_id: voom_core::FileLocationId(1),
        file_version_id: voom_core::FileVersionId(2),
        storage_root_id: voom_core::StorageRootId(3),
        provider_relative_locator: voom_core::ProviderRelativeLocator::new("y.mkv".to_owned())
            .unwrap(),
    });
}

#[test]
fn file_location_retired_by_move_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationRetiredByMovePayload {
        file_location_id: voom_core::FileLocationId(1),
        file_version_id: voom_core::FileVersionId(2),
        retired_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn file_location_recorded_by_move_payload_rejects_unknown_field() {
    assert_rejects_unknown(&FileLocationRecordedByMovePayload {
        retired_file_location_id: voom_core::FileLocationId(1),
        new_file_location_id: voom_core::FileLocationId(2),
        file_version_id: voom_core::FileVersionId(3),
        kind: "local_path".to_owned(),
        value: "/media/z.mkv".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    });
    assert_rejects_unknown(&FileLocationRootedRecordedByMovePayload {
        retired_file_location_id: voom_core::FileLocationId(1),
        new_file_location_id: voom_core::FileLocationId(2),
        file_version_id: voom_core::FileVersionId(3),
        storage_root_id: voom_core::StorageRootId(4),
        provider_relative_locator: voom_core::ProviderRelativeLocator::new("z.mkv".to_owned())
            .unwrap(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn legacy_and_rooted_location_payloads_do_not_accept_each_others_shapes() {
    let legacy = serde_json::json!({
        "file_location_id": 1,
        "file_version_id": 2,
        "kind": "local_path",
        "value": "/media/x"
    });
    assert!(serde_json::from_value::<FileLocationRecordedPayload>(legacy.clone()).is_ok());
    assert!(serde_json::from_value::<FileLocationAliasedPayload>(legacy.clone()).is_ok());
    assert!(serde_json::from_value::<FileLocationRootedRecordedPayload>(legacy.clone()).is_err());
    assert!(serde_json::from_value::<FileLocationRootedAliasedPayload>(legacy).is_err());

    let legacy_move = serde_json::json!({
        "retired_file_location_id": 1,
        "new_file_location_id": 2,
        "file_version_id": 3,
        "kind": "local_path",
        "value": "/media/z",
        "observed_at": "1970-01-01T00:00:00Z"
    });
    assert!(
        serde_json::from_value::<FileLocationRecordedByMovePayload>(legacy_move.clone()).is_ok()
    );
    assert!(
        serde_json::from_value::<FileLocationRootedRecordedByMovePayload>(legacy_move).is_err()
    );

    let rooted = serde_json::json!({
        "file_location_id": 1,
        "file_version_id": 2,
        "storage_root_id": 3,
        "provider_relative_locator": "x.mkv"
    });
    assert!(serde_json::from_value::<FileLocationRecordedPayload>(rooted.clone()).is_err());
    assert!(serde_json::from_value::<FileLocationAliasedPayload>(rooted.clone()).is_err());
    assert!(serde_json::from_value::<FileLocationRootedRecordedPayload>(rooted.clone()).is_ok());
    assert!(serde_json::from_value::<FileLocationRootedAliasedPayload>(rooted).is_ok());
}

#[test]
fn identity_evidence_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&IdentityEvidenceRecordedPayload {
        evidence_id: voom_core::EvidenceId(1),
        target_type: "file_version".to_owned(),
        target_id: 2,
        assertion_type: crate::AssertionKind::HashMatch,
        provider: "ingest".to_owned(),
        provider_version: "1.0".to_owned(),
        confidence: 0.9,
        observed_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn identity_evidence_accepted_payload_rejects_unknown_field() {
    assert_rejects_unknown(&IdentityEvidenceAcceptedPayload {
        evidence_id: voom_core::EvidenceId(1),
        target_type: "file_version".to_owned(),
        target_id: 2,
        accepted_user_id: Some("alice".to_owned()),
        accepted_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn identity_evidence_superseded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&IdentityEvidenceSupersededPayload {
        superseded_evidence_id: voom_core::EvidenceId(1),
        superseded_by_evidence_id: voom_core::EvidenceId(2),
        target_type: "file_version".to_owned(),
        target_id: 3,
        superseded_at: OffsetDateTime::UNIX_EPOCH,
    });
}

#[test]
fn media_snapshot_recorded_payload_rejects_unknown_field() {
    assert_rejects_unknown(&MediaSnapshotRecordedPayload {
        media_snapshot_id: voom_core::MediaSnapshotId(1),
        file_version_id: voom_core::FileVersionId(2),
        probed_by_worker_id: Some(voom_core::WorkerId(3)),
        probed_at: OffsetDateTime::UNIX_EPOCH,
    });
}
