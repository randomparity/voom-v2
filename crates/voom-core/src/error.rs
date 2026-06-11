use thiserror::Error;

/// Stable wire-format identifier for an error. Consumers match on this enum
/// (exhaustively) instead of comparing against `&'static str` codes, so a
/// renamed or newly-added variant becomes a compile-time error in every
/// surface rather than a silent string-mismatch at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
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
    // --- Commit safety gate (M3 Phase 2 / §9.3) -------------------
    /// A blocking use-lease overlaps the commit's affected scope.
    BlockedByUseLease,
    /// Another in-flight commit-intent owns one of the scopes in the
    /// affected closure; the caller must wait or take it over.
    BlockedByPendingCommit,
    /// Between `prepare` and `authorize` (or between `authorize` and
    /// `finalize`'s defensive trip-wire), the affected-scope closure
    /// changed — typically because an alias resolver discovered a new
    /// location or an external rename reconciliation retired one
    /// location and recorded another.
    BlockedByClosureGrew,
    /// One or more accepted-evidence pins (file-version IDs, hashes,
    /// locations) no longer match current state; the commit cannot
    /// proceed without operator re-evaluation.
    StaleIdentityEvidence,
    /// The alias resolver could not enumerate the full closure for the
    /// affected `FileVersion`(s); the commit cannot proceed without
    /// operator action (or a sanctioned `closure_incomplete` bypass).
    ClosureResolutionIncomplete,
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
    /// A compiled policy and policy input set could not be converted into an
    /// execution-plan projection.
    PlanGenerationError,
    /// A compliance report could not be generated or serialized deterministically.
    ComplianceReportError,
    /// Policy-derived planned work could not be bridged into executable workflow work.
    PolicyExecutionError,
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
    // --- Sprint 2 Phase 1: worker protocol layer ----------------------
    /// A retired worker incarnation attempted to call the supervisor;
    /// the call was refused without mutating any lease.
    WorkerRetired,
    /// A worker callback's presented incarnation does not match the
    /// live `worker_incarnations` row (epoch / secret hash mismatch
    /// outside the standard auth path).
    WorkerIncarnationStale,
    /// More than one active worker advertises the requested
    /// `OperationKind` and no explicit override is set.
    AmbiguousWorkerSelection,
}

impl ErrorCode {
    /// Stable inventory for parser and test coverage. Keep this in the same
    /// order as the enum so public code additions are easy to review.
    pub const ALL: &'static [Self] = &[
        Self::DbUnreachable,
        Self::DbUninitialized,
        Self::DbPartialSchema,
        Self::DbDirtyMigration,
        Self::DbSchemaTooNew,
        Self::ConfigInvalid,
        Self::NotFound,
        Self::Internal,
        Self::BadArgs,
        Self::DependencyCycle,
        Self::Conflict,
        Self::BlockedByUseLease,
        Self::BlockedByPendingCommit,
        Self::BlockedByClosureGrew,
        Self::StaleIdentityEvidence,
        Self::ClosureResolutionIncomplete,
        Self::WorkerTimeout,
        Self::WorkerCrash,
        Self::NoEligibleWorker,
        Self::ArtifactUnavailable,
        Self::ArtifactChecksumMismatch,
        Self::ExternalSystemUnavailable,
        Self::ExternalSystemRateLimited,
        Self::VerificationFailure,
        Self::BackupFailure,
        Self::CommitFailure,
        Self::PolicyParseError,
        Self::PolicyValidationError,
        Self::PlanGenerationError,
        Self::ComplianceReportError,
        Self::PolicyExecutionError,
        Self::MissingCapability,
        Self::MalformedWorkerResult,
        Self::UserCancellation,
        Self::ApprovalRequired,
        Self::PriorityPolicyConflict,
        Self::WorkerRetired,
        Self::WorkerIncarnationStale,
        Self::AmbiguousWorkerSelection,
    ];

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
            Self::BlockedByUseLease => "BLOCKED_BY_USE_LEASE",
            Self::BlockedByPendingCommit => "BLOCKED_BY_PENDING_COMMIT",
            Self::BlockedByClosureGrew => "BLOCKED_BY_CLOSURE_GREW",
            Self::StaleIdentityEvidence => "STALE_IDENTITY_EVIDENCE",
            Self::ClosureResolutionIncomplete => "CLOSURE_RESOLUTION_INCOMPLETE",
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
            Self::PlanGenerationError => "PLAN_GENERATION_ERROR",
            Self::ComplianceReportError => "COMPLIANCE_REPORT_ERROR",
            Self::PolicyExecutionError => "POLICY_EXECUTION_ERROR",
            Self::MissingCapability => "MISSING_CAPABILITY",
            Self::MalformedWorkerResult => "MALFORMED_WORKER_RESULT",
            Self::UserCancellation => "USER_CANCELLATION",
            Self::ApprovalRequired => "APPROVAL_REQUIRED",
            Self::PriorityPolicyConflict => "PRIORITY_POLICY_CONFLICT",
            Self::WorkerRetired => "WORKER_RETIRED",
            Self::WorkerIncarnationStale => "WORKER_INCARNATION_STALE",
            Self::AmbiguousWorkerSelection => "AMBIGUOUS_WORKER_SELECTION",
        }
    }

    /// Parse a public wire-format error code.
    ///
    /// Returns `None` for unknown values so callers can decide whether the
    /// source is user input, persisted data, or an internal invariant breach.
    #[must_use]
    pub fn from_wire_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// Boxed, type-erased source error. Used to preserve an underlying error's
/// source chain without forcing a concrete database-driver dependency into
/// `voom-core` (the bottom of the one-way layer graph). Consumers recover the
/// concrete type via `downcast_ref` — e.g. `downcast_ref::<sqlx::Error>()`.
type BoxSource = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("database error: {message}")]
    Database {
        message: String,
        #[source]
        source: Option<BoxSource>,
    },
    #[error("uninitialized database: {0}")]
    UninitializedDatabase(String),
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
    // --- Commit safety gate (M3 Phase 2 / §9.3) -----------------------
    #[error("blocked by use lease: {0}")]
    BlockedByUseLease(String),
    #[error("blocked by pending commit: {0}")]
    BlockedByPendingCommit(String),
    #[error("blocked by closure grew: {0}")]
    BlockedByClosureGrew(String),
    #[error("stale identity evidence: {0}")]
    StaleIdentityEvidence(String),
    #[error("closure resolution incomplete: {0}")]
    ClosureResolutionIncomplete(String),
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
    #[error("plan generation error: {0}")]
    PlanGeneration(String),
    #[error("compliance report error: {0}")]
    ComplianceReport(String),
    #[error("policy execution error: {0}")]
    PolicyExecution(String),
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
    #[error("worker retired: {0}")]
    WorkerRetired(String),
    #[error("worker incarnation stale: {0}")]
    WorkerIncarnationStale(String),
    #[error("ambiguous worker selection: {0}")]
    AmbiguousWorkerSelection(String),
}

impl VoomError {
    /// Database error with a human-readable message and no structured source.
    ///
    /// Use this for database-layer failures that do not originate from a
    /// driver error (URL/parse failures, integer overflow, decode misses,
    /// literal sentinels). For errors that wrap a `sqlx::Error`, prefer
    /// [`Self::database_context`] so the source chain is preserved.
    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
            source: None,
        }
    }

    /// Database error that preserves an underlying error's source chain.
    ///
    /// `context` is the prefix describing the failing operation; the composed
    /// `Display` message is `"database error: {context}: {source}"`. The source
    /// is type-erased into a boxed `dyn Error` so `voom-core` needs no database
    /// driver dependency; recover the concrete error via
    /// `err.source().and_then(|s| s.downcast_ref::<sqlx::Error>())`.
    pub fn database_context(context: impl std::fmt::Display, source: impl Into<BoxSource>) -> Self {
        let source = source.into();
        Self::Database {
            message: format!("{context}: {source}"),
            source: Some(source),
        }
    }

    /// Typed wire-format code for this error. Prefer this over [`Self::code`]
    /// at every consumer that classifies on the value.
    #[must_use]
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::Database { .. } => ErrorCode::DbUnreachable,
            Self::UninitializedDatabase(_) => ErrorCode::DbUninitialized,
            Self::Migration(_) => ErrorCode::DbPartialSchema,
            Self::DirtyMigration(_) => ErrorCode::DbDirtyMigration,
            Self::SchemaTooNew(_) => ErrorCode::DbSchemaTooNew,
            Self::Config(_) => ErrorCode::ConfigInvalid,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Internal(_) => ErrorCode::Internal,
            Self::DependencyCycle(_) => ErrorCode::DependencyCycle,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::BlockedByUseLease(_) => ErrorCode::BlockedByUseLease,
            Self::BlockedByPendingCommit(_) => ErrorCode::BlockedByPendingCommit,
            Self::BlockedByClosureGrew(_) => ErrorCode::BlockedByClosureGrew,
            Self::StaleIdentityEvidence(_) => ErrorCode::StaleIdentityEvidence,
            Self::ClosureResolutionIncomplete(_) => ErrorCode::ClosureResolutionIncomplete,
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
            Self::PlanGeneration(_) => ErrorCode::PlanGenerationError,
            Self::ComplianceReport(_) => ErrorCode::ComplianceReportError,
            Self::PolicyExecution(_) => ErrorCode::PolicyExecutionError,
            Self::MissingCapability(_) => ErrorCode::MissingCapability,
            Self::MalformedWorkerResult(_) => ErrorCode::MalformedWorkerResult,
            Self::UserCancellation(_) => ErrorCode::UserCancellation,
            Self::ApprovalRequired(_) => ErrorCode::ApprovalRequired,
            Self::PriorityPolicyConflict(_) => ErrorCode::PriorityPolicyConflict,
            Self::WorkerRetired(_) => ErrorCode::WorkerRetired,
            Self::WorkerIncarnationStale(_) => ErrorCode::WorkerIncarnationStale,
            Self::AmbiguousWorkerSelection(_) => ErrorCode::AmbiguousWorkerSelection,
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
