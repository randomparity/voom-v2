use clap::Parser as _;

use super::Cli;

fn parsed_command(args: &[&str]) -> String {
    format!("{:?}", Cli::try_parse_from(args).unwrap().command)
}

#[test]
fn scan_session_exact_forms_parse_with_defaults_and_filters() {
    assert_eq!(
        parsed_command(&["voom", "scan-session", "request", "--root", "7"]),
        "ScanSession(Request { root: 7, idle_timeout_seconds: 300 })"
    );
    assert_eq!(
        parsed_command(&[
            "voom",
            "scan-session",
            "request",
            "--root",
            "7",
            "--idle-timeout-seconds",
            "300",
        ]),
        "ScanSession(Request { root: 7, idle_timeout_seconds: 300 })"
    );
    assert_eq!(
        parsed_command(&["voom", "scan-session", "show", "--id", "9"]),
        "ScanSession(Show { id: 9 })"
    );
    assert_eq!(
        parsed_command(&[
            "voom",
            "scan-session",
            "list",
            "--root",
            "7",
            "--status",
            "running",
            "--after",
            "4",
            "--limit",
            "50",
        ]),
        "ScanSession(List { root: Some(7), status: Some(Running), after: Some(4), limit: 50 })"
    );
    assert_eq!(
        parsed_command(&["voom", "scan-session", "list"]),
        "ScanSession(List { root: None, status: None, after: None, limit: 50 })"
    );
    assert_eq!(
        parsed_command(&[
            "voom",
            "scan-session",
            "reconciliation",
            "--id",
            "9",
            "--after",
            "100",
            "--limit",
            "50",
        ]),
        "ScanSession(Reconciliation { id: 9, after: Some(100), limit: 50 })"
    );
    assert_eq!(
        parsed_command(&["voom", "scan-session", "reconciliation", "--id", "9"]),
        "ScanSession(Reconciliation { id: 9, after: None, limit: 50 })"
    );
    assert_eq!(
        parsed_command(&[
            "voom",
            "scan-session",
            "cancel",
            "--id",
            "9",
            "--reason",
            "operator stopped scan",
        ]),
        "ScanSession(Cancel { id: 9, reason: \"operator stopped scan\" })"
    );
}

#[test]
fn scan_session_rejects_out_of_range_numbers_and_unknown_status() {
    for args in [
        vec![
            "voom",
            "scan-session",
            "request",
            "--root",
            "7",
            "--idle-timeout-seconds",
            "0",
        ],
        vec![
            "voom",
            "scan-session",
            "request",
            "--root",
            "7",
            "--idle-timeout-seconds",
            "86401",
        ],
        vec!["voom", "scan-session", "list", "--limit", "0"],
        vec!["voom", "scan-session", "list", "--limit", "101"],
        vec![
            "voom",
            "scan-session",
            "reconciliation",
            "--id",
            "9",
            "--limit",
            "0",
        ],
        vec![
            "voom",
            "scan-session",
            "reconciliation",
            "--id",
            "9",
            "--limit",
            "101",
        ],
        vec!["voom", "scan-session", "list", "--status", "complete"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn scan_session_rejects_invalid_cancel_reasons_and_missing_subcommand() {
    let too_long = "a".repeat(1025);
    for reason in ["", " \t\r\n", "bad\0reason", too_long.as_str()] {
        assert!(
            Cli::try_parse_from([
                "voom",
                "scan-session",
                "cancel",
                "--id",
                "9",
                "--reason",
                reason,
            ])
            .is_err(),
            "reason {reason:?} must be rejected"
        );
    }
    assert!(Cli::try_parse_from(["voom", "scan-session"]).is_err());
}

#[test]
fn legacy_scan_parse_contract_is_unchanged() {
    assert_eq!(
        parsed_command(&["voom", "scan", "--root", "7"]),
        "Scan { root: 7 }"
    );
}
