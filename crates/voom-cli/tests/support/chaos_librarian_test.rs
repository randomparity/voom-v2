use super::*;

#[test]
fn retained_run_evidence_survives_assertion_unwind_only_when_requested() {
    let root = TempDir::new().unwrap();
    let mut retained_path = PathBuf::new();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let evidence = tempdir_for_evidence(Some(root.path())).unwrap();
        retained_path = evidence.path().to_owned();
        std::fs::write(evidence.path().join("scenario.yaml"), "seed: 248").unwrap();
        std::fs::write(
            evidence.path().join("replay.json"),
            "{\"resolved_seed\":248}",
        )
        .unwrap();
        assert!(
            evidence.path().join("comparison.json").exists(),
            "constructed corpus assertion failure before comparison"
        );
    }));
    assert!(failure.is_err());
    assert!(retained_path.starts_with(root.path()));
    assert!(
        !retained_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .starts_with('.'),
        "retained run roots must cross the collector's hidden-path exclusion"
    );
    assert!(retained_path.join("scenario.yaml").is_file());
    assert!(retained_path.join("replay.json").is_file());
    let ordinary = tempdir_for_evidence(None).unwrap();
    let path = ordinary.path().to_owned();
    drop(ordinary);
    assert!(
        !path.exists(),
        "ordinary runs must retain their existing cleanup"
    );
}
