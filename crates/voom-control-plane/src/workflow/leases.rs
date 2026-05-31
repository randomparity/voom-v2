use std::future::Future;
use std::time::{Duration, Instant};

use serde_json::Value;
use voom_core::{FailureClass, LeaseId, VoomError};
use voom_store::repo::leases::{Lease, NewLease};

use crate::ControlPlane;
use crate::workflow::executor::WorkflowExecutorOptions;

pub(super) async fn acquire_lease_with_retry(
    control: &ControlPlane,
    input: NewLease,
) -> Result<Lease, VoomError> {
    retry_on_database_locked(|| {
        let input = input.clone();
        async move { control.acquire_lease(input).await }
    })
    .await
}

pub(crate) async fn release_lease_with_retry(
    control: &ControlPlane,
    lease_id: LeaseId,
    payload: Value,
) -> Result<(), VoomError> {
    retry_on_database_locked(|| {
        let payload = payload.clone();
        async move {
            control
                .release_lease(lease_id, payload, control.clock().now())
                .await
                .map(|_| ())
        }
    })
    .await
}

pub(crate) async fn fail_lease_and_return<T>(
    control: &ControlPlane,
    lease_id: LeaseId,
    class: FailureClass,
    source: VoomError,
) -> Result<T, VoomError> {
    fail_lease_with_retry(control, lease_id, source.to_string(), class).await?;
    Err(source)
}

pub(super) async fn fail_lease_with_retry(
    control: &ControlPlane,
    lease_id: LeaseId,
    reason: String,
    class: FailureClass,
) -> Result<(), VoomError> {
    retry_on_database_locked(|| {
        let reason = reason.clone();
        async move {
            control
                .fail_lease(lease_id, reason, class, control.clock().now())
                .await
                .map(|_| ())
        }
    })
    .await
}

pub(super) async fn heartbeat_lease_with_retry(
    control: &ControlPlane,
    lease_id: LeaseId,
    ttl: time::Duration,
) -> Result<(), VoomError> {
    retry_on_database_locked(|| async move {
        control
            .heartbeat_lease(lease_id, ttl, control.clock().now())
            .await
            .map(|_| ())
    })
    .await
}

pub(super) async fn fail_if_watchdog_elapsed(
    control: &ControlPlane,
    lease_id: LeaseId,
    last_heartbeat: Instant,
    last_progress: Instant,
    options: &WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    let now = Instant::now();
    if now.duration_since(last_heartbeat) >= options.heartbeat_timeout {
        return fail_lease_and_return(
            control,
            lease_id,
            FailureClass::WorkerTimeout,
            VoomError::WorkerTimeout(format!("heartbeat timeout for lease {lease_id}")),
        )
        .await;
    }
    if now.duration_since(last_progress) >= options.progress_idle_timeout {
        return fail_lease_and_return(
            control,
            lease_id,
            FailureClass::ProgressTimeout,
            VoomError::WorkerTimeout(format!("progress timeout for lease {lease_id}")),
        )
        .await;
    }
    Ok(())
}

pub(super) async fn heartbeat_workflow_lease(
    control: &ControlPlane,
    lease_id: LeaseId,
    last_heartbeat: &mut Instant,
    options: &WorkflowExecutorOptions,
) -> Result<(), VoomError> {
    heartbeat_lease_with_retry(control, lease_id, time_duration(options.lease_ttl)?).await?;
    *last_heartbeat = Instant::now();
    Ok(())
}

pub(crate) async fn retry_on_database_locked<T, Fut, Op>(mut operation: Op) -> Result<T, VoomError>
where
    Fut: Future<Output = Result<T, VoomError>>,
    Op: FnMut() -> Fut,
{
    let mut last = None;
    for _ in 0..8 {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) if is_database_locked(&err) => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| VoomError::Database("database is locked".to_owned())))
}

pub(crate) fn failure_class_for_error(source: &VoomError) -> FailureClass {
    FailureClass::from_error_code(source.error_code()).unwrap_or(FailureClass::WorkerCrash)
}

pub(super) fn time_duration(duration: Duration) -> Result<time::Duration, VoomError> {
    time::Duration::try_from(duration)
        .map_err(|e| VoomError::Config(format!("duration out of range: {e}")))
}

fn is_database_locked(err: &VoomError) -> bool {
    matches!(err, VoomError::Database(message) if message.contains("database is locked"))
}
