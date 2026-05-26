#![expect(
    clippy::panic_in_result_fn,
    reason = "integration test assertions should fail fast after setup errors use Result"
)]

use voom_control_plane::ControlPlane;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_test_support::worker::{TestWorkerConfig, TestWorkerLaunch, cargo_bin_or_build};
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
    let mut provider = RemuxProviderLaunch::start(&cp).await?;

    let result = async {
        cp.execute_compliance_policy(created_policy.version.id, input.id)
            .await
            .map_err(|err| err.source.to_string())
    }
    .await;
    let Err(err) = combine_result_and_cleanup(result, provider.shutdown()) else {
        return Err(std::io::Error::other(
            "synthetic policy remux unexpectedly created workflow tickets",
        )
        .into());
    };

    assert!(err.contains("workflow root payload binding"));
    assert!(err.contains("remux requires file_version or file_location target"));

    let ticket_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE kind = 'synthetic.workflow.operation.remux'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(ticket_count, 0);

    let lease_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM leases WHERE worker_id IN (SELECT worker_id FROM worker_capabilities WHERE operation = ?)",
    )
    .bind(operation_name(OperationKind::Remux))
    .fetch_one(&pool)
    .await?;
    assert_eq!(lease_count, 0);
    Ok(())
}

struct RemuxProviderLaunch {
    inner: TestWorkerLaunch,
}

impl RemuxProviderLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: TestWorkerLaunch::start(
                cp,
                TestWorkerConfig::synthetic(
                    cargo_bin_or_build("voom-fakes", "fake-remuxer")?,
                    "compliance-remux",
                    "compliance-remux-secret",
                    operation_name(OperationKind::Remux),
                ),
            )
            .await?,
        })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}

fn combine_result_and_cleanup<T>(
    result: Result<T, String>,
    cleanup: Result<(), Box<dyn std::error::Error>>,
) -> Result<T, String> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(format!("provider cleanup failed: {err}")),
        (Err(err), Err(cleanup_err)) => {
            Err(format!("{err}; provider cleanup failed: {cleanup_err}"))
        }
    }
}

fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::Remux => "remux",
        _ => unreachable!("test only needs remux"),
    }
}
