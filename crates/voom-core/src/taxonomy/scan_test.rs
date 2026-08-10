use super::*;

#[test]
fn scan_status_round_trips_exact_durable_tokens() {
    for (status, wire) in [
        (ScanSessionStatus::Requested, "requested"),
        (ScanSessionStatus::Running, "running"),
        (ScanSessionStatus::Succeeded, "succeeded"),
        (ScanSessionStatus::Failed, "failed"),
        (ScanSessionStatus::Cancelled, "cancelled"),
        (ScanSessionStatus::Stale, "stale"),
    ] {
        assert_eq!(status.as_str(), wire);
        assert_eq!(
            ScanSessionStatus::parse_database("scan_sessions.status", wire.to_owned()).unwrap(),
            status,
        );
    }
}

#[test]
fn terminal_reason_is_bounded_by_encoded_bytes() {
    assert!(ScanTerminalReason::new("é".repeat(512)).is_ok());
    assert!(ScanTerminalReason::new(format!("{}a", "é".repeat(512))).is_err());
    assert!(ScanTerminalReason::new("\t\r\n ").is_err());
    assert!(ScanTerminalReason::new("bad\0reason").is_err());
}

#[test]
fn terminal_reason_deserialization_enforces_operator_input_validation() {
    let reason = ScanTerminalReason::new("operator cancelled scan").unwrap();
    assert_eq!(
        serde_json::to_string(&reason).unwrap(),
        "\"operator cancelled scan\""
    );
    assert!(serde_json::from_str::<ScanTerminalReason>("\"\\t\\r\\n \"").is_err());
    assert!(serde_json::from_str::<ScanTerminalReason>("\"bad\\u0000reason\"").is_err());
}

#[test]
fn scan_status_and_terminal_reason_distinguish_database_from_config_errors() {
    let status = ScanSessionStatus::parse_database("scan_sessions.status", "unknown".to_owned());
    assert!(matches!(status, Err(VoomError::Database { .. })));

    let reason = ScanTerminalReason::parse_database("scan_sessions.reason", "\t ".to_owned());
    assert!(matches!(reason, Err(VoomError::Database { .. })));
    assert!(matches!(
        ScanTerminalReason::new("\t "),
        Err(VoomError::Config(_))
    ));
}
