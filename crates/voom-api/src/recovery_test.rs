use std::cell::Cell;
use std::future::{Future, pending, poll_fn};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use tokio::sync::Notify;
use voom_core::VoomError;

use super::run_passes;

struct ActivePass<'a>(&'a Cell<usize>);
impl Drop for ActivePass<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

async fn poll_pending(future: Pin<&mut impl Future<Output = ()>>) {
    let mut future = future;
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn recovery_timeout_error_and_success_each_wait_after_pass_without_overlap_or_catchup() {
    let starts = Cell::new(0);
    let active = Cell::new(0);
    let entered = Notify::new();
    let recovery = run_passes(|| async {
        starts.set(starts.get() + 1);
        active.set(active.get() + 1);
        let _active = ActivePass(&active);
        assert_eq!(active.get(), 1, "recovery passes must never overlap");
        entered.notify_one();
        match starts.get() {
            1 => pending().await,
            2 => Err(VoomError::database("private sentinel".to_owned())),
            _ => Ok(()),
        }
    });
    tokio::pin!(recovery);
    poll_pending(recovery.as_mut()).await;
    entered.notified().await;
    assert_eq!(starts.get(), 1, "the first pass starts immediately");
    tokio::time::advance(Duration::from_secs(29)).await;
    poll_pending(recovery.as_mut()).await;
    assert_eq!(active.get(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    poll_pending(recovery.as_mut()).await;
    assert_eq!(active.get(), 0, "deadline drops the in-flight pass");
    assert_eq!(starts.get(), 1, "timeout must not trigger a tight retry");
    tokio::time::advance(Duration::from_secs(4)).await;
    poll_pending(recovery.as_mut()).await;
    assert_eq!(starts.get(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    poll_pending(recovery.as_mut()).await;
    entered.notified().await;
    assert_eq!(starts.get(), 2);
    assert_eq!(active.get(), 0);
    tokio::time::advance(Duration::from_secs(4)).await;
    poll_pending(recovery.as_mut()).await;
    assert_eq!(starts.get(), 2, "errors retain the post-pass cadence");
    tokio::time::advance(Duration::from_secs(96)).await;
    poll_pending(recovery.as_mut()).await;
    entered.notified().await;
    assert_eq!(
        starts.get(),
        3,
        "missed time must not accumulate catch-up passes"
    );
    poll_pending(recovery.as_mut()).await;
    assert_eq!(starts.get(), 3);
    tokio::time::advance(Duration::from_secs(5)).await;
    poll_pending(recovery.as_mut()).await;
    entered.notified().await;
    assert_eq!(starts.get(), 4, "successful passes also wait five seconds");
}
