use super::*;

#[test]
fn ordered_result_prefers_non_empty_outputs_and_preserves_order() {
    let result = r#"{
        "result_file_location_id": 1,
        "outputs": [
            {"result_file_location_id": 2},
            {"result_file_location_id": 3}
        ]
    }"#;

    assert_eq!(result_location_ids(result).unwrap(), [2, 3]);
}

#[test]
fn ordered_result_uses_scalar_fallback_for_absent_invalid_or_empty_outputs() {
    for result in [
        r#"{"result_file_location_id": 1}"#,
        r#"{"result_file_location_id": 1, "outputs": null}"#,
        r#"{"result_file_location_id": 1, "outputs": {}}"#,
        r#"{"result_file_location_id": 1, "outputs": []}"#,
    ] {
        assert_eq!(result_location_ids(result).unwrap(), [1]);
    }
}

#[test]
fn ordered_result_rejects_malformed_json_and_negative_location_ids() {
    let malformed = ordered_ticket_result("{").unwrap_err();
    assert!(malformed.to_string().contains("ticket result is malformed"));

    let negative = result_location_ids(r#"{"result_file_location_id": -1}"#).unwrap_err();
    assert!(
        negative
            .to_string()
            .contains("promotion ticket result location id is invalid")
    );
}
