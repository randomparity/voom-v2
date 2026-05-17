//! Repository pattern: trait per storage area, Sqlite impl per trait.

pub mod events;
pub mod jobs;
pub mod leases;
pub mod schema_meta;
pub mod tickets;
pub mod workers;

pub use events::{EventFilter, EventPage, EventRepo, EventRow, Page, SqliteEventRepo};
pub use jobs::{Job, JobRepo, JobState, NewJob, SqliteJobRepo};
pub use leases::{ExpireReport, Lease, LeaseRepo, LeaseState, NewLease, SqliteLeaseRepo, backoff};
pub use schema_meta::{SchemaMetaRepo, SqliteSchemaMetaRepo};
pub use tickets::{NewTicket, SqliteTicketRepo, Ticket, TicketRepo, TicketState};
pub use workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, Worker, WorkerKind,
    WorkerRepo, WorkerStatus,
};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
