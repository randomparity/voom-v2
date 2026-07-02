//! External-system lifecycle payloads (Sprint 17, T15). Health and sync are
//! stateful facts, so this family records durable events (ADR 0001). Every
//! payload carries `#[serde(deny_unknown_fields)]` — the ADR 0013 durable
//! contract — and evolves additive-only.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A system was registered. Health always starts `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemRegisteredPayload {
    pub external_system_id: u64,
    pub kind: String,
    pub display_name: String,
    pub health_status: String,
}

/// A health probe changed the recorded status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemHealthChangedPayload {
    pub external_system_id: u64,
    pub previous: String,
    pub current: String,
}

/// A sync recorded an external→internal link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemLinkedPayload {
    pub external_system_id: u64,
    pub link_id: u64,
    pub target_type: String,
    pub target_id: u64,
    pub external_ref: String,
}

/// A sync retired a link no longer present in the external system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemUnlinkedPayload {
    pub external_system_id: u64,
    pub link_id: u64,
    pub target_type: String,
    pub target_id: u64,
    pub external_ref: String,
}

/// A read-only sync run completed. The durable source for `sync-report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemSyncedPayload {
    pub external_system_id: u64,
    pub outcome: String,
    pub links_recorded: u32,
    pub links_retired: u32,
    #[serde(with = "time::serde::iso8601")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::iso8601")]
    pub finished_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "external_system_test.rs"]
mod tests;
