use serde::{Deserialize, Serialize};
use voom_core::{LibraryId, NodeId, StorageProviderKind, StorageRootId, StorageRootState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootCreatedPayload {
    pub storage_root_id: StorageRootId,
    pub library_id: LibraryId,
    pub owner_node_id: NodeId,
    pub provider_kind: StorageProviderKind,
    pub state: StorageRootState,
    pub root_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootOwnerAssignedPayload {
    pub storage_root_id: StorageRootId,
    pub owner_node_id: NodeId,
    pub state: StorageRootState,
    pub root_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootActivatedPayload {
    pub storage_root_id: StorageRootId,
    pub owner_node_id: NodeId,
    pub activation_identity: String,
    pub root_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootValidationLostPayload {
    pub storage_root_id: StorageRootId,
    pub owner_node_id: NodeId,
    pub reason: String,
    pub root_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootReactivatedPayload {
    pub storage_root_id: StorageRootId,
    pub owner_node_id: NodeId,
    pub activation_identity: String,
    pub root_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootRetiredPayload {
    pub storage_root_id: StorageRootId,
    pub owner_node_id: Option<NodeId>,
    pub root_epoch: u64,
}

#[cfg(test)]
#[path = "storage_test.rs"]
mod tests;
