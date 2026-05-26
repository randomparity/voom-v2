#[test]
fn binary_test_module_is_wired() {
    assert_eq!(env!("CARGO_PKG_NAME"), "voom-mkvtoolnix-worker");
}
