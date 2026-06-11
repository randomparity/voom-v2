use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// --- M3 — asset use leases (Phase 1) -----------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseLeaseAcquiredPayload {
    pub lease_id: u64,
    /// One of: `"playback" | "scan" | "copy" | "manual_lock" | "external_lock" | "worker_operation"`.
    pub kind: String,
    /// One of: `"asset" | "bundle" | "version" | "location"`.
    pub scope_type: String,
    pub scope_id: u64,
    /// One of: `"user" | "control_plane" | "worker" | "external_system"`.
    pub issuer_kind: String,
    pub issuer_ref: String,
    /// One of: `"blocking" | "advisory"`.
    pub blocking_mode: String,
    pub ttl_bound: bool,
    #[serde(with = "time::serde::iso8601")]
    pub acquired_at: OffsetDateTime,
    #[serde(default, with = "time::serde::iso8601::option")]
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseLeaseReleasedPayload {
    pub lease_id: u64,
    /// One of: `"released" | "superseded"` (the issuer-driven release reasons).
    /// `expired`, `force_released`, and `issuer_lost` are emitted by their
    /// dedicated event variants.
    pub release_reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseLeaseExpiredPayload {
    pub lease_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseLeaseForceReleasedPayload {
    pub lease_id: u64,
    pub actor: String,
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseLeaseRecoveredStaleIssuerPayload {
    pub lease_id: u64,
    pub actor: String,
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub released_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseLeaseReanchoredByMovePayload {
    pub lease_id: u64,
    pub retired_location_id: u64,
    pub new_location_id: u64,
    #[serde(with = "time::serde::iso8601")]
    pub reanchored_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "use_leases_test.rs"]
mod tests;
