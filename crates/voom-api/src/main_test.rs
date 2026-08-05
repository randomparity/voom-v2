#[cfg(unix)]
use std::future::{pending, ready};

#[cfg(unix)]
use super::termination_signal_with;

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
