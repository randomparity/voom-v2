//! `EventEnvelope` — the row shape stored in the `events` table.
//!
//! The envelope is never serialized as a whole — `EventRepo::append_in_tx`
//! writes each field to its own column (`kind`, `subject_type`,
//! `subject_id`, `trace_id`, `occurred_at`, and the serialized `payload`
//! `Event`). It therefore doesn't derive `Serialize`/`Deserialize`;
//! `EventKind` and `SubjectType` deliberately stay out of serde to keep
//! the dotted wire format as the single source of truth.

use time::OffsetDateTime;

use crate::{kind::EventKind, payload::Event, subject::SubjectType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId(pub u64);

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceId(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    pub kind: EventKind,
    pub occurred_at: OffsetDateTime,
    pub subject_type: SubjectType,
    pub subject_id: Option<u64>,
    pub trace_id: Option<TraceId>,
    pub payload: Event,
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
