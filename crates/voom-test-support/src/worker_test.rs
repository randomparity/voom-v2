use super::*;

#[test]
fn prebuilt_worker_binary_errors_when_the_expected_binary_is_absent() {
    let result = prebuilt_worker_binary("definitely-missing-worker");
    assert!(
        result.is_err(),
        "missing worker unexpectedly resolved to {result:?}"
    );
    let message = result.err().map_or_else(String::new, |err| err.to_string());

    assert!(
        message.contains("prebuilt worker binary missing at"),
        "{message}"
    );
}
