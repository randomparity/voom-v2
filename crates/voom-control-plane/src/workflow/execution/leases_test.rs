use super::*;

fn timing(heartbeat_timeout: Duration, progress_idle_timeout: Duration) -> WorkflowTimingOptions {
    WorkflowTimingOptions {
        heartbeat_timeout,
        progress_idle_timeout,
        ..WorkflowTimingOptions::for_tests()
    }
}

#[test]
fn watchdog_deadline_uses_heartbeat_for_an_exact_tie() {
    let started = Instant::now();

    let (deadline, class) = next_watchdog_deadline(
        started,
        started,
        &timing(Duration::from_secs(2), Duration::from_secs(2)),
    );

    assert_eq!(deadline, started + Duration::from_secs(2));
    assert_eq!(class, FailureClass::WorkerTimeout);
}

#[test]
fn watchdog_deadline_preserves_strict_progress_order_after_both_elapsed() {
    let started = Instant::now();
    let timing = timing(Duration::from_secs(2), Duration::from_secs(1));

    let class = elapsed_watchdog_class(started + Duration::from_secs(3), started, started, &timing);

    assert_eq!(class, Some(FailureClass::ProgressTimeout));
}

#[test]
fn watchdog_deadline_preserves_strict_heartbeat_order_after_both_elapsed() {
    let started = Instant::now();
    let timing = timing(Duration::from_secs(1), Duration::from_secs(2));

    let class = elapsed_watchdog_class(started + Duration::from_secs(3), started, started, &timing);

    assert_eq!(class, Some(FailureClass::WorkerTimeout));
}

#[test]
fn watchdog_deadline_is_not_elapsed_before_the_selected_deadline() {
    let started = Instant::now();
    let timing = timing(Duration::from_secs(2), Duration::from_secs(1));

    let class = elapsed_watchdog_class(
        started + Duration::from_millis(999),
        started,
        started,
        &timing,
    );

    assert_eq!(class, None);
}
