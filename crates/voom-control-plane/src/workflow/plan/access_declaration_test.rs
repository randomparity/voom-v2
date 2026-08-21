use super::*;

use voom_core::ArtifactAccessRight::{Delete, Read, Write};

const ROOT: StorageRootId = StorageRootId(7);
const LOCATION: FileLocationId = FileLocationId(42);

fn location_source() -> TicketStorageSource {
    TicketStorageSource::Location {
        storage_root_id: ROOT,
        file_location_id: LOCATION,
    }
}

fn root_source() -> TicketStorageSource {
    TicketStorageSource::Root {
        storage_root_id: ROOT,
    }
}

fn root_entry(rights: &[ArtifactAccessRight]) -> ArtifactAccessEntry {
    entry(root_target(ROOT), rights)
}

fn location_entry(rights: &[ArtifactAccessRight]) -> ArtifactAccessEntry {
    entry(location_target(ROOT, LOCATION), rights)
}

/// The expected declaration for one operation, per source variant.
///
/// `None` means the operation is not byte-touching.
type ExpectedCell = Option<(Vec<ArtifactAccessEntry>, Vec<ArtifactAccessEntry>)>;

/// Every cell of the mapping table, transcribed literally.
fn mapping() -> Vec<(OperationKind, ExpectedCell)> {
    let read_only = || (vec![location_entry(&[Read])], vec![root_entry(&[Read])]);
    let read_write = || {
        (
            vec![root_entry(&[Write]), location_entry(&[Read])],
            vec![root_entry(&[Read, Write])],
        )
    };
    vec![
        // scan_library projects a location source to its root.
        (
            OperationKind::ScanLibrary,
            Some((vec![root_entry(&[Read])], vec![root_entry(&[Read])])),
        ),
        (OperationKind::ProbeFile, Some(read_only())),
        (OperationKind::HashFile, Some(read_only())),
        (OperationKind::VerifyArtifact, Some(read_only())),
        (OperationKind::Remux, Some(read_write())),
        (OperationKind::TranscodeVideo, Some(read_write())),
        (OperationKind::TranscodeAudio, Some(read_write())),
        (OperationKind::ExtractAudio, Some(read_write())),
        (OperationKind::EditTracks, Some(read_write())),
        (OperationKind::BackUpFile, Some(read_write())),
        (OperationKind::CommitArtifact, Some(read_write())),
        (
            OperationKind::DeleteArtifact,
            Some((
                vec![location_entry(&[Read, Delete])],
                vec![root_entry(&[Read, Delete])],
            )),
        ),
        (OperationKind::IdentifyMedia, None),
        (OperationKind::ScoreQuality, None),
        (OperationKind::SyncExternalSystem, None),
    ]
}

#[test]
fn the_mapping_table_covers_every_operation_exactly_once() {
    let table = mapping();
    assert_eq!(table.len(), OperationKind::ALL.len());
    for operation in OperationKind::ALL {
        assert_eq!(
            table
                .iter()
                .filter(|(candidate, _)| candidate == operation)
                .count(),
            1,
            "coverage of {operation:?}"
        );
    }
}

#[test]
fn declaration_for_matches_the_mapping_for_every_operation_and_source() {
    for (operation, expected) in mapping() {
        let Some((from_location, from_root)) = expected else {
            assert_eq!(
                declaration_for(operation, Some(&location_source())).unwrap(),
                None,
                "{operation:?} is not byte-touching"
            );
            assert_eq!(
                declaration_for(operation, Some(&root_source())).unwrap(),
                None,
                "{operation:?} is not byte-touching"
            );
            continue;
        };
        let cases = [
            (location_source(), from_location),
            (root_source(), from_root),
        ];
        for (source, expected_entries) in cases {
            let declaration = declaration_for(operation, Some(&source))
                .unwrap()
                .unwrap_or_else(|| panic!("{operation:?} declared nothing"));
            assert_eq!(
                declaration.entries(),
                expected_entries.as_slice(),
                "{operation:?} against {source:?}"
            );
        }
    }
}

#[test]
fn every_produced_declaration_is_canonical_in_construction_order() {
    // `declaration_for` does not sort. If it built entries out of order, `new`
    // would reject them, so this proves the table is transcribed canonically.
    for operation in OperationKind::ALL {
        for source in [location_source(), root_source()] {
            let produced = declaration_for(*operation, Some(&source)).unwrap();
            let Some(declaration) = produced else {
                continue;
            };
            assert_eq!(
                ArtifactAccessDeclaration::new(declaration.entries().to_vec()).unwrap(),
                declaration,
                "{operation:?} against {source:?}"
            );
        }
    }
}

#[test]
fn scan_library_projects_a_location_source_to_its_root() {
    let declaration = declaration_for(OperationKind::ScanLibrary, Some(&location_source()))
        .unwrap()
        .unwrap();

    assert_eq!(declaration.entries(), [root_entry(&[Read])]);
    assert_eq!(declaration.file_location_ids().count(), 0);
}

#[test]
fn a_byte_touching_operation_without_a_source_is_a_config_error() {
    for operation in OperationKind::ALL.iter().filter(|o| o.is_byte_touching()) {
        let error = declaration_for(*operation, None).unwrap_err();
        assert!(
            matches!(error, VoomError::Config(_)),
            "{operation:?} returned {error:?}"
        );
        assert!(
            error.to_string().contains(operation.as_str()),
            "message names the operation: {error}"
        );
    }
}

#[test]
fn a_non_byte_touching_operation_declares_nothing_even_without_a_source() {
    for operation in OperationKind::ALL.iter().filter(|o| !o.is_byte_touching()) {
        assert_eq!(declaration_for(*operation, None).unwrap(), None);
    }
}

#[test]
fn a_root_source_never_names_a_location() {
    for operation in OperationKind::ALL {
        let Some(declaration) = declaration_for(*operation, Some(&root_source())).unwrap() else {
            continue;
        };
        assert_eq!(
            declaration.file_location_ids().count(),
            0,
            "{operation:?} named a location from a root source"
        );
        for root in declaration.storage_root_ids() {
            assert_eq!(root, ROOT);
        }
    }
}
