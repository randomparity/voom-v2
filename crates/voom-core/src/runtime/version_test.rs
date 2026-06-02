use super::*;

#[test]
fn dev_build_is_not_release() {
    let v = VersionInfo::new("0.1.0-dev", "abc1234", false, "debug");
    assert!(!v.release);
    assert_eq!(v.version, "0.1.0-dev+abc1234");
}

#[test]
fn tagged_build_is_release() {
    let v = VersionInfo::new("0.1.0", "def5678", false, "release");
    assert!(v.release);
    assert_eq!(v.version, "0.1.0+def5678");
}

#[test]
fn dirty_tree_appends_dirty_suffix() {
    let v = VersionInfo::new("0.1.0-dev", "abc1234", true, "debug");
    assert_eq!(v.version, "0.1.0-dev+abc1234.dirty");
}

#[test]
fn unknown_sha_still_renders() {
    let v = VersionInfo::new("0.1.0-dev", "unknown", false, "debug");
    assert_eq!(v.version, "0.1.0-dev+unknown");
}
