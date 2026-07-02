//! External-system use cases (Sprint 17, T15). Registration and health are
//! stateful facts, so — unlike the pure config CRUD families — these methods
//! emit durable events (ADR 0001). Path-mapping CRUD is pure operator config
//! and emits nothing. Shape: `docs/adr/0029`.

pub mod sync;
pub mod systems;
