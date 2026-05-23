use voom_policy::{FixtureName, load_fixture, load_policy_fixture};

use crate::{ExecutionPlan, PlanningContext, PlanningRequest, generate_plan};

use super::*;

#[test]
fn compliant_container_fixture_matches_golden_plan() {
    let policy_source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let compiled = voom_policy::compile_policy(&policy_source).unwrap().policy;
    let input = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input,
        context: PlanningContext {
            input_source_label: Some("synthetic_compliant_baseline".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("container_metadata_compliant").unwrap()
    );
}

#[test]
fn noncompliant_container_fixture_matches_golden_plan() {
    let policy_source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let compiled = voom_policy::compile_policy(&policy_source).unwrap().policy;
    let input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input,
        context: PlanningContext {
            input_source_label: Some("synthetic_noncompliant_transcode_needed".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("container_metadata_noncompliant").unwrap()
    );
}

#[test]
fn golden_plans_deserialize_through_public_type() {
    for name in [
        "container_metadata_compliant",
        "container_metadata_noncompliant",
    ] {
        let value = load_golden_plan(name).unwrap();
        serde_json::from_value::<ExecutionPlan>(value).unwrap();
    }
}

#[test]
fn golden_compliance_reports_deserialize_through_public_type() {
    for name in [
        "container_metadata_compliant",
        "container_metadata_noncompliant",
        "container_metadata_blocked",
        "container_metadata_mixed",
    ] {
        let value = load_golden_compliance_report(name).unwrap();
        serde_json::from_value::<crate::ComplianceReport>(value).unwrap();
    }
}

#[test]
fn unknown_golden_plan_name_fails_loudly() {
    let err = load_golden_plan("typo").unwrap_err();

    assert!(matches!(err, GoldenPlanFixtureError::UnknownFixture(name) if name == "typo"));
}
