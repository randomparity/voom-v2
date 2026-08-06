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
fn artifact_verification_id_display_and_json_match_public_id_contract() {
    let id = ArtifactVerificationId(7);
    assert_eq!(id.to_string(), "7");
    assert_eq!(serde_json::to_string(&id).unwrap(), "7");
}

#[test]
fn artifact_commit_record_id_display_and_json_match_public_id_contract() {
    let id = ArtifactCommitRecordId(9);
    assert_eq!(id.to_string(), "9");
    assert_eq!(serde_json::to_string(&id).unwrap(), "9");
}

#[test]
fn node_id_display_and_json_match_public_id_contract() {
    let id = NodeId(42);
    assert_eq!(id.to_string(), "42");
    assert_eq!(serde_json::to_string(&id).unwrap(), "42");
}

#[test]
fn node_incarnation_id_uses_strict_lowercase_hex_wire_format() {
    let text = "0123456789abcdef0123456789abcdef";
    let id: NodeIncarnationId = text.parse().unwrap();

    assert_eq!(id.to_string(), text);
    assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{text}\""));
    assert_eq!(
        serde_json::from_str::<NodeIncarnationId>(&format!("\"{text}\"")).unwrap(),
        id
    );
}

#[test]
fn node_incarnation_id_rejects_noncanonical_text() {
    for invalid in [
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcdeg",
    ] {
        assert!(
            invalid.parse::<NodeIncarnationId>().is_err(),
            "accepted {invalid:?}"
        );
        assert!(
            serde_json::from_str::<NodeIncarnationId>(&format!("\"{invalid}\"")).is_err(),
            "serde accepted {invalid:?}"
        );
    }
}

#[test]
fn generated_node_incarnation_id_is_canonical() {
    let id = NodeIncarnationId::generate().unwrap();
    let text = id.to_string();

    assert_eq!(text.len(), 32);
    assert!(
        text.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn storage_root_id_display_and_json_match_public_id_contract() {
    let id = StorageRootId(73);
    assert_eq!(id.to_string(), "73");
    assert_eq!(serde_json::to_string(&id).unwrap(), "73");
    assert_eq!(serde_json::from_str::<StorageRootId>("73").unwrap(), id);
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
fn policy_input_set_id_displays_inner_u64() {
    assert_eq!(PolicyInputSetId(10).to_string(), "10");
}

#[test]
fn policy_input_set_id_round_trips_through_json() {
    let id = PolicyInputSetId(10);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "10");
    let back: PolicyInputSetId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn policy_synthetic_target_id_displays_inner_u64() {
    assert_eq!(PolicySyntheticTargetId(11).to_string(), "11");
}

#[test]
fn policy_synthetic_target_id_round_trips_through_json() {
    let id = PolicySyntheticTargetId(11);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "11");
    let back: PolicySyntheticTargetId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn policy_document_id_displays_inner_u64() {
    assert_eq!(PolicyDocumentId(42).to_string(), "42");
}

#[test]
fn policy_version_id_round_trips_through_json() {
    let id = PolicyVersionId(7);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "7");
    assert_eq!(serde_json::from_str::<PolicyVersionId>(&json).unwrap(), id);
}

#[test]
fn use_lease_id_displays_inner_u64() {
    assert_eq!(UseLeaseId(42).to_string(), "42");
}

#[test]
fn commit_id_displays_inner_u64() {
    assert_eq!(CommitId(9).to_string(), "9");
}

#[test]
fn commit_id_round_trips_through_json() {
    let id = CommitId(42);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "42");
    let back: CommitId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn external_system_id_display_and_json_match_public_id_contract() {
    let id = ExternalSystemId(42);
    assert_eq!(id.to_string(), "42");
    assert_eq!(serde_json::to_string(&id).unwrap(), "42");
    assert_eq!(serde_json::from_str::<ExternalSystemId>("42").unwrap(), id);
}

#[test]
fn external_path_mapping_id_displays_inner_u64() {
    assert_eq!(ExternalPathMappingId(7).to_string(), "7");
}

#[test]
fn external_system_link_id_displays_inner_u64() {
    assert_eq!(ExternalSystemLinkId(9).to_string(), "9");
}
