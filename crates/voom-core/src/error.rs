use thiserror::Error;

/// Stable wire-format identifier for an error. Consumers match on this enum
/// (exhaustively) instead of comparing against `&'static str` codes, so a
/// renamed or newly-added variant becomes a compile-time error in every
/// surface rather than a silent string-mismatch at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Database file is missing or unreachable from this host.
    DbUnreachable,
    /// Database is reachable but has no migrations applied.
    DbUninitialized,
    /// Database is reachable but its schema is partial or corrupted.
    DbPartialSchema,
    /// A previous migration left a row recorded as `success=0`; sqlx will
    /// refuse to migrate further until it is manually cleared.
    DbDirtyMigration,
    /// Database has migrations this binary does not know about.
    DbSchemaTooNew,
    /// Configuration value is invalid (e.g. malformed URL, unknown enum).
    ConfigInvalid,
    /// Resource lookup miss.
    NotFound,
    /// Unexpected internal failure with no actionable hint.
    Internal,
    /// CLI argument parsing failed (clap surface).
    BadArgs,
    /// A ticket dependency edge would create a cycle.
    DependencyCycle,
    /// Optimistic-locking conflict; caller should re-read and retry.
    Conflict,
    // --- FailureClass-derived (§12.1 / §12.5). One ErrorCode per
    // FailureClass that the CLI surface can name. -----------------
    /// Worker lease expired without a heartbeat or release.
    WorkerTimeout,
    /// Worker process crashed mid-attempt; recovered via lease expiry.
    WorkerCrash,
    /// No worker advertises the required capability/grants right now.
    NoEligibleWorker,
    /// A required artifact has no live location.
    ArtifactUnavailable,
    /// A read of an artifact disagreed with its recorded checksum.
    ArtifactChecksumMismatch,
    /// An external system the operation depends on is down/unreachable.
    ExternalSystemUnavailable,
    /// An external system rate-limited the operation.
    ExternalSystemRateLimited,
    /// A post-write verification step disagreed with the produced bytes.
    VerificationFailure,
    /// A backup write or read failed.
    BackupFailure,
    /// A commit-safety-gate phase rejected the commit.
    CommitFailure,
    /// A policy document failed to parse.
    PolicyParseError,
    /// A policy document parsed but failed validation.
    PolicyValidationError,
    /// The selected worker lacks a required capability.
    MissingCapability,
    /// A worker result deserialized but didn't satisfy the contract.
    MalformedWorkerResult,
    /// A user/operator cancelled the work in progress.
    UserCancellation,
    /// An operator approval is required before progress can continue.
    ApprovalRequired,
    /// Two priority sources disagree and policy has no precedence rule.
    PriorityPolicyConflict,
}

impl ErrorCode {
    /// Wire-format string for the JSON envelope's `error.code` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DbUnreachable => "DB_UNREACHABLE",
            Self::DbUninitialized => "DB_UNINITIALIZED",
            Self::DbPartialSchema => "DB_PARTIAL_SCHEMA",
            Self::DbDirtyMigration => "DB_DIRTY_MIGRATION",
            Self::DbSchemaTooNew => "DB_SCHEMA_TOO_NEW",
            Self::ConfigInvalid => "CONFIG_INVALID",
            Self::NotFound => "NOT_FOUND",
            Self::Internal => "INTERNAL",
            Self::BadArgs => "BAD_ARGS",
            Self::DependencyCycle => "DEPENDENCY_CYCLE",
            Self::Conflict => "CONFLICT",
            Self::WorkerTimeout => "WORKER_TIMEOUT",
            Self::WorkerCrash => "WORKER_CRASH",
            Self::NoEligibleWorker => "NO_ELIGIBLE_WORKER",
            Self::ArtifactUnavailable => "ARTIFACT_UNAVAILABLE",
            Self::ArtifactChecksumMismatch => "ARTIFACT_CHECKSUM_MISMATCH",
            Self::ExternalSystemUnavailable => "EXTERNAL_SYSTEM_UNAVAILABLE",
            Self::ExternalSystemRateLimited => "EXTERNAL_SYSTEM_RATE_LIMITED",
            Self::VerificationFailure => "VERIFICATION_FAILURE",
            Self::BackupFailure => "BACKUP_FAILURE",
            Self::CommitFailure => "COMMIT_FAILURE",
            Self::PolicyParseError => "POLICY_PARSE_ERROR",
            Self::PolicyValidationError => "POLICY_VALIDATION_ERROR",
            Self::MissingCapability => "MISSING_CAPABILITY",
            Self::MalformedWorkerResult => "MALFORMED_WORKER_RESULT",
            Self::UserCancellation => "USER_CANCELLATION",
            Self::ApprovalRequired => "APPROVAL_REQUIRED",
            Self::PriorityPolicyConflict => "PRIORITY_POLICY_CONFLICT",
        }
    }
}

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("database error: {0}")]
    Database(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("dirty migration: {0}")]
    DirtyMigration(String),
    #[error("schema is newer than this binary: {0}")]
    SchemaTooNew(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("dependency cycle: {0}")]
    DependencyCycle(String),
    #[error("conflict: {0}")]
    Conflict(String),
    // --- FailureClass-derived (§12.1 / §12.5) -------------------------
    #[error("worker timeout: {0}")]
    WorkerTimeout(String),
    #[error("worker crash: {0}")]
    WorkerCrash(String),
    #[error("no eligible worker: {0}")]
    NoEligibleWorker(String),
    #[error("artifact unavailable: {0}")]
    ArtifactUnavailable(String),
    #[error("artifact checksum mismatch: {0}")]
    ArtifactChecksumMismatch(String),
    #[error("external system unavailable: {0}")]
    ExternalSystemUnavailable(String),
    #[error("external system rate limited: {0}")]
    ExternalSystemRateLimited(String),
    #[error("verification failure: {0}")]
    VerificationFailure(String),
    #[error("backup failure: {0}")]
    BackupFailure(String),
    #[error("commit failure: {0}")]
    CommitFailure(String),
    #[error("policy parse error: {0}")]
    PolicyParseError(String),
    #[error("policy validation error: {0}")]
    PolicyValidationError(String),
    #[error("missing capability: {0}")]
    MissingCapability(String),
    #[error("malformed worker result: {0}")]
    MalformedWorkerResult(String),
    #[error("user cancellation: {0}")]
    UserCancellation(String),
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    #[error("priority policy conflict: {0}")]
    PriorityPolicyConflict(String),
}

impl VoomError {
    /// Typed wire-format code for this error. Prefer this over [`Self::code`]
    /// at every consumer that classifies on the value.
    #[must_use]
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::Database(_) => ErrorCode::DbUnreachable,
            Self::Migration(_) => ErrorCode::DbPartialSchema,
            Self::DirtyMigration(_) => ErrorCode::DbDirtyMigration,
            Self::SchemaTooNew(_) => ErrorCode::DbSchemaTooNew,
            Self::Config(_) => ErrorCode::ConfigInvalid,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Internal(_) => ErrorCode::Internal,
            Self::DependencyCycle(_) => ErrorCode::DependencyCycle,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::WorkerTimeout(_) => ErrorCode::WorkerTimeout,
            Self::WorkerCrash(_) => ErrorCode::WorkerCrash,
            Self::NoEligibleWorker(_) => ErrorCode::NoEligibleWorker,
            Self::ArtifactUnavailable(_) => ErrorCode::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch(_) => ErrorCode::ArtifactChecksumMismatch,
            Self::ExternalSystemUnavailable(_) => ErrorCode::ExternalSystemUnavailable,
            Self::ExternalSystemRateLimited(_) => ErrorCode::ExternalSystemRateLimited,
            Self::VerificationFailure(_) => ErrorCode::VerificationFailure,
            Self::BackupFailure(_) => ErrorCode::BackupFailure,
            Self::CommitFailure(_) => ErrorCode::CommitFailure,
            Self::PolicyParseError(_) => ErrorCode::PolicyParseError,
            Self::PolicyValidationError(_) => ErrorCode::PolicyValidationError,
            Self::MissingCapability(_) => ErrorCode::MissingCapability,
            Self::MalformedWorkerResult(_) => ErrorCode::MalformedWorkerResult,
            Self::UserCancellation(_) => ErrorCode::UserCancellation,
            Self::ApprovalRequired(_) => ErrorCode::ApprovalRequired,
            Self::PriorityPolicyConflict(_) => ErrorCode::PriorityPolicyConflict,
        }
    }

    /// Stable string code matching the JSON envelope's `error.code`. Thin
    /// wrapper around [`Self::error_code`] kept for the envelope writers that
    /// take `&'static str` (`voom_cli::envelope::emit_err`).
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.error_code().as_str()
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
