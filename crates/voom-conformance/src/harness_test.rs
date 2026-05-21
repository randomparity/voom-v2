use super::*;

#[test]
fn suite_result_merges_passes_and_failures() {
    let mut a = SuiteResult::default();
    a.pass("a");
    a.fail("b", "bad");
    let mut b = SuiteResult::default();
    b.pass("c");
    a.extend(b);
    assert_eq!(a.passed, vec!["a", "c"]);
    assert_eq!(a.failed, vec![("b".to_owned(), "bad".to_owned())]);
}

#[test]
fn empty_active_suite_becomes_failure() {
    let mut result = SuiteResult::default();
    result.fail_if_empty_for("echo-worker");
    assert!(!result.all_passed());
    assert_eq!(result.failed[0].0, "echo-worker::empty_suite");
}
