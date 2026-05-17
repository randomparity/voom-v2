//! `EventKind` — wire-format event-type identifier. Sprint 1 M1 subset.
//!
//! The enum deliberately does NOT derive `Serialize`/`Deserialize`. The
//! on-disk and in-memory wire format is the dotted string returned by
//! `as_str()` (e.g. `"schema.initialized"`); deriving `serde` with
//! `rename_all = "snake_case"` would produce a divergent string
//! (`"schema_initialized"`) and silently break the round-trip with the
//! `events.kind` column. Use `EventKind::from_str` / `TryFrom<&str>`
//! to decode; `Event` (in `payload.rs`) is the serde-tagged sum type
//! that uses these strings explicitly per variant.

use voom_core::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    SchemaInitialized,
    JobOpened,
    JobSucceeded,
    JobFailed,
    JobCancelled,
    TicketCreated,
    TicketReady,
    TicketLeased,
    TicketSucceeded,
    TicketFailedRetriable,
    TicketFailedTerminal,
    TicketRequeuedAfterLeaseExpiry,
    LeaseAcquired,
    LeaseReleased,
    LeaseExpired,
    LeaseForceReleased,
    WorkerRegistered,
    WorkerCapabilityRecorded,
    WorkerGrantRecorded,
    WorkerRetired,
    ArtifactHandleCreated,
    ArtifactLocationRecorded,
    ArtifactLocationRetired,
    ArtifactLineageRecorded,
}

impl EventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SchemaInitialized => "schema.initialized",
            Self::JobOpened => "job.opened",
            Self::JobSucceeded => "job.succeeded",
            Self::JobFailed => "job.failed",
            Self::JobCancelled => "job.cancelled",
            Self::TicketCreated => "ticket.created",
            Self::TicketReady => "ticket.ready",
            Self::TicketLeased => "ticket.leased",
            Self::TicketSucceeded => "ticket.succeeded",
            Self::TicketFailedRetriable => "ticket.failed_retriable",
            Self::TicketFailedTerminal => "ticket.failed_terminal",
            Self::TicketRequeuedAfterLeaseExpiry => "ticket.requeued_after_lease_expiry",
            Self::LeaseAcquired => "lease.acquired",
            Self::LeaseReleased => "lease.released",
            Self::LeaseExpired => "lease.expired",
            Self::LeaseForceReleased => "lease.force_released",
            Self::WorkerRegistered => "worker.registered",
            Self::WorkerCapabilityRecorded => "worker.capability_recorded",
            Self::WorkerGrantRecorded => "worker.grant_recorded",
            Self::WorkerRetired => "worker.retired",
            Self::ArtifactHandleCreated => "artifact_handle.created",
            Self::ArtifactLocationRecorded => "artifact_location.recorded",
            Self::ArtifactLocationRetired => "artifact_location.retired",
            Self::ArtifactLineageRecorded => "artifact_lineage.recorded",
        }
    }

    /// Parse the on-disk wire-format string into an `EventKind`. Mirrors
    /// `as_str()` exactly — every variant must round-trip.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the string is not one of the
    /// known dotted wire-format values.
    #[expect(
        clippy::should_implement_trait,
        reason = "explicit inherent fn keeps the dotted wire format the single source of truth; \
                  std::str::FromStr would mask the dedicated VoomError-bearing API"
    )]
    pub fn from_str(s: &str) -> Result<Self, VoomError> {
        Ok(match s {
            "schema.initialized" => Self::SchemaInitialized,
            "job.opened" => Self::JobOpened,
            "job.succeeded" => Self::JobSucceeded,
            "job.failed" => Self::JobFailed,
            "job.cancelled" => Self::JobCancelled,
            "ticket.created" => Self::TicketCreated,
            "ticket.ready" => Self::TicketReady,
            "ticket.leased" => Self::TicketLeased,
            "ticket.succeeded" => Self::TicketSucceeded,
            "ticket.failed_retriable" => Self::TicketFailedRetriable,
            "ticket.failed_terminal" => Self::TicketFailedTerminal,
            "ticket.requeued_after_lease_expiry" => Self::TicketRequeuedAfterLeaseExpiry,
            "lease.acquired" => Self::LeaseAcquired,
            "lease.released" => Self::LeaseReleased,
            "lease.expired" => Self::LeaseExpired,
            "lease.force_released" => Self::LeaseForceReleased,
            "worker.registered" => Self::WorkerRegistered,
            "worker.capability_recorded" => Self::WorkerCapabilityRecorded,
            "worker.grant_recorded" => Self::WorkerGrantRecorded,
            "worker.retired" => Self::WorkerRetired,
            "artifact_handle.created" => Self::ArtifactHandleCreated,
            "artifact_location.recorded" => Self::ArtifactLocationRecorded,
            "artifact_location.retired" => Self::ArtifactLocationRetired,
            "artifact_lineage.recorded" => Self::ArtifactLineageRecorded,
            other => {
                return Err(VoomError::Database(format!(
                    "events.kind {other:?} not in EventKind vocab"
                )));
            }
        })
    }
}

impl TryFrom<&str> for EventKind {
    type Error = VoomError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::from_str(s)
    }
}

#[cfg(test)]
#[path = "kind_test.rs"]
mod tests;
