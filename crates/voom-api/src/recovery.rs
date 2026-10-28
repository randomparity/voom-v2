//! Explicit API-service recovery. Constructing a router or control plane stays passive.

use std::future::Future;
use std::time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::{ErrorCode, VoomError};

const PASS_SPACING: Duration = Duration::from_secs(5);
const PASS_DEADLINE: Duration = Duration::from_secs(30);

/// Recover remote authority while this future is polled by the server lifetime.
/// Dropping it cancels the current pass; a later service start can safely replay it.
pub async fn run(control_plane: ControlPlane) {
    run_passes(|| async {
        control_plane
            .remote_recover(control_plane.clock().now())
            .await
            .map(|_| ())
    })
    .await;
}

async fn run_passes<F, P>(mut pass: F)
where
    F: FnMut() -> P,
    P: Future<Output = Result<(), VoomError>>,
{
    loop {
        match tokio::time::timeout(PASS_DEADLINE, pass()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                event = "remote_recovery_failed",
                code = error.error_code().as_str(),
                message = "remote recovery failed; retrying after the recovery cadence"
            ),
            Err(_) => tracing::warn!(
                event = "remote_recovery_timeout",
                code = ErrorCode::Internal.as_str(),
                message = "remote recovery exceeded its pass deadline; retrying after the recovery cadence"
            ),
        }
        tokio::time::sleep(PASS_SPACING).await;
    }
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
