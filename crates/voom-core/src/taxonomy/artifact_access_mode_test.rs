use super::*;

#[test]
fn artifact_access_mode_tokens_are_stable_across_boundaries() {
    let cases = [
        (ArtifactAccessMode::SharedMount, "shared_mount"),
        (
            ArtifactAccessMode::ControlPlanePlaceholder,
            "control_plane_placeholder",
        ),
        (
            ArtifactAccessMode::StagedOutputPlaceholder,
            "staged_output_placeholder",
        ),
    ];

    for (mode, token) in cases {
        assert_eq!(mode.as_str(), token);
        assert_eq!(serde_json::to_value(mode).unwrap(), token);
        assert_eq!(
            serde_json::from_value::<ArtifactAccessMode>(token.into()).unwrap(),
            mode
        );
        assert_eq!(ArtifactAccessMode::from_wire(token), Some(mode));
    }
}

#[test]
fn artifact_access_mode_rejects_unknown_wire_and_database_tokens() {
    assert_eq!(ArtifactAccessMode::from_wire("local_path"), None);

    let error = ArtifactAccessMode::parse_database(
        "artifact_access_plans.selected_access_mode",
        "local_path",
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("artifact_access_plans.selected_access_mode")
    );
    assert!(error.to_string().contains("local_path"));
}
