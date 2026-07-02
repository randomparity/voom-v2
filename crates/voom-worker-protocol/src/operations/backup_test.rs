use super::*;

#[test]
fn back_up_file_request_serializes_source_and_destination() {
    let req = BackUpFileRequest {
        source_path: "/library/movie.mkv".to_owned(),
        destination_path: "/backups/42/movie.mkv".to_owned(),
    };

    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "source_path": "/library/movie.mkv",
            "destination_path": "/backups/42/movie.mkv"
        })
    );
}

#[test]
fn back_up_file_result_status_serializes_as_backed_up() {
    let result = BackUpFileResult {
        status: BackUpFileStatus::BackedUp,
        provider: "voom-backup-worker".to_owned(),
        provider_version: "0.1.0".to_owned(),
        destination_path: "/backups/42/movie.mkv".to_owned(),
        size_bytes: 12,
        checksum: "blake3:012345".to_owned(),
    };

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(json["status"], "backed_up");
    assert_eq!(json["size_bytes"], 12);
}

#[test]
fn back_up_file_result_round_trips() {
    let result = BackUpFileResult {
        status: BackUpFileStatus::BackedUp,
        provider: "voom-backup-worker".to_owned(),
        provider_version: "0.1.0".to_owned(),
        destination_path: "/backups/42/movie.mkv".to_owned(),
        size_bytes: 4096,
        checksum: "blake3:abcdef".to_owned(),
    };

    let json = serde_json::to_string(&result).unwrap();
    let decoded: BackUpFileResult = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded, result);
}

#[test]
fn back_up_file_payloads_reject_unknown_fields() {
    let request_err = serde_json::from_value::<BackUpFileRequest>(serde_json::json!({
        "source_path": "/library/movie.mkv",
        "destination_path": "/backups/42/movie.mkv",
        "unexpected": true
    }))
    .unwrap_err();
    assert!(request_err.to_string().contains("unknown field"));

    let result_err = serde_json::from_value::<BackUpFileResult>(serde_json::json!({
        "status": "backed_up",
        "provider": "voom-backup-worker",
        "provider_version": "0.1.0",
        "destination_path": "/backups/42/movie.mkv",
        "size_bytes": 12,
        "checksum": "blake3:012345",
        "unexpected": true
    }))
    .unwrap_err();
    assert!(result_err.to_string().contains("unknown field"));
}
