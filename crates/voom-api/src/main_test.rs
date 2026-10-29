#[cfg(unix)]
use std::future::{pending, ready};

#[cfg(unix)]
use super::termination_signal_with;
use super::{ServerError, server_diagnostic};

#[cfg(unix)]
#[tokio::test]
async fn termination_signal_selects_sigint() {
    assert!(
        termination_signal_with(ready(Some(())), pending::<Option<()>>())
            .await
            .is_ok()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn termination_signal_selects_sigterm() {
    assert!(
        termination_signal_with(pending::<Option<()>>(), ready(Some(())))
            .await
            .is_ok()
    );
}

#[test]
fn stopped_listener_maps_to_fail_loud_process_diagnostic() {
    let diagnostic = server_diagnostic(&ServerError::Stopped);

    assert_eq!(diagnostic.operation, "serve_connections");
    assert_eq!(diagnostic.code, "INTERNAL");
    assert!(diagnostic.message.contains("stopped unexpectedly"));
}

struct RecoveryDrop<'a>(&'a tokio::sync::Notify);
impl Drop for RecoveryDrop<'_> {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[tokio::test]
async fn recovery_is_dropped_before_termination_returns() {
    let entered = tokio::sync::Notify::new();
    let dropped = tokio::sync::Notify::new();
    let stop = tokio::sync::Notify::new();
    let recovery = async {
        let _guard = RecoveryDrop(&dropped);
        entered.notify_one();
        std::future::pending::<()>().await;
    };
    let lifetime = super::termination_with_recovery(
        async {
            stop.notified().await;
            Ok(())
        },
        recovery,
    );
    let (result, ()) = tokio::join!(lifetime, async {
        entered.notified().await;
        stop.notify_one();
        dropped.notified().await;
    });
    assert!(result.is_ok());
}

#[tokio::test]
async fn recovery_unexpected_return_requests_internal_shutdown() {
    let dropped = tokio::sync::Notify::new();
    let entered = tokio::sync::Notify::new();
    let termination = async {
        entered.notify_one();
        let _guard = RecoveryDrop(&dropped);
        std::future::pending::<Result<(), super::StartupDiagnostic>>().await
    };
    let result =
        super::termination_with_recovery(termination, async { entered.notified().await }).await;
    assert_eq!(result.err().map(|error| error.code), Some("INTERNAL"));
    dropped.notified().await;
}

#[tokio::test]
async fn recovery_owner_drop_cancels_the_polled_service() {
    let entered = tokio::sync::Notify::new();
    let dropped = tokio::sync::Notify::new();
    let recovery = async {
        let _guard = RecoveryDrop(&dropped);
        entered.notify_one();
        std::future::pending::<()>().await;
    };
    let mut owner = Box::pin(super::termination_with_recovery(
        std::future::pending::<Result<(), super::StartupDiagnostic>>(),
        recovery,
    ));
    let running = tokio::select! {
        () = entered.notified() => true,
        _ = &mut owner => false,
    };
    assert!(running, "service must stay pending");
    drop(owner);
    dropped.notified().await;
}
