//! Sibling tests for [`super::owner_access_evidence`] — the durable evidence
//! contract (ADR 0071). These pin the *encoding*, not just behavior: a test
//! that cannot fail when the wire shape drifts is wrong.

use super::*;
use crate::taxonomy::artifact_access_declaration::{
    ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessRight, ExistingArtifactAccess,
    FileLocationAccess, PlannedArtifactAccess, StorageRootAccess,
};
use crate::taxonomy::ids::{ArtifactHandleId, FileLocationId, StorageRootId};

fn entry(target: ArtifactAccessTarget, rights: &[ArtifactAccessRight]) -> ArtifactAccessEntry {
    ArtifactAccessEntry {
        target,
        rights: rights.to_vec(),
    }
}

fn declaration() -> ArtifactAccessDeclaration {
    // Canonical ascending order: storage_root < file_location < existing_artifact
    // < planned_artifact. Two distinct roots so epoch-set checks have room.
    ArtifactAccessDeclaration::new(vec![
        entry(
            ArtifactAccessTarget::StorageRoot(StorageRootAccess {
                storage_root_id: StorageRootId(7),
            }),
            &[ArtifactAccessRight::Read, ArtifactAccessRight::Write],
        ),
        entry(
            ArtifactAccessTarget::FileLocation(FileLocationAccess {
                storage_root_id: StorageRootId(9),
                file_location_id: FileLocationId(11),
            }),
            &[ArtifactAccessRight::Read],
        ),
        entry(
            ArtifactAccessTarget::ExistingArtifact(ExistingArtifactAccess {
                artifact_handle_id: ArtifactHandleId(13),
                storage_root_id: StorageRootId(9),
                file_location_id: FileLocationId(12),
            }),
            &[ArtifactAccessRight::Read],
        ),
        entry(
            ArtifactAccessTarget::PlannedArtifact(PlannedArtifactAccess {
                artifact_handle_id: ArtifactHandleId(15),
                target_storage_root_id: StorageRootId(7),
            }),
            &[ArtifactAccessRight::Write],
        ),
    ])
    .unwrap()
}

fn epochs(ids: &[(u64, u64)]) -> Vec<RootEpoch> {
    ids.iter()
        .map(|&(id, epoch)| RootEpoch {
            storage_root_id: StorageRootId(id),
            root_epoch: epoch,
        })
        .collect()
}

#[test]
fn owner_evidence_round_trips_canonically() {
    let evidence = OwnerAccessEvidence::new(declaration(), epochs(&[(7, 3), (9, 0)])).unwrap();

    let json = serde_json::to_string(&evidence).unwrap();
    let decoded: OwnerAccessEvidence = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, evidence);
}

#[test]
fn frozen_owner_evidence_encoding() {
    // String comparison, not Value: preserve_order makes Value equality blind
    // to key order, and key order is wire contract here.
    let evidence = OwnerAccessEvidence::new(declaration(), epochs(&[(7, 3), (9, 0)])).unwrap();
    let json = serde_json::to_string(&evidence).unwrap();
    assert!(json.contains(r#"{"declaration":"#));
    assert!(json.contains(r#"{"storage_root_id":7,"root_epoch":3}"#));
    assert!(json.contains(r#"{"storage_root_id":9,"root_epoch":0}"#));
}

#[test]
fn empty_root_epochs_rejected() {
    let err = OwnerAccessEvidence::new(declaration(), Vec::new()).unwrap_err();
    assert!(err.to_string().contains("at least one root epoch"));
}

#[test]
fn unordered_or_duplicate_root_epochs_rejected() {
    let declaration = declaration();
    let err = OwnerAccessEvidence::new(declaration.clone(), epochs(&[(9, 1), (7, 2)])).unwrap_err();
    assert!(err.to_string().contains("strictly ascending"));

    // Duplicates fail the same strictness rule.
    let err = OwnerAccessEvidence::new(declaration.clone(), epochs(&[(7, 1), (7, 2)])).unwrap_err();
    assert!(err.to_string().contains("strictly ascending"));
    let _ = declaration;
}

#[test]
fn epoch_set_must_equal_declared_roots() {
    // Missing root 9 (referenced by file_location and existing_artifact).
    let err = OwnerAccessEvidence::new(declaration(), epochs(&[(7, 1)])).unwrap_err();
    assert!(
        err.to_string()
            .contains("do not match the declared storage roots")
    );

    // Extra root not referenced anywhere.
    let err =
        OwnerAccessEvidence::new(declaration(), epochs(&[(7, 1), (9, 1), (12, 4)])).unwrap_err();
    assert!(
        err.to_string()
            .contains("do not match the declared storage roots")
    );
}

#[test]
fn unknown_field_rejected_on_decode() {
    // deny_unknown_fields is load-bearing on the REAL serde unit: an unknown
    // field in a persisted row must fail decode, never default.
    let err = serde_json::from_str::<OwnerAccessEvidence>(
        r#"{"declaration":[{"target":{"kind":"storage_root","storage_root_id":7},
            "rights":["read"]}],"root_epochs":[{"storage_root_id":7,"root_epoch":1}],
            "mount":"/mnt/evil"}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}
#[test]
fn negative_epoch_rejected_as_corrupt() {
    // ADR 0070: negative root epoch is corrupt; u64 decode fails closed.
    let err = serde_json::from_str::<OwnerAccessEvidence>(
        r#"{"declaration":[{"target":{"kind":"storage_root","storage_root_id":7},
            "rights":["read"]}],"root_epochs":[{"storage_root_id":7,"root_epoch":-1}]}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("invalid value"), "{err}");
}

#[test]
fn rejection_evidence_round_trips_and_orders_targets() {
    let declaration = declaration();
    let references: Vec<AccessReferenceRejection> = declaration
        .entries()
        .iter()
        .map(|entry| AccessReferenceRejection {
            target: entry.target.clone(),
            reason: AccessReferenceReason::InvalidRootState,
        })
        .collect();
    let evidence = AccessRejectionEvidence::new(references).unwrap();

    let json = serde_json::to_string(&evidence).unwrap();
    let decoded: AccessRejectionEvidence = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, evidence);
    for (decoded, declared) in decoded.references.iter().zip(declaration.entries()) {
        assert_eq!(&decoded.target, &declared.target);
    }
    assert_eq!(decoded.references[0].reason.as_str(), "invalid_root_state");
}

#[test]
fn empty_rejection_references_rejected() {
    let err = AccessRejectionEvidence::new(Vec::new()).unwrap_err();
    assert!(err.to_string().contains("at least one reference"));
}

#[test]
fn unordered_rejection_targets_rejected() {
    let declaration = declaration();
    let mut references: Vec<AccessReferenceRejection> = declaration
        .entries()
        .iter()
        .map(|entry| AccessReferenceRejection {
            target: entry.target.clone(),
            reason: AccessReferenceReason::MixedOwner,
        })
        .collect();
    references.swap(0, 1);
    let err = AccessRejectionEvidence::new(references).unwrap_err();
    assert!(err.to_string().contains("strictly ascending"));

    // Decode path enforces the identical rule (constructor = deserializer).
    let duplicated = vec![
        AccessReferenceRejection {
            target: declaration.entries()[0].target.clone(),
            reason: AccessReferenceReason::MixedOwner,
        },
        AccessReferenceRejection {
            target: declaration.entries()[0].target.clone(),
            reason: AccessReferenceReason::MixedOwner,
        },
    ];
    let err = AccessRejectionEvidence::new(duplicated).unwrap_err();
    assert!(err.to_string().contains("strictly ascending"));
}

#[test]
fn decision_access_evidence_tag_discriminates_and_rejects_unknown() {
    let owner = DecisionAccessEvidence::Owner(
        OwnerAccessEvidence::new(declaration(), epochs(&[(7, 3), (9, 0)])).unwrap(),
    );
    let json = serde_json::to_string(&owner).unwrap();
    assert!(json.starts_with(r#"{"evidence":"owner""#), "{json}");
    let decoded: DecisionAccessEvidence = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, owner);
    let rejected = DecisionAccessEvidence::Rejected(
        AccessRejectionEvidence::new(vec![AccessReferenceRejection {
            target: declaration().entries()[0].target.clone(),
            reason: AccessReferenceReason::StorageRootNotFound,
        }])
        .unwrap(),
    );
    let json = serde_json::to_string(&rejected).unwrap();
    assert!(json.starts_with(r#"{"evidence":"rejected""#), "{json}");
    let decoded: DecisionAccessEvidence = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, rejected);

    // Unknown variant names must never decode into a default.
    let err = serde_json::from_str::<DecisionAccessEvidence>(r#"{"evidence":"shared_mount"}"#)
        .unwrap_err();
    assert!(err.to_string().contains("unknown variant"), "{err}");
}
