use serde_json::json;
use voom_policy::fixtures::FixtureName;

use super::{PlanData, fixture_name};

#[test]
fn fixture_name_parser_accepts_public_labels() {
    assert_eq!(
        fixture_name("synthetic_compliant_baseline").unwrap(),
        FixtureName::SyntheticCompliantBaseline
    );
    assert_eq!(
        fixture_name("synthetic_noncompliant_transcode_needed").unwrap(),
        FixtureName::SyntheticNoncompliantTranscodeNeeded
    );
    assert_eq!(
        fixture_name("unknown_fixture").unwrap_err(),
        "unknown input fixture"
    );
}

#[test]
fn plan_data_wraps_plan_under_plan_key() {
    let plan = serde_json::from_value(
        voom_plan::load_golden_plan("container_metadata_compliant").unwrap(),
    )
    .unwrap();
    let data = serde_json::to_value(PlanData { plan }).unwrap();

    assert_eq!(
        data.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["plan"]
    );
    assert_eq!(
        data["plan"]["input"]["source_label"],
        json!("synthetic_compliant_baseline")
    );
}

#[tokio::test]
async fn dry_run_bad_args_return_bad_args_exit_code() {
    let code = super::dry_run(
        std::path::Path::new("does-not-need-to-exist"),
        "unknown_fixture",
    )
    .await
    .unwrap();

    assert_eq!(code, 1);
}

#[tokio::test]
async fn dry_run_missing_policy_file_returns_bad_args_exit_code() {
    let code = super::dry_run(
        std::path::Path::new("does-not-exist.voom"),
        "synthetic_compliant_baseline",
    )
    .await
    .unwrap();

    assert_eq!(code, 1);
}
