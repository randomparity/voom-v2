use std::io::ErrorKind;

use super::*;

#[test]
fn load_scenario_preserves_missing_file_path_and_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("missing.json");

    let error = load_scenario(&path).unwrap_err();

    let ScenarioError::Read {
        path: error_path,
        source,
    } = error
    else {
        panic!("missing scenario must report a read error");
    };
    assert_eq!(error_path, path);
    assert_eq!(source.kind(), ErrorKind::NotFound);
}

#[test]
fn load_scenario_preserves_malformed_file_path_and_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("malformed.json");
    std::fs::write(&path, b"{").unwrap();

    let error = load_scenario(&path).unwrap_err();

    let ScenarioError::Decode {
        path: error_path,
        source,
    } = error
    else {
        panic!("malformed scenario must report a decode error");
    };
    assert_eq!(error_path, path);
    assert_eq!(source.classify(), serde_json::error::Category::Eof);
}
