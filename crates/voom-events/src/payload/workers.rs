use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::{NodeKind, NodeStatus, TicketOperation, WorkerKind};

// --- nodes ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRegisteredPayload {
    pub node_id: u64,
    pub name: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub heartbeat_ttl_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHeartbeatRecordedPayload {
    pub node_id: u64,
    pub status: NodeStatus,
    #[serde(with = "time::serde::iso8601")]
    pub last_seen_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMarkedStalePayload {
    pub node_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub marked_stale_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRetiredPayload {
    pub node_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub retired_at: OffsetDateTime,
    pub epoch: u64,
}

// --- workers ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRegisteredPayload {
    pub worker_id: u64,
    pub name: String,
    pub kind: WorkerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerLinkedToNodePayload {
    pub worker_id: u64,
    pub node_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCapabilityRecordedPayload {
    pub worker_id: u64,
    pub capability_id: u64,
    pub operation: TicketOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerGrantRecordedPayload {
    pub worker_id: u64,
    pub grant_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRetiredPayload {
    pub worker_id: u64,
}

#[cfg(test)]
#[path = "workers_test.rs"]
mod tests;
