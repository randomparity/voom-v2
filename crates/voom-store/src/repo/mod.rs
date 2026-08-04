//! Concrete `SQLite` repositories plus the few traits used as abstraction boundaries.

pub mod audit;
pub(crate) mod common;
pub mod execution;
pub mod external;
pub mod issues;
pub mod library;
pub mod media;
pub mod policy;

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
