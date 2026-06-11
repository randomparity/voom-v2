use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::{FailureClass, IssueId, TicketOperation};

// --- jobs -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobOpenedPayload {
    pub job_id: u64,
    pub kind: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSucceededPayload {
    pub job_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobFailedPayload {
    pub job_id: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobCancelledPayload {
    pub job_id: u64,
    pub reason: String,
}

// --- tickets ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketCreatedPayload {
    pub ticket_id: u64,
    pub job_id: Option<u64>,
    pub kind: TicketOperation,
    pub priority: i64,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketReadyPayload {
    pub ticket_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketLeasedPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
    pub worker_id: u64,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketSucceededPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketFailedRetriablePayload {
    pub ticket_id: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
    /// Failure category that drove the retriability decision. Audit
    /// reads this back through `EventKind::TicketFailedRetriable` to
    /// reconstruct the decision without re-deriving it from `reason`.
    pub class: FailureClass,
    #[serde(with = "time::serde::iso8601")]
    pub next_eligible_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketFailedTerminalPayload {
    pub ticket_id: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
    /// Failure category. M3's auto-open path (§10.2 / S3) reads it back
    /// to populate `issues.severity` / `issues.priority`.
    pub class: FailureClass,
    /// `terminal_failure` issue auto-opened by the §10.2 / S3 path.
    /// `None` in M1 (the `issues` table doesn't exist yet) — `Some(id)`
    /// in M3 once `SqliteIssueRepo` lands. Always serialized (`null` in M1)
    /// so the wire shape stays stable across the M3 migration.
    pub issue_id: Option<IssueId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketRequeuedAfterLeaseExpiryPayload {
    pub ticket_id: u64,
    pub lease_id: u64,
}

/// Emitted alongside `lease.force_released` when the operator asked
/// for `also_requeue = true` and the ticket still had attempts
/// remaining. Carries the operator `actor` / `reason` for audit
/// continuity even though `lease.force_released` also records them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketRequeuedAfterForceReleasePayload {
    pub ticket_id: u64,
    pub lease_id: u64,
    pub actor: String,
    pub reason: String,
}

// --- leases (worker-execution) ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseAcquiredPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub worker_id: u64,
    pub ttl_seconds: i64,
    #[serde(with = "time::serde::iso8601")]
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseReleasedPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub release_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseExpiredPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseForceReleasedPayload {
    pub lease_id: u64,
    pub ticket_id: u64,
    pub actor: String,
    pub reason: String,
    pub also_requeue: bool,
}

#[cfg(test)]
#[path = "execution_test.rs"]
mod tests;
