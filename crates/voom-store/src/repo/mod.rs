//! Repository pattern: trait per storage area, Sqlite impl per trait.

pub mod events;
pub mod schema_meta;

pub use events::{EventFilter, EventPage, EventRepo, EventRow, Page, SqliteEventRepo};
pub use schema_meta::{SchemaMetaRepo, SqliteSchemaMetaRepo};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
