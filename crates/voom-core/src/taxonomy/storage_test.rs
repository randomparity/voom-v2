use super::*;

#[test]
fn storage_provider_and_root_state_tokens_are_stable() {
    assert_eq!(
        StorageProviderKind::LocalFilesystem.as_str(),
        "local_filesystem"
    );
    assert_eq!(
        StorageProviderKind::from_wire("local_filesystem"),
        Some(StorageProviderKind::LocalFilesystem)
    );
    assert_eq!(StorageProviderKind::from_wire("shared_mount"), None);

    let states = [
        (StorageRootState::Unassigned, "unassigned"),
        (StorageRootState::Configured, "configured"),
        (StorageRootState::Active, "active"),
        (StorageRootState::Unavailable, "unavailable"),
        (StorageRootState::Retired, "retired"),
    ];
    for (state, token) in states {
        assert_eq!(state.as_str(), token);
        assert_eq!(StorageRootState::from_wire(token), Some(state));
        assert_eq!(serde_json::to_value(state).unwrap(), token);
        assert_eq!(
            serde_json::from_value::<StorageRootState>(token.into()).unwrap(),
            state
        );
    }
    assert_eq!(StorageRootState::from_wire("disabled"), None);
}

#[test]
fn provider_locator_enforces_opaque_configuration_bounds() {
    let locator = ProviderLocator::new("/srv/media".to_owned()).unwrap();
    assert_eq!(locator.as_str(), "/srv/media");
    assert_eq!(serde_json::to_value(&locator).unwrap(), "/srv/media");
    assert_eq!(
        serde_json::from_value::<ProviderLocator>(serde_json::json!("/srv/media")).unwrap(),
        locator
    );

    for invalid in ["", "bad\0locator"] {
        assert!(
            ProviderLocator::new(invalid.to_owned()).is_err(),
            "{invalid:?}"
        );
    }
    assert!(ProviderLocator::new("x".repeat(4097)).is_err());
}

#[test]
fn provider_relative_locator_rejects_escape_and_ambiguous_shapes() {
    let locator = ProviderRelativeLocator::new("films/Alien (1979).mkv".to_owned()).unwrap();
    assert_eq!(locator.as_str(), "films/Alien (1979).mkv");
    assert_eq!(
        serde_json::from_value::<ProviderRelativeLocator>(serde_json::json!(
            "films/Alien (1979).mkv"
        ))
        .unwrap(),
        locator
    );

    for invalid in [
        "",
        "/absolute",
        "trailing/",
        "double//separator",
        ".",
        "..",
        "./file",
        "dir/./file",
        "dir/../file",
        "dir\\file",
        "C:/windows",
        "C:windows",
        "bad\0locator",
    ] {
        assert!(
            ProviderRelativeLocator::new(invalid.to_owned()).is_err(),
            "{invalid:?}"
        );
    }
    assert!(ProviderRelativeLocator::new("x".repeat(4097)).is_err());
}

#[test]
fn database_parsers_reclassify_corrupt_persisted_storage_values() {
    let provider =
        StorageProviderKind::parse_database("library_roots.provider_kind", "s3").unwrap_err();
    assert!(provider.to_string().contains("library_roots.provider_kind"));

    let relative = ProviderRelativeLocator::parse_database(
        "file_locations.provider_relative_locator",
        "../escape",
    )
    .unwrap_err();
    assert!(
        relative
            .to_string()
            .contains("file_locations.provider_relative_locator")
    );
}
