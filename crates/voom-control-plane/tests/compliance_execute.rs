#![expect(
    clippy::panic_in_result_fn,
    reason = "integration test assertions should fail fast after setup errors use Result"
)]

use voom_control_plane::ControlPlane;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_worker_protocol::OperationKind;

#[tokio::test]
async fn compliance_execute_runs_set_container_as_remux_through_workflow_executor()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::NamedTempFile::new()?;
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await?;
    let pool = voom_store::connect(&url).await?;
    let cp = ControlPlane::open(&url).await?;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom")?;
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .map_err(|err| std::io::Error::other(format!("{err:?}")))?;
    let input = cp
        .create_policy_input_set(load_fixture(
            FixtureName::SyntheticNoncompliantTranscodeNeeded,
        )?)
        .await?;

    let data = cp
        .execute_compliance_policy(created_policy.version.id, input.id)
        .await
        .map_err(|err| std::io::Error::other(err.source.to_string()))?;

    assert!(data.execution.job_id.is_some());
    assert_eq!(data.execution.submitted_node_count, 1);
    assert_eq!(data.execution.dispatch_count, 1);
    assert_eq!(data.execution.failure_count, 0);
    assert_eq!(data.execution.per_operation.get("remux"), Some(&1));

    let ticket_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE kind = 'synthetic.workflow.operation.remux'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(ticket_count, 1);

    let lease_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM leases WHERE worker_id IN (SELECT worker_id FROM worker_capabilities WHERE operation = ?)",
    )
    .bind(operation_name(OperationKind::Remux))
    .fetch_one(&pool)
    .await?;
    assert_eq!(lease_count, 1);
    Ok(())
}

fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::Remux => "remux",
        _ => unreachable!("test only needs remux"),
    }
}
