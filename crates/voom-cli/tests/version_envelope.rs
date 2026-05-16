#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use serde_json::Value;
use voom_cli::commands::version::build_version_info;

#[test]
fn version_envelope_shape() {
    let info = build_version_info("0.1.0-dev", "abc1234", false, "debug");

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!("version_dev", &info);
    });
}

#[test]
fn release_flag_is_true_only_when_no_prerelease() {
    let dev = build_version_info("0.1.0-dev", "abc1234", false, "debug");
    let rel = build_version_info("0.1.0", "def5678", false, "release");
    let dirty = build_version_info("0.1.0-dev", "abc1234", true, "debug");

    assert!(!dev.release);
    assert!(rel.release);
    assert!(dirty.version.ends_with(".dirty"));
}

#[test]
fn version_envelope_serializes_as_expected_keys() {
    let info = build_version_info("0.1.0-dev", "abc1234", false, "debug");
    let json: Value = serde_json::to_value(&info).unwrap();
    let obj = json.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["build_profile", "dirty", "git_sha", "release", "semver", "version"]
    );
}
