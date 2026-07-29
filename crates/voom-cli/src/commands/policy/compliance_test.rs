use super::{ReportMode, parse_report_mode};
use crate::cli::{Command, ComplianceCommand};

#[test]
fn compliance_report_command_requires_policy_version_and_input_set() {
    use clap::Parser;

    let err = crate::cli::Cli::try_parse_from([
        "voom",
        "compliance",
        "report",
        "--policy-version-id",
        "1",
    ])
    .unwrap_err();

    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
}

#[test]
fn compliance_report_command_rejects_job_id_with_preview_arg() {
    use clap::Parser;

    let err = crate::cli::Cli::try_parse_from([
        "voom",
        "compliance",
        "report",
        "--job-id",
        "1",
        "--policy-version-id",
        "2",
    ])
    .unwrap_err();

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn parse_report_mode_accepts_preview_pair() {
    let mode = parse_report_mode(Some(1), Some(2), None).unwrap();
    assert!(matches!(
        mode,
        ReportMode::Preview {
            policy_version_id: 1,
            input_set_id: 2
        }
    ));
}

#[test]
fn parse_report_mode_accepts_job_id() {
    let mode = parse_report_mode(None, None, Some(7)).unwrap();
    assert!(matches!(mode, ReportMode::Run { job_id: 7 }));
}

#[test]
fn parse_report_mode_rejects_none() {
    assert!(parse_report_mode(None, None, None).is_err());
}

#[test]
fn parse_report_mode_rejects_all_three() {
    assert!(parse_report_mode(Some(1), Some(2), Some(3)).is_err());
}

#[test]
fn compliance_execute_defaults_file_window_to_four() {
    use clap::Parser;

    let cli = crate::cli::Cli::try_parse_from([
        "voom",
        "compliance",
        "execute",
        "--policy-version-id",
        "1",
        "--input-set-id",
        "2",
    ])
    .unwrap();
    assert_eq!(execute_file_window(&cli.command), Some(4));
}

#[test]
fn compliance_execute_accepts_explicit_file_window() {
    use clap::Parser;

    let cli = crate::cli::Cli::try_parse_from([
        "voom",
        "compliance",
        "execute",
        "--policy-version-id",
        "1",
        "--input-set-id",
        "2",
        "--max-in-flight-files",
        "9",
    ])
    .unwrap();
    assert_eq!(execute_file_window(&cli.command), Some(9));
}

fn execute_file_window(command: &Command) -> Option<usize> {
    if let Command::Compliance(ComplianceCommand::Execute {
        max_in_flight_files,
        ..
    }) = command
    {
        Some(*max_in_flight_files)
    } else {
        None
    }
}
