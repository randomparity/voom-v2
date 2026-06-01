use super::*;

#[test]
fn verify_artifact_request_serializes_expected_facts_with_null_optionals() {
    let req = VerifyArtifactRequest {
        path: "/staging/movie.mkv".to_owned(),
        expected: VerifyArtifactExpectedFacts {
            size_bytes: 12,
            content_hash: "blake3:012345".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
    };

    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "path": "/staging/movie.mkv",
            "expected": {
                "size_bytes": 12,
                "content_hash": "blake3:012345",
                "modified_at": null,
                "local_file_key": null
            }
        })
    );
}

#[test]
fn verify_artifact_result_status_serializes_as_verified() {
    let result = VerifyArtifactResult {
        status: VerifyArtifactStatus::Verified,
        provider: "voom-verify-artifact-worker".to_owned(),
        provider_version: "0.1.0".to_owned(),
        observed: VerifyArtifactObservedFacts {
            size_bytes: 12,
            content_hash: "blake3:012345".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
    };

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(json["status"], "verified");
}

#[test]
fn verify_artifact_payloads_reject_unknown_fields() {
    let request_err = serde_json::from_value::<VerifyArtifactRequest>(serde_json::json!({
        "path": "/staging/movie.mkv",
        "expected": {
            "size_bytes": 12,
            "content_hash": "blake3:012345",
            "modified_at": null,
            "local_file_key": null
        },
        "unexpected": true
    }))
    .unwrap_err();
    assert!(request_err.to_string().contains("unknown field"));

    let expected_err = serde_json::from_value::<VerifyArtifactExpectedFacts>(serde_json::json!({
        "size_bytes": 12,
        "content_hash": "blake3:012345",
        "modified_at": null,
        "local_file_key": null,
        "unexpected": true
    }))
    .unwrap_err();
    assert!(expected_err.to_string().contains("unknown field"));

    let observed_err = serde_json::from_value::<VerifyArtifactObservedFacts>(serde_json::json!({
        "size_bytes": 12,
        "content_hash": "blake3:012345",
        "unexpected": true
    }))
    .unwrap_err();
    assert!(observed_err.to_string().contains("unknown field"));

    let result_err = serde_json::from_value::<VerifyArtifactResult>(serde_json::json!({
        "status": "verified",
        "provider": "voom-verify-artifact-worker",
        "provider_version": "0.1.0",
        "observed": {
            "size_bytes": 12,
            "content_hash": "blake3:012345"
        },
        "unexpected": true
    }))
    .unwrap_err();
    assert!(result_err.to_string().contains("unknown field"));
}
