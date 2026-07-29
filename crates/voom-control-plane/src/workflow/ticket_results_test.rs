use super::*;

const POLICY_VERIFICATION_RESULT: &str = r#"{
    "source_file_version_id": 11,
    "source_location_id": 12,
    "source_media_snapshot_id": 13,
    "artifact_handle_id": 14,
    "artifact_location_id": 15,
    "artifact_verification_id": 16,
    "status": "verified",
    "path": "/media/movie.mkv",
    "expected_size_bytes": 17,
    "expected_checksum": "blake3:expected",
    "observed_size_bytes": 17,
    "observed_checksum": "blake3:expected"
}"#;

#[test]
fn ordered_result_prefers_non_empty_outputs_and_preserves_order() {
    let result = r#"{
        "result_file_location_id": 1,
        "outputs": [
            {"result_file_location_id": 2},
            {"result_file_location_id": 3}
        ]
    }"#;

    let OrderedTicketResult::Outputs(outputs) = ordered_ticket_result(result).unwrap() else {
        panic!("non-empty outputs must be preserved");
    };
    assert_eq!(outputs[0]["result_file_location_id"], 2);
    assert_eq!(outputs[1]["result_file_location_id"], 3);
}

#[test]
fn ordered_result_uses_scalar_fallback_for_absent_invalid_or_empty_outputs() {
    for result in [
        r#"{"result_file_location_id": 1}"#,
        r#"{"result_file_location_id": 1, "outputs": null}"#,
        r#"{"result_file_location_id": 1, "outputs": {}}"#,
        r#"{"result_file_location_id": 1, "outputs": []}"#,
    ] {
        let OrderedTicketResult::Scalar(result) = ordered_ticket_result(result).unwrap() else {
            panic!("absent or empty outputs must use the scalar result");
        };
        assert_eq!(result["result_file_location_id"], 1);
    }
}

#[test]
fn ordered_result_rejects_malformed_json() {
    let malformed = ordered_ticket_result("{").unwrap_err();
    assert!(malformed.to_string().contains("ticket result is malformed"));
}

#[test]
fn policy_verification_result_initial_shape_remains_readable() {
    let result: PolicyVerificationTicketResult =
        serde_json::from_str(POLICY_VERIFICATION_RESULT).unwrap();

    assert_eq!(result.source_file_version_id, FileVersionId(11));
    assert_eq!(result.artifact_verification_id, ArtifactVerificationId(16));
    assert_eq!(result.path, "/media/movie.mkv");
}

#[test]
fn policy_verification_result_rejects_unknown_fields() {
    let mut value: serde_json::Value = serde_json::from_str(POLICY_VERIFICATION_RESULT).unwrap();
    value["future_field"] = serde_json::json!(true);

    let error = serde_json::from_value::<PolicyVerificationTicketResult>(value).unwrap_err();
    assert!(error.to_string().contains("unknown field `future_field`"));
}
