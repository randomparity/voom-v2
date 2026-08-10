use super::*;

#[test]
fn run_codes_map_to_public_exit_contract() {
    let cases = [
        (0, Exit::Ok),
        (1, Exit::BadArgs),
        (-1, Exit::Failure),
        (2, Exit::Failure),
        (3, Exit::Failure),
        (i32::MAX, Exit::Failure),
    ];

    for (code, expected) in cases {
        assert_eq!(
            Exit::from_run_code(code),
            expected,
            "unexpected mapping for run code {code}"
        );
    }
}

#[tokio::test]
async fn scan_session_command_dispatches_to_the_local_control_plane() -> anyhow::Result<()> {
    let database = voom_test_support::TempDatabase::new()?;
    let url = voom_store::test_support::sqlite_url_for(database.path());
    voom_store::init(&url).await?;
    let pool = voom_store::connect(&url).await?;
    let root = voom_store::test_support::seed_test_storage_root(&pool).await?;
    let cli = Cli::try_parse_from([
        "voom",
        "--database-url",
        &url,
        "scan-session",
        "request",
        "--root",
        &root.0.to_string(),
    ])?;

    let exit = dispatch(cli).await?;
    anyhow::ensure!(exit == Exit::Ok, "unexpected scan-session exit: {exit:?}");
    Ok(())
}
