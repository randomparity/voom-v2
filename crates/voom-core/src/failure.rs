//! `FailureClass` — single source of truth for retriability decisions
//! across `LeaseRepo::fail`, `LeaseRepo::expire_due`, and the
//! `ticket.failed_*` event payloads. Mirrors the architectural spec's
//! Failure taxonomy (Error Handling And Recovery → Failure taxonomy)
//! exactly so the `retry_class`/`is_retriable` partition cannot drift
//! from the spec without a compile error somewhere downstream.

use crate::error::ErrorCode;
use crate::issue::{IssuePriority, IssueSeverity};

/// Twenty failure categories defined by the architectural spec's
/// Failure taxonomy: ten retriable, five non-retriable, five
/// operator-required. The retriability partition is enforced by
/// `retry_class` below; any new variant requires extending all five
/// methods or the compiler flags the missing arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    // Retriable — a fresh attempt against the same input could
    // plausibly succeed without operator intervention.
    WorkerTimeout,
    WorkerCrash,
    NoEligibleWorker,
    ArtifactUnavailable,
    ArtifactChecksumMismatch,
    ExternalSystemUnavailable,
    ExternalSystemRateLimited,
    VerificationFailure,
    BackupFailure,
    CommitFailure,
    // Non-retriable — the input itself is wrong; retrying without a
    // change cannot succeed.
    PolicyParseError,
    PolicyValidationError,
    MissingCapability,
    MalformedWorkerResult,
    UserCancellation,
    // Operator-required — execution cannot proceed until an operator
    // takes some action (re-evaluate evidence, resolve a closure,
    // approve a privileged step, etc.).
    StaleIdentityEvidence,
    ClosureResolutionIncomplete,
    BlockedByActiveUseLease,
    ApprovalRequired,
    PriorityPolicyConflict,
}

/// Coarse-grained retriability class. Used by the terminal-failure
/// auto-open path (§10.2) to derive issue priority and severity, and
/// by the lease-fail path to decide whether to requeue vs. transition
/// to terminal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureRetryClass {
    Retriable,
    NonRetriable,
    OperatorRequired,
}

impl FailureClass {
    /// Coarse-grained retry class — the single source of truth for the
    /// retriability partition. All other classifier methods derive from
    /// this match.
    #[must_use]
    pub const fn retry_class(self) -> FailureRetryClass {
        match self {
            Self::WorkerTimeout
            | Self::WorkerCrash
            | Self::NoEligibleWorker
            | Self::ArtifactUnavailable
            | Self::ArtifactChecksumMismatch
            | Self::ExternalSystemUnavailable
            | Self::ExternalSystemRateLimited
            | Self::VerificationFailure
            | Self::BackupFailure
            | Self::CommitFailure => FailureRetryClass::Retriable,
            Self::PolicyParseError
            | Self::PolicyValidationError
            | Self::MissingCapability
            | Self::MalformedWorkerResult
            | Self::UserCancellation => FailureRetryClass::NonRetriable,
            Self::StaleIdentityEvidence
            | Self::ClosureResolutionIncomplete
            | Self::BlockedByActiveUseLease
            | Self::ApprovalRequired
            | Self::PriorityPolicyConflict => FailureRetryClass::OperatorRequired,
        }
    }

    /// `true` iff a fresh attempt against the same input could plausibly
    /// succeed without operator intervention or upstream change.
    #[must_use]
    pub const fn is_retriable(self) -> bool {
        matches!(self.retry_class(), FailureRetryClass::Retriable)
    }

    /// Severity to stamp on the `terminal_failure` issue opened by the
    /// auto-open path (§10.2 / S3). `OperatorRequired` and
    /// `NonRetriable` default to `High`; `Retriable` (always reached
    /// only with retries exhausted) defaults to `Medium`.
    #[must_use]
    pub const fn issue_severity(self) -> IssueSeverity {
        match self.retry_class() {
            FailureRetryClass::OperatorRequired | FailureRetryClass::NonRetriable => {
                IssueSeverity::High
            }
            FailureRetryClass::Retriable => IssueSeverity::Medium,
        }
    }

    /// Priority to stamp on the `terminal_failure` issue opened by the
    /// auto-open path (§10.2 / S3). `OperatorRequired` and
    /// `NonRetriable` default to `High`; `Retriable` (retries
    /// exhausted) defaults to `Normal`.
    #[must_use]
    pub const fn issue_priority(self) -> IssuePriority {
        match self.retry_class() {
            FailureRetryClass::OperatorRequired | FailureRetryClass::NonRetriable => {
                IssuePriority::High
            }
            FailureRetryClass::Retriable => IssuePriority::Normal,
        }
    }

    /// Maps to the `ErrorCode` the CLI envelope surfaces on a
    /// `ticket.failed_terminal` path (§12.1). One variant per class
    /// preserves the round-trip from failure source → wire string.
    #[must_use]
    pub const fn into_error_code(self) -> ErrorCode {
        match self {
            Self::WorkerTimeout => ErrorCode::WorkerTimeout,
            Self::WorkerCrash => ErrorCode::WorkerCrash,
            Self::NoEligibleWorker => ErrorCode::NoEligibleWorker,
            Self::ArtifactUnavailable => ErrorCode::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch => ErrorCode::ArtifactChecksumMismatch,
            Self::ExternalSystemUnavailable => ErrorCode::ExternalSystemUnavailable,
            Self::ExternalSystemRateLimited => ErrorCode::ExternalSystemRateLimited,
            Self::VerificationFailure => ErrorCode::VerificationFailure,
            Self::BackupFailure => ErrorCode::BackupFailure,
            Self::CommitFailure => ErrorCode::CommitFailure,
            Self::PolicyParseError => ErrorCode::PolicyParseError,
            Self::PolicyValidationError => ErrorCode::PolicyValidationError,
            Self::MissingCapability => ErrorCode::MissingCapability,
            Self::MalformedWorkerResult => ErrorCode::MalformedWorkerResult,
            Self::UserCancellation => ErrorCode::UserCancellation,
            // Operator-required classes each carry their own ErrorCode.
            // Names diverge where the FailureClass predates the code:
            // `BlockedByActiveUseLease` maps to `ErrorCode::BlockedByUseLease`.
            Self::StaleIdentityEvidence => ErrorCode::StaleIdentityEvidence,
            Self::ClosureResolutionIncomplete => ErrorCode::ClosureResolutionIncomplete,
            Self::BlockedByActiveUseLease => ErrorCode::BlockedByUseLease,
            Self::ApprovalRequired => ErrorCode::ApprovalRequired,
            Self::PriorityPolicyConflict => ErrorCode::PriorityPolicyConflict,
        }
    }
}

#[cfg(test)]
#[path = "failure_test.rs"]
mod tests;
