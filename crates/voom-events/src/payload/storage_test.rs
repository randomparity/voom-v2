use serde::Serialize;
use serde::de::DeserializeOwned;
use voom_core::{LibraryId, NodeId, StorageProviderKind, StorageRootId, StorageRootState};

use super::*;
use crate::{Event, EventKind};

fn rejects_unknown<T: Serialize + DeserializeOwned>(value: &T) {
    let mut json = serde_json::to_value(value).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("unknown".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<T>(json).is_err());
}

#[test]
fn root_lifecycle_payloads_round_trip_without_provider_locator() {
    let events = vec![
        Event::StorageRootCreated(StorageRootCreatedPayload {
            storage_root_id: StorageRootId(1),
            library_id: LibraryId(2),
            owner_node_id: NodeId(3),
            provider_kind: StorageProviderKind::LocalFilesystem,
            state: StorageRootState::Configured,
            root_epoch: 0,
        }),
        Event::StorageRootOwnerAssigned(StorageRootOwnerAssignedPayload {
            storage_root_id: StorageRootId(1),
            owner_node_id: NodeId(3),
            state: StorageRootState::Configured,
            root_epoch: 0,
        }),
        Event::StorageRootActivated(StorageRootActivatedPayload {
            storage_root_id: StorageRootId(1),
            owner_node_id: NodeId(3),
            activation_identity: "dev:ino".to_owned(),
            root_epoch: 1,
        }),
        Event::StorageRootValidationLost(StorageRootValidationLostPayload {
            storage_root_id: StorageRootId(1),
            owner_node_id: NodeId(3),
            reason: "not mounted".to_owned(),
            root_epoch: 1,
        }),
        Event::StorageRootReactivated(StorageRootReactivatedPayload {
            storage_root_id: StorageRootId(1),
            owner_node_id: NodeId(3),
            activation_identity: "dev:ino".to_owned(),
            root_epoch: 1,
        }),
        Event::StorageRootRetired(StorageRootRetiredPayload {
            storage_root_id: StorageRootId(1),
            owner_node_id: Some(NodeId(3)),
            root_epoch: 1,
        }),
    ];
    for event in events {
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.pointer("/payload/provider_locator").is_none());
        let kind = event.kind();
        assert_eq!(json["kind"], kind.as_str());
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
    }
}

#[test]
fn root_lifecycle_payloads_reject_unknown_fields() {
    rejects_unknown(&StorageRootCreatedPayload {
        storage_root_id: StorageRootId(1),
        library_id: LibraryId(2),
        owner_node_id: NodeId(3),
        provider_kind: StorageProviderKind::LocalFilesystem,
        state: StorageRootState::Configured,
        root_epoch: 0,
    });
    rejects_unknown(&StorageRootOwnerAssignedPayload {
        storage_root_id: StorageRootId(1),
        owner_node_id: NodeId(3),
        state: StorageRootState::Configured,
        root_epoch: 0,
    });
    rejects_unknown(&StorageRootActivatedPayload {
        storage_root_id: StorageRootId(1),
        owner_node_id: NodeId(3),
        activation_identity: "identity".to_owned(),
        root_epoch: 1,
    });
    rejects_unknown(&StorageRootValidationLostPayload {
        storage_root_id: StorageRootId(1),
        owner_node_id: NodeId(3),
        reason: "reason".to_owned(),
        root_epoch: 1,
    });
    rejects_unknown(&StorageRootReactivatedPayload {
        storage_root_id: StorageRootId(1),
        owner_node_id: NodeId(3),
        activation_identity: "identity".to_owned(),
        root_epoch: 1,
    });
    rejects_unknown(&StorageRootRetiredPayload {
        storage_root_id: StorageRootId(1),
        owner_node_id: Some(NodeId(3)),
        root_epoch: 1,
    });
}

#[test]
fn root_lifecycle_kinds_are_distinct() {
    let kinds = [
        EventKind::StorageRootCreated,
        EventKind::StorageRootOwnerAssigned,
        EventKind::StorageRootActivated,
        EventKind::StorageRootValidationLost,
        EventKind::StorageRootReactivated,
        EventKind::StorageRootRetired,
    ];
    let mut seen = std::collections::HashSet::new();
    for kind in kinds {
        assert!(seen.insert(kind.as_str()));
        assert_eq!(EventKind::from_str(kind.as_str()).unwrap(), kind);
    }
}
