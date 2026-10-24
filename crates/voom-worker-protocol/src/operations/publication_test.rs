use super::*;
use serde_json::json;

#[test]
fn owner_publication_wire_is_strict() {
    let identity = json!({"operation_id": 1, "generation": 0, "claim_lease_id": 2,
        "claim_token": "private-claim", "output_count": 2});
    let mut extra = identity.clone();
    extra["unrecognized"] = json!(true);
    assert!(serde_json::from_value::<CommitGroupIdentity>(extra).is_err());
    let group: CommitGroupIdentity = serde_json::from_value(identity).unwrap();
    assert!(!format!("{group:?}").contains("private-claim"));
    let proof = CommitGroupMemberProof {
        intent_id: ArtifactCommitIntentId(3),
        fence_hex: "private-fence".to_owned(),
        applied: ObservedFileFacts {
            size_bytes: 1,
            content_hash: "blake3:hash".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
    };
    assert!(!format!("{proof:?}").contains("private-fence"));
    let mut extra = serde_json::to_value(proof).unwrap();
    extra["unrecognized"] = json!(true);
    assert!(serde_json::from_value::<CommitGroupMemberProof>(extra).is_err());
    let output = json!({"provider_relative_locator":"staged/result.mkv",
        "facts":{"size_bytes":1,"content_hash":"blake3:hash"}, "unrecognized":true});
    assert!(serde_json::from_value::<OwnerObservedOutput>(output).is_err());
}
