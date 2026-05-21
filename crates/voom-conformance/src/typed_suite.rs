pub async fn run(_launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    result.fail("typed_suite::pending_task_3", "typed suite pending Task 3");
    result
}
