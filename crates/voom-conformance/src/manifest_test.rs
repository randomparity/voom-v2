use super::*;
use voom_worker_protocol::OperationKind;

const VALID: &str = r#"
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true

[[binaries.operations]]
operation = "probe_file"
valid_payload = { path = "/library/example.mkv" }
invalid_payload = { }

[[binaries]]
name = "benchmark-worker"
target = "benchmark-worker"
status = "active"
required = true

[[binaries.operations]]
operation = "probe_file"
valid_payload = { path = "/library/example.mkv" }
invalid_payload = { }

[scaffold]
binaries = ["chaos-worker"]
"#;

#[test]
fn parses_active_and_scaffold_entries() {
    let manifest = Manifest::parse_str(VALID).unwrap();
    assert_eq!(
        manifest
            .active
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["echo-worker", "benchmark-worker"]
    );
    assert_eq!(manifest.scaffold, vec!["chaos-worker"]);
    assert_eq!(
        manifest.active[0].operations[0].operation,
        OperationKind::ProbeFile
    );
    assert_eq!(
        manifest.active[0].operations[0].valid_payload,
        serde_json::json!({"path": "/library/example.mkv"})
    );
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
        "binaries = [\"chaos-worker\"]",
        "binaries = [\"benchmark-worker\"]",
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
        operations: probe_operations(),
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
        operations: probe_operations(),
        path: None,
    };
    let err = resolve_active_with(&entry, |_| None).unwrap_err();
    assert!(err.to_string().contains("CARGO_BIN_EXE_echo-worker"));
}

#[test]
fn explicit_path_takes_precedence_over_target_dir_fallback() {
    let entry = ActiveBinary {
        name: "chaos-worker".to_owned(),
        target: "chaos-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        operations: probe_operations(),
        path: Some(std::path::PathBuf::from("/explicit/chaos-worker")),
    };
    let path =
        resolve_active_with_sources(&entry, |_| None, Some(std::path::Path::new("/tmp/target")))
            .unwrap();
    assert_eq!(path, std::path::PathBuf::from("/explicit/chaos-worker"));
}

#[test]
fn resolves_cross_package_binary_from_target_dir_fallback() {
    let entry = ActiveBinary {
        name: "chaos-worker".to_owned(),
        target: "chaos-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        operations: probe_operations(),
        path: None,
    };
    let path =
        resolve_active_with_sources(&entry, |_| None, Some(std::path::Path::new("/tmp/target")))
            .unwrap();
    assert_eq!(
        path,
        std::path::PathBuf::from("/tmp/target/debug/chaos-worker")
    );
}

#[test]
fn default_target_dir_fallback_points_at_workspace_target_dir() {
    let dir = default_target_dir();
    assert!(dir.ends_with("target"), "{dir:?}");
}

#[test]
fn rejects_active_entry_without_operation_cases() {
    let raw = r#"
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true
"#;
    let err = Manifest::parse_str(raw).unwrap_err();
    assert!(err.to_string().contains("operation case"));
}

#[test]
fn rejects_non_object_operation_payloads() {
    let raw = r#"
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true

[[binaries.operations]]
operation = "probe_file"
valid_payload = "not-an-object"
invalid_payload = { }
"#;
    let err = Manifest::parse_str(raw).unwrap_err();
    assert!(err.to_string().contains("valid_payload"));
}

#[test]
fn operation_coverage_accepts_every_fixed_operation() {
    let manifest = Manifest::parse_str(&manifest_with_operations(OperationKind::ALL)).unwrap();
    validate_operation_coverage(&manifest).unwrap();
}

#[test]
fn operation_coverage_reports_missing_operation() {
    let operations = OperationKind::ALL
        .iter()
        .copied()
        .filter(|operation| *operation != OperationKind::DeleteArtifact)
        .collect::<Vec<_>>();
    let manifest = Manifest::parse_str(&manifest_with_operations(&operations)).unwrap();
    let err = validate_operation_coverage(&manifest).unwrap_err();
    assert!(err.to_string().contains("DeleteArtifact"));
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn manifest_with_operations(operations: &[OperationKind]) -> String {
    let mut raw = String::from(
        r#"
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true
"#,
    );
    for operation in operations {
        raw.push_str(&format!(
            r#"
[[binaries.operations]]
operation = "{}"
valid_payload = {{ path = "/library/example.mkv" }}
invalid_payload = {{ }}
"#,
            operation_name(*operation)
        ));
    }
    raw
}

fn probe_operations() -> Vec<OperationCase> {
    vec![OperationCase {
        operation: OperationKind::ProbeFile,
        valid_payload: serde_json::json!({"path": "/library/example.mkv"}),
        invalid_payload: serde_json::json!({}),
    }]
}
