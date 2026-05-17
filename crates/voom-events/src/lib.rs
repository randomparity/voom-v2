#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "tests favor unwrap/expect over plumbing Result<()> through every assertion"
    )
)]
//! Typed event vocabulary for the VOOM control plane.
//!
//! `EventKind` is the wire-format enum; `Event` is the matching typed sum
//! over per-kind payload structs; `EventEnvelope` is the row shape the
//! `events` table stores.

pub mod envelope;
pub mod kind;
pub mod payload;
pub mod subject;

pub use envelope::{EventEnvelope, EventId, TraceId};
pub use kind::EventKind;
pub use payload::Event;
pub use subject::SubjectType;
