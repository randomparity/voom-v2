use super::*;

#[test]
fn parses_supported_mkvmerge_version() {
    let version = parse_mkvmerge_version("mkvmerge v80.0 ('Roundabout') 64-bit").unwrap();

    assert_eq!(version.major, 80);
}

#[test]
fn rejects_unsupported_mkvmerge_version() {
    let err = parse_mkvmerge_version("mkvmerge v40.0").unwrap_err();

    assert!(err.to_string().contains("unsupported mkvmerge version"));
}

#[test]
fn preflight_rejects_missing_mkvmerge() {
    let temp = tempfile::tempdir().unwrap();

    let err = preflight_mkvmerge(&temp.path().join("missing-mkvmerge")).unwrap_err();

    assert!(err.to_string().contains("missing or not executable"));
}

#[test]
fn preflight_rejects_non_executable_mkvmerge() {
    let temp = tempfile::tempdir().unwrap();
    let command = temp.path().join("mkvmerge");
    std::fs::write(&command, "#!/bin/sh\nprintf '%s\\n' 'mkvmerge v80.0'\n").unwrap();

    let err = preflight_mkvmerge(&command).unwrap_err();

    assert!(err.to_string().contains("missing or not executable"));
}

#[test]
fn preflight_rejects_unsupported_mkvmerge_binary_version() {
    let temp = tempfile::tempdir().unwrap();
    let command = stub_bin(
        temp.path(),
        "mkvmerge",
        "#!/bin/sh\nprintf '%s\\n' 'mkvmerge v40.0'\n",
    );

    let err = preflight_mkvmerge(&command).unwrap_err();

    assert!(err.to_string().contains("unsupported mkvmerge version"));
}

#[test]
fn preflight_accepts_supported_mkvmerge_binary_version() {
    let temp = tempfile::tempdir().unwrap();
    let command = stub_bin(
        temp.path(),
        "mkvmerge",
        "#!/bin/sh\nprintf '%s\\n' \"mkvmerge v80.0 ('Roundabout') 64-bit\"\n",
    );

    let config = preflight_mkvmerge(&command).unwrap();

    assert_eq!(config.command, command);
    assert!(config.provider_version.contains("mkvmerge v80.0"));
}

fn stub_bin(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    voom_test_support::executable::write_executable(&path, body).unwrap();
    path
}

#[test]
fn preflight_runs_mkvmerge_with_utf8_locale() {
    let temp = tempfile::tempdir().unwrap();
    let command = stub_bin(
        temp.path(),
        "mkvmerge",
        "#!/bin/sh\n[ \"$LC_ALL\" = C.UTF-8 ] || { echo \"LC_ALL=$LC_ALL\" >&2; exit 3; }\n\
         printf '%s\\n' \"mkvmerge v80.0 ('Roundabout') 64-bit\"\n",
    );

    preflight_mkvmerge(&command).unwrap();
}
