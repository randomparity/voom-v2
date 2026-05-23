use voom_policy::{FixtureName, load_fixture, load_policy_fixture};

use super::*;
use crate::cases::cp;

#[test]
fn plan_policy_source_with_input_draft_does_not_need_database() {
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    let plan = plan_policy_source_with_input(
        &source,
        input,
        Some("synthetic_noncompliant_transcode_needed"),
    )
    .unwrap();

    assert_eq!(plan.policy.slug, "container-metadata");
    assert_eq!(
        plan.input.source_label.as_deref(),
        Some("synthetic_noncompliant_transcode_needed")
    );
    assert!(
        plan.nodes
            .iter()
            .any(|node| node.status == voom_plan::NodeStatus::Planned)
    );
}

#[tokio::test]
async fn durable_planning_reads_compiled_policy_without_creating_execution_state() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(
            load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap(),
        )
        .await
        .unwrap();

    let before_jobs = count_rows(&cp, "jobs").await;
    let before_events = count_rows(&cp, "events").await;
    let before_tickets = count_rows(&cp, "tickets").await;

    let plan = cp
        .plan_accepted_policy_version_with_input_set(created_policy.version.id, input.id)
        .await
        .unwrap();

    assert_eq!(plan.policy.version_id, Some(created_policy.version.id));
    assert_eq!(plan.input.input_set_id, Some(input.id));
    assert_eq!(before_jobs, count_rows(&cp, "jobs").await);
    assert_eq!(before_events, count_rows(&cp, "events").await);
    assert_eq!(before_tickets, count_rows(&cp, "tickets").await);
}

async fn count_rows(cp: &crate::ControlPlane, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar::<_, i64>(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}
