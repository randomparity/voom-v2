//! Repository pattern: trait per storage area, Sqlite impl per trait.

pub mod schema_meta;

pub use schema_meta::{SchemaMetaRepo, SqliteSchemaMetaRepo};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
