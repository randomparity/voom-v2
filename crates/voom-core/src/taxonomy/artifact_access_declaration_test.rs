use super::*;
use serde_json::json;

fn root(id: u64) -> ArtifactAccessTarget {
    ArtifactAccessTarget::StorageRoot(StorageRootAccess {
        storage_root_id: StorageRootId(id),
    })
}

fn location(root_id: u64, location_id: u64) -> ArtifactAccessTarget {
    ArtifactAccessTarget::FileLocation(FileLocationAccess {
        storage_root_id: StorageRootId(root_id),
        file_location_id: FileLocationId(location_id),
    })
}

fn existing(handle: u64, root_id: u64, location_id: u64) -> ArtifactAccessTarget {
    ArtifactAccessTarget::ExistingArtifact(ExistingArtifactAccess {
        artifact_handle_id: ArtifactHandleId(handle),
        storage_root_id: StorageRootId(root_id),
        file_location_id: FileLocationId(location_id),
    })
}

fn planned(handle: u64, root_id: u64) -> ArtifactAccessTarget {
    ArtifactAccessTarget::PlannedArtifact(PlannedArtifactAccess {
        artifact_handle_id: ArtifactHandleId(handle),
        target_storage_root_id: StorageRootId(root_id),
    })
}

fn entry(target: ArtifactAccessTarget, rights: &[ArtifactAccessRight]) -> ArtifactAccessEntry {
    ArtifactAccessEntry {
        target,
        rights: rights.to_vec(),
    }
}

fn message(error: &VoomError) -> String {
    error.to_string()
}

#[test]
fn rights_tokens_and_order_are_stable() {
    let rights = [
        (ArtifactAccessRight::Read, "read"),
        (ArtifactAccessRight::Write, "write"),
        (ArtifactAccessRight::Delete, "delete"),
    ];
    for (right, token) in rights {
        assert_eq!(right.as_str(), token);
        assert_eq!(ArtifactAccessRight::from_wire(token), Some(right));
        assert_eq!(serde_json::to_value(right).unwrap(), token);
    }
    assert_eq!(ArtifactAccessRight::from_wire("append"), None);
    assert!(ArtifactAccessRight::Read < ArtifactAccessRight::Write);
    assert!(ArtifactAccessRight::Write < ArtifactAccessRight::Delete);
}

#[test]
fn target_variant_order_is_wire_contract() {
    // Reordering ArtifactAccessTarget's variants silently reclassifies every stored
    // declaration as non-canonical, and no guardrail can see it. This is that guard.
    let ascending = [root(1), location(1, 2), existing(3, 1, 2), planned(4, 1)];
    for window in ascending.windows(2) {
        assert!(window[0] < window[1], "variant order changed: {window:?}");
    }
}

#[test]
fn accepts_a_canonical_declaration_and_round_trips_it() {
    let declaration = ArtifactAccessDeclaration::new(vec![
        entry(root(7), &[ArtifactAccessRight::Write]),
        entry(location(7, 9), &[ArtifactAccessRight::Read]),
    ])
    .unwrap();
    assert_eq!(declaration.entries().len(), 2);

    let encoded = serde_json::to_value(&declaration).unwrap();
    let decoded: ArtifactAccessDeclaration = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, declaration);
}

#[test]
fn frozen_canonical_encoding_pins_variant_and_field_order() {
    // Compared as a *string*, not as a Value. The workspace builds serde_json with
    // `preserve_order`, so `Value`'s Map is an IndexMap whose PartialEq ignores key
    // order — a Value comparison here would stay green through the field reorder
    // this test exists to catch.
    let declaration = ArtifactAccessDeclaration::new(vec![
        entry(
            root(1),
            &[ArtifactAccessRight::Read, ArtifactAccessRight::Write],
        ),
        entry(location(1, 2), &[ArtifactAccessRight::Read]),
        entry(
            existing(3, 1, 4),
            &[ArtifactAccessRight::Read, ArtifactAccessRight::Delete],
        ),
        entry(planned(4, 5), &[ArtifactAccessRight::Write]),
    ])
    .unwrap();

    let expected = json!([
        {
            "target": { "kind": "storage_root", "storage_root_id": 1 },
            "rights": ["read", "write"]
        },
        {
            "target": {
                "kind": "file_location",
                "storage_root_id": 1,
                "file_location_id": 2
            },
            "rights": ["read"]
        },
        {
            "target": {
                "kind": "existing_artifact",
                "artifact_handle_id": 3,
                "storage_root_id": 1,
                "file_location_id": 4
            },
            "rights": ["read", "delete"]
        },
        {
            "target": {
                "kind": "planned_artifact",
                "artifact_handle_id": 4,
                "target_storage_root_id": 5
            },
            "rights": ["write"]
        }
    ]);
    assert_eq!(
        serde_json::to_string(&declaration).unwrap(),
        serde_json::to_string(&expected).unwrap(),
        "encoding is not byte-identical to the frozen form"
    );
    assert_eq!(
        serde_json::from_value::<ArtifactAccessDeclaration>(expected).unwrap(),
        declaration
    );
}

#[test]
fn within_variant_field_order_decides_canonical_entry_order() {
    // Two entries of the same variant, differing only in a field after the first.
    // `ArtifactAccessTarget`'s derived `Ord` compares fields in declaration order,
    // so swapping `storage_root_id` and `file_location_id` inside
    // `FileLocationAccess` reverses which of these two sorts first and makes this
    // list non-canonical. The single-entry-per-variant fixture above cannot see
    // that, because no two of its entries share a variant.
    let ascending = vec![
        entry(location(1, 9), &[ArtifactAccessRight::Read]),
        entry(location(2, 3), &[ArtifactAccessRight::Read]),
    ];
    assert!(ArtifactAccessDeclaration::new(ascending.clone()).is_ok());

    let descending = vec![ascending[1].clone(), ascending[0].clone()];
    let error = ArtifactAccessDeclaration::new(descending).unwrap_err();
    assert_eq!(
        message(&error),
        "config error: artifact access entries must be strictly ascending by target; entry 1 \
         does not follow entry 0"
    );
}

#[test]
fn rejects_an_empty_declaration() {
    let error = ArtifactAccessDeclaration::new(Vec::new()).unwrap_err();
    assert_eq!(
        message(&error),
        "config error: artifact access declaration must not be empty"
    );
}

#[test]
fn rejects_an_entry_with_no_rights() {
    let error = ArtifactAccessDeclaration::new(vec![entry(root(1), &[])]).unwrap_err();
    assert_eq!(
        message(&error),
        "config error: artifact access entry 0 must declare at least one right"
    );
}

#[test]
fn rejects_unordered_or_duplicated_rights() {
    let unordered = ArtifactAccessDeclaration::new(vec![entry(
        root(1),
        &[ArtifactAccessRight::Write, ArtifactAccessRight::Read],
    )])
    .unwrap_err();
    assert_eq!(
        message(&unordered),
        "config error: artifact access entry 0 rights must be strictly ascending \
         read < write < delete"
    );

    let duplicated = ArtifactAccessDeclaration::new(vec![entry(
        root(1),
        &[ArtifactAccessRight::Read, ArtifactAccessRight::Read],
    )])
    .unwrap_err();
    assert_eq!(
        message(&duplicated),
        "config error: artifact access entry 0 rights must be strictly ascending \
         read < write < delete"
    );
}

#[test]
fn rejects_unordered_or_duplicated_entries() {
    let unordered = ArtifactAccessDeclaration::new(vec![
        entry(location(1, 2), &[ArtifactAccessRight::Read]),
        entry(root(1), &[ArtifactAccessRight::Write]),
    ])
    .unwrap_err();
    assert_eq!(
        message(&unordered),
        "config error: artifact access entries must be strictly ascending by target; \
         entry 1 does not follow entry 0"
    );

    let duplicated = ArtifactAccessDeclaration::new(vec![
        entry(root(1), &[ArtifactAccessRight::Read]),
        entry(root(1), &[ArtifactAccessRight::Read]),
    ])
    .unwrap_err();
    assert_eq!(
        message(&duplicated),
        "config error: artifact access entries must be strictly ascending by target; \
         entry 1 does not follow entry 0"
    );
}

#[test]
fn rejects_a_file_location_named_by_two_entries() {
    let error = ArtifactAccessDeclaration::new(vec![
        entry(location(1, 5), &[ArtifactAccessRight::Read]),
        entry(existing(9, 1, 5), &[ArtifactAccessRight::Write]),
    ])
    .unwrap_err();
    assert_eq!(
        message(&error),
        "config error: artifact access declares file location 5 in more than one entry"
    );
}

#[test]
fn rejects_an_artifact_handle_named_by_two_entries() {
    let error = ArtifactAccessDeclaration::new(vec![
        entry(existing(9, 1, 5), &[ArtifactAccessRight::Read]),
        entry(planned(9, 2), &[ArtifactAccessRight::Write]),
    ])
    .unwrap_err();
    assert_eq!(
        message(&error),
        "config error: artifact access declares artifact handle 9 in more than one entry"
    );
}

#[test]
fn rejects_every_zero_id_by_field_name() {
    let cases = [
        (root(0), "storage_root_id"),
        (location(0, 2), "storage_root_id"),
        (location(1, 0), "file_location_id"),
        (existing(0, 1, 2), "artifact_handle_id"),
        (planned(4, 0), "target_storage_root_id"),
    ];
    for (target, field) in cases {
        let error =
            ArtifactAccessDeclaration::new(vec![entry(target, &[ArtifactAccessRight::Read])])
                .unwrap_err();
        assert_eq!(
            message(&error),
            format!("config error: artifact access entry 0 has a zero {field}")
        );
    }
}

#[test]
fn deserialization_applies_every_construction_rule() {
    // The wire form has no second accepted encoding: the same rules run on the way in.
    let reordered = json!([
        {
            "target": { "kind": "file_location", "storage_root_id": 1, "file_location_id": 2 },
            "rights": ["read"]
        },
        {
            "target": { "kind": "storage_root", "storage_root_id": 1 },
            "rights": ["write"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(reordered).is_err());

    let reordered_rights = json!([
        {
            "target": { "kind": "storage_root", "storage_root_id": 1 },
            "rights": ["write", "read"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(reordered_rights).is_err());

    let empty = json!([]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(empty).is_err());

    let zero = json!([
        {
            "target": { "kind": "storage_root", "storage_root_id": 0 },
            "rights": ["read"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(zero).is_err());
}

#[test]
fn rejects_unknown_fields_and_unknown_target_kinds() {
    let unknown_entry_field = json!([
        {
            "target": { "kind": "storage_root", "storage_root_id": 1 },
            "rights": ["read"],
            "note": "extra"
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(unknown_entry_field).is_err());

    let unknown_target_field = json!([
        {
            "target": { "kind": "storage_root", "storage_root_id": 1, "path": "/srv" },
            "rights": ["read"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(unknown_target_field).is_err());

    let unknown_kind = json!([
        {
            "target": { "kind": "shared_mount", "storage_root_id": 1 },
            "rights": ["read"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(unknown_kind).is_err());
}

#[test]
fn handle_targets_cannot_drop_their_required_references() {
    // Criterion 2, structurally: an existing handle without a location has no encoding,
    // and a planned handle without a target root has none either.
    let existing_without_location = json!([
        {
            "target": {
                "kind": "existing_artifact",
                "artifact_handle_id": 3,
                "storage_root_id": 1
            },
            "rights": ["read"]
        }
    ]);
    assert!(
        serde_json::from_value::<ArtifactAccessDeclaration>(existing_without_location).is_err()
    );

    let planned_without_root = json!([
        {
            "target": { "kind": "planned_artifact", "artifact_handle_id": 4 },
            "rights": ["write"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(planned_without_root).is_err());

    // The two shapes are not interchangeable.
    let planned_with_location = json!([
        {
            "target": {
                "kind": "planned_artifact",
                "artifact_handle_id": 4,
                "target_storage_root_id": 5,
                "file_location_id": 2
            },
            "rights": ["write"]
        }
    ]);
    assert!(serde_json::from_value::<ArtifactAccessDeclaration>(planned_with_location).is_err());
}

#[test]
fn only_the_ascending_permutation_of_four_entries_is_accepted() {
    let targets = [root(1), location(1, 2), existing(3, 1, 4), planned(5, 6)];
    let mut accepted = 0_u32;
    for permutation in permutations(&targets) {
        let entries = permutation
            .iter()
            .map(|target| entry(target.clone(), &[ArtifactAccessRight::Read]))
            .collect::<Vec<_>>();
        if ArtifactAccessDeclaration::new(entries).is_ok() {
            accepted += 1;
            for window in permutation.windows(2) {
                assert!(window[0] < window[1]);
            }
        }
    }
    assert_eq!(accepted, 1, "exactly one of 24 orderings is canonical");
}

fn permutations(targets: &[ArtifactAccessTarget; 4]) -> Vec<Vec<ArtifactAccessTarget>> {
    let mut out = Vec::new();
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let indexes = [a, b, c, d];
                    let mut seen = [false; 4];
                    if indexes
                        .iter()
                        .any(|index| std::mem::replace(&mut seen[*index], true))
                    {
                        continue;
                    }
                    out.push(
                        indexes
                            .iter()
                            .map(|index| targets[*index].clone())
                            .collect(),
                    );
                }
            }
        }
    }
    out
}
#[test]
fn a_declaration_may_name_two_distinct_storage_roots() {
    // Reading one root and writing another is expressible today. Nothing produces
    // that shape yet — every current producer writes to the root it reads from —
    // but when a producer resolves a distinct output root (#484), it needs no
    // vocabulary change, only a second entry. Roots are the one reference a
    // declaration may repeat, so only the ascending order rule applies.
    let two_roots = ArtifactAccessDeclaration::new(vec![
        entry(root(1), &[ArtifactAccessRight::Read]),
        entry(root(2), &[ArtifactAccessRight::Write]),
    ])
    .unwrap();

    assert_eq!(two_roots.entries().len(), 2);
    assert_eq!(
        two_roots.storage_root_ids().collect::<Vec<_>>(),
        vec![StorageRootId(1), StorageRootId(2)]
    );
}
