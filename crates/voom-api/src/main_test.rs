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
