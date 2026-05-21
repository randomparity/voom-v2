pub async fn run_active_worker(_launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    result.fail(
        "raw_wire_suite::pending_task_4",
        "raw-wire suite pending Task 4",
    );
    result
}
