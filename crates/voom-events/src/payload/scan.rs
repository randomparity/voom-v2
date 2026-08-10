use serde::{Deserialize, Serialize};
use voom_core::{ScanSessionId, ScanSessionStatus, StorageRootId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSessionLifecyclePayload {
    pub scan_session_id: ScanSessionId,
    pub storage_root_id: StorageRootId,
    pub status: ScanSessionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservationBatchAcceptedPayload {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub batch_observation_count: u64,
    pub cumulative_observation_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSessionSucceededPayload {
    pub scan_session_id: ScanSessionId,
    pub storage_root_id: StorageRootId,
    pub observation_count: u64,
    pub retired_location_count: u64,
}

#[cfg(test)]
#[path = "scan_test.rs"]
mod tests;
