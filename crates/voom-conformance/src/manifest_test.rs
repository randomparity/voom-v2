use super::*;

const VALID: &str = r#"
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true

[scaffold]
binaries = ["chaos-worker", "benchmark-worker"]
"#;

#[test]
fn parses_active_and_scaffold_entries() {
    let manifest = Manifest::parse_str(VALID).unwrap();
    assert_eq!(manifest.active[0].name, "echo-worker");
    assert_eq!(manifest.active[0].target, "echo-worker");
    assert_eq!(manifest.scaffold, vec!["chaos-worker", "benchmark-worker"]);
}

#[test]
fn rejects_active_entry_without_required_true() {
    let raw = VALID.replace("required = true", "required = false");
    let err = Manifest::parse_str(&raw).unwrap_err();
    assert!(err.to_string().contains("required=true"));
}

#[test]
fn rejects_active_entry_with_non_active_status() {
    let raw = VALID.replace("status = \"active\"", "status = \"scaffold\"");
    let err = Manifest::parse_str(&raw).unwrap_err();
    assert!(err.to_string().contains("status=active"));
}

#[test]
fn rejects_binary_listed_as_active_and_scaffold() {
    let raw = VALID.replace(
        "binaries = [\"chaos-worker\", \"benchmark-worker\"]",
        "binaries = [\"echo-worker\"]",
    );
    let err = Manifest::parse_str(&raw).unwrap_err();
    assert!(err.to_string().contains("active and scaffold"));
}

#[test]
fn resolves_active_from_cargo_bin_env() {
    let entry = ActiveBinary {
        name: "echo-worker".to_owned(),
        target: "echo-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        path: None,
    };
    let path = resolve_active_with(&entry, |k| {
        (k == "CARGO_BIN_EXE_echo-worker").then(|| "/tmp/echo-worker".into())
    })
    .unwrap();
    assert_eq!(path, std::path::PathBuf::from("/tmp/echo-worker"));
}

#[test]
fn missing_active_binary_is_error() {
    let entry = ActiveBinary {
        name: "echo-worker".to_owned(),
        target: "echo-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        path: None,
    };
    let err = resolve_active_with(&entry, |_| None).unwrap_err();
    assert!(err.to_string().contains("CARGO_BIN_EXE_echo-worker"));
}
