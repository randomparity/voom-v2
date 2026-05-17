//! Typed payload structs paired with `EventKind` via the `Event` sum type.
//! Sprint 1 M1 subset.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::kind::EventKind;

// --- system -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaInitializedPayload {
    pub migrations_applied: u32,
    #[serde(with = "time::serde::iso8601")]
    pub schema_init_at: OffsetDateTime,
}

// --- jobs -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOpenedPayload {
    pub job_id: u64,
    pub kind: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSucceededPayload {
    pub job_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailedPayload {
    pub job_id: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelledPayload {
    pub job_id: u64,
    pub reason: String,
}

// --- tickets ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketCreatedPayload {
    pub ticket_id: u64,
    pub job_id: Option<u64>,
    pub kind: String,
    pub priority: i64,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketReadyPayload {
    pub ticket_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketLeasedPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
    pub worker_id: u64,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSucceededPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketFailedRetriablePayload {
    pub ticket_id: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub next_eligible_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketFailedTerminalPayload {
    pub ticket_id: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRequeuedAfterLeaseExpiryPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
}

// --- leases (worker-execution) ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseAcquiredPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub worker_id: u64,
    pub ttl_seconds: i64,
    #[serde(with = "time::serde::iso8601")]
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseReleasedPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub release_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseExpiredPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseForceReleasedPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub actor: String,
    pub reason: String,
    pub also_requeue: bool,
}

// --- workers ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRegisteredPayload {
    pub worker_id: u64,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCapabilityRecordedPayload {
    pub worker_id: u64,
    pub capability_id: u64,
    pub operation: String,
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

// --- artifacts -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactHandleCreatedPayload {
    pub artifact_handle_id: u64,
    pub privacy_class: String,
    pub durability_class: String,
    pub mutability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLocationRecordedPayload {
    pub artifact_location_id: u64,
    pub artifact_handle_id: u64,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLocationRetiredPayload {
    pub artifact_location_id: u64,
    pub artifact_handle_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLineageRecordedPayload {
    pub artifact_lineage_id: u64,
    pub parent_artifact_id: u64,
    pub child_artifact_id: u64,
    pub operation: String,
}

// --- sum type --------------------------------------------------------------

/// Sum type pairing each `EventKind` with its typed payload.
/// The compiler prevents writers from emitting a payload that doesn't
/// match the kind.
///
/// The `tag` column uses the dotted wire format produced by
/// `EventKind::as_str()`. Every variant carries an explicit
/// `#[serde(rename = "...")]` matching `as_str()` exactly so the
/// JSON round-trip cannot drift from what the `events.kind` column
/// stores. Do NOT use `rename_all` here — it would produce `snake_case`
/// strings (e.g. `"schema_initialized"`) that don't match the wire
/// format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Event {
    #[serde(rename = "schema.initialized")]
    SchemaInitialized(SchemaInitializedPayload),
    #[serde(rename = "job.opened")]
    JobOpened(JobOpenedPayload),
    #[serde(rename = "job.succeeded")]
    JobSucceeded(JobSucceededPayload),
    #[serde(rename = "job.failed")]
    JobFailed(JobFailedPayload),
    #[serde(rename = "job.cancelled")]
    JobCancelled(JobCancelledPayload),
    #[serde(rename = "ticket.created")]
    TicketCreated(TicketCreatedPayload),
    #[serde(rename = "ticket.ready")]
    TicketReady(TicketReadyPayload),
    #[serde(rename = "ticket.leased")]
    TicketLeased(TicketLeasedPayload),
    #[serde(rename = "ticket.succeeded")]
    TicketSucceeded(TicketSucceededPayload),
    #[serde(rename = "ticket.failed_retriable")]
    TicketFailedRetriable(TicketFailedRetriablePayload),
    #[serde(rename = "ticket.failed_terminal")]
    TicketFailedTerminal(TicketFailedTerminalPayload),
    #[serde(rename = "ticket.requeued_after_lease_expiry")]
    TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload),
    #[serde(rename = "lease.acquired")]
    LeaseAcquired(LeaseAcquiredPayload),
    #[serde(rename = "lease.released")]
    LeaseReleased(LeaseReleasedPayload),
    #[serde(rename = "lease.expired")]
    LeaseExpired(LeaseExpiredPayload),
    #[serde(rename = "lease.force_released")]
    LeaseForceReleased(LeaseForceReleasedPayload),
    #[serde(rename = "worker.registered")]
    WorkerRegistered(WorkerRegisteredPayload),
    #[serde(rename = "worker.capability_recorded")]
    WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload),
    #[serde(rename = "worker.grant_recorded")]
    WorkerGrantRecorded(WorkerGrantRecordedPayload),
    #[serde(rename = "worker.retired")]
    WorkerRetired(WorkerRetiredPayload),
    #[serde(rename = "artifact_handle.created")]
    ArtifactHandleCreated(ArtifactHandleCreatedPayload),
    #[serde(rename = "artifact_location.recorded")]
    ArtifactLocationRecorded(ArtifactLocationRecordedPayload),
    #[serde(rename = "artifact_location.retired")]
    ArtifactLocationRetired(ArtifactLocationRetiredPayload),
    #[serde(rename = "artifact_lineage.recorded")]
    ArtifactLineageRecorded(ArtifactLineageRecordedPayload),
}

impl Event {
    /// The `EventKind` that pairs with this payload. Derived by exhaustive
    /// match so a new variant is a compile error until both `EventKind` and
    /// the `as_str()` table grow to match.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::SchemaInitialized(_) => EventKind::SchemaInitialized,
            Self::JobOpened(_) => EventKind::JobOpened,
            Self::JobSucceeded(_) => EventKind::JobSucceeded,
            Self::JobFailed(_) => EventKind::JobFailed,
            Self::JobCancelled(_) => EventKind::JobCancelled,
            Self::TicketCreated(_) => EventKind::TicketCreated,
            Self::TicketReady(_) => EventKind::TicketReady,
            Self::TicketLeased(_) => EventKind::TicketLeased,
            Self::TicketSucceeded(_) => EventKind::TicketSucceeded,
            Self::TicketFailedRetriable(_) => EventKind::TicketFailedRetriable,
            Self::TicketFailedTerminal(_) => EventKind::TicketFailedTerminal,
            Self::TicketRequeuedAfterLeaseExpiry(_) => EventKind::TicketRequeuedAfterLeaseExpiry,
            Self::LeaseAcquired(_) => EventKind::LeaseAcquired,
            Self::LeaseReleased(_) => EventKind::LeaseReleased,
            Self::LeaseExpired(_) => EventKind::LeaseExpired,
            Self::LeaseForceReleased(_) => EventKind::LeaseForceReleased,
            Self::WorkerRegistered(_) => EventKind::WorkerRegistered,
            Self::WorkerCapabilityRecorded(_) => EventKind::WorkerCapabilityRecorded,
            Self::WorkerGrantRecorded(_) => EventKind::WorkerGrantRecorded,
            Self::WorkerRetired(_) => EventKind::WorkerRetired,
            Self::ArtifactHandleCreated(_) => EventKind::ArtifactHandleCreated,
            Self::ArtifactLocationRecorded(_) => EventKind::ArtifactLocationRecorded,
            Self::ArtifactLocationRetired(_) => EventKind::ArtifactLocationRetired,
            Self::ArtifactLineageRecorded(_) => EventKind::ArtifactLineageRecorded,
        }
    }
}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
