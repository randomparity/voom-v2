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
