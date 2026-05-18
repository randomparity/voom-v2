//! Repository pattern: trait per storage area, Sqlite impl per trait.

pub mod artifacts;
pub mod bundles;
pub(crate) mod common;
pub mod events;
pub mod identity;
pub mod jobs;
pub mod leases;
pub mod schema_meta;
pub mod tickets;
pub mod workers;

pub use artifacts::{
    ArtifactHandle, ArtifactLineage, ArtifactLocation, ArtifactRepo, NewArtifactHandle,
    NewArtifactLineage, NewArtifactLocation, SqliteArtifactRepo,
};
pub use events::{EventFilter, EventPage, EventRepo, EventRow, Page, SqliteEventRepo};
pub use jobs::{Job, JobRepo, JobState, NewJob, SqliteJobRepo};
pub use leases::{
    ExpireReport, ForceReleaseOutcome, Lease, LeaseRepo, LeaseState, NewLease, ReleaseReason,
    SqliteLeaseRepo,
};
pub use schema_meta::{SchemaMetaRepo, SqliteSchemaMetaRepo};
pub use tickets::{NewTicket, SqliteTicketRepo, Ticket, TicketRepo, TicketState};
pub use workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, Worker, WorkerKind,
    WorkerRepo, WorkerStatus,
};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
