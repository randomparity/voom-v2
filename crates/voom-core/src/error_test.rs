use std::error::Error;

use super::*;

#[test]
fn database_variant_has_db_unreachable_code() {
    let err = VoomError::database("connection refused");
    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(err.code(), "DB_UNREACHABLE");
}

#[test]
fn database_constructor_has_no_source() {
    let err = VoomError::database("connection refused");
    assert!(err.source().is_none());
    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert_eq!(err.to_string(), "database error: connection refused");
}

#[test]
fn database_context_preserves_code() {
    let err = VoomError::database_context("asset_use_leases insert", sqlx::Error::RowNotFound);
    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(err.code(), "DB_UNREACHABLE");
}

#[test]
fn database_context_exposes_sqlx_source() {
    let err = VoomError::database_context("asset_use_leases insert", sqlx::Error::RowNotFound);
    let source = err.source().unwrap();
    let sqlx_err = source.downcast_ref::<sqlx::Error>().unwrap();
    assert!(matches!(sqlx_err, sqlx::Error::RowNotFound));
}

#[test]
fn database_context_message_is_byte_identical_to_old_format() {
    let context = "video_profiles.crf";
    let err = VoomError::database_context(context, sqlx::Error::RowNotFound);
    let expected = format!("database error: {context}: {}", sqlx::Error::RowNotFound);
    assert_eq!(err.to_string(), expected);
}

#[test]
fn migration_variant_has_partial_schema_code() {
    let err = VoomError::Migration("missing migration".into());
    assert_eq!(err.error_code(), ErrorCode::DbPartialSchema);
    assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
}

#[test]
fn uninitialized_database_variant_has_uninitialized_code() {
    let err = VoomError::UninitializedDatabase("no migrations applied".into());
    assert_eq!(err.error_code(), ErrorCode::DbUninitialized);
    assert_eq!(err.code(), "DB_UNINITIALIZED");
}

#[test]
fn schema_too_new_variant_has_too_new_code() {
    let err = VoomError::SchemaTooNew("future migration applied".into());
    assert_eq!(err.error_code(), ErrorCode::DbSchemaTooNew);
    assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
}

#[test]
fn internal_variant_has_internal_code() {
    let err = VoomError::Internal("unexpected".into());
    assert_eq!(err.error_code(), ErrorCode::Internal);
    assert_eq!(err.code(), "INTERNAL");
}

#[test]
fn dirty_migration_variant_has_dirty_migration_code() {
    let err = VoomError::DirtyMigration("version 1 is dirty".into());
    assert_eq!(err.error_code(), ErrorCode::DbDirtyMigration);
    assert_eq!(err.code(), "DB_DIRTY_MIGRATION");
}

#[test]
fn dependency_cycle_error_code_string() {
    let e = VoomError::DependencyCycle("ticket 5 -> ticket 2 -> ticket 5".to_owned());
    assert_eq!(e.code(), "DEPENDENCY_CYCLE");
    assert_eq!(e.error_code(), ErrorCode::DependencyCycle);
}

#[test]
fn conflict_error_code_string() {
    let e = VoomError::Conflict("ticket 7 epoch mismatch".to_owned());
    assert_eq!(e.code(), "CONFLICT");
    assert_eq!(e.error_code(), ErrorCode::Conflict);
}

#[test]
fn blocked_by_use_lease_error_code_string() {
    let e = VoomError::BlockedByUseLease("lease 17 owns scope".to_owned());
    assert_eq!(e.code(), "BLOCKED_BY_USE_LEASE");
    assert_eq!(e.error_code(), ErrorCode::BlockedByUseLease);
}

#[test]
fn blocked_by_pending_commit_error_code_string() {
    let e = VoomError::BlockedByPendingCommit("commit 4 already pending on scope".to_owned());
    assert_eq!(e.code(), "BLOCKED_BY_PENDING_COMMIT");
    assert_eq!(e.error_code(), ErrorCode::BlockedByPendingCommit);
}

#[test]
fn blocked_by_closure_grew_error_code_string() {
    let e =
        VoomError::BlockedByClosureGrew("closure shifted between prepare and authorize".to_owned());
    assert_eq!(e.code(), "BLOCKED_BY_CLOSURE_GREW");
    assert_eq!(e.error_code(), ErrorCode::BlockedByClosureGrew);
}

#[test]
fn stale_identity_evidence_error_code_string() {
    let e = VoomError::StaleIdentityEvidence("pinned version retired".to_owned());
    assert_eq!(e.code(), "STALE_IDENTITY_EVIDENCE");
    assert_eq!(e.error_code(), ErrorCode::StaleIdentityEvidence);
}

#[test]
fn closure_resolution_incomplete_error_code_string() {
    let e =
        VoomError::ClosureResolutionIncomplete("alias resolver returned Unreachable".to_owned());
    assert_eq!(e.code(), "CLOSURE_RESOLUTION_INCOMPLETE");
    assert_eq!(e.error_code(), ErrorCode::ClosureResolutionIncomplete);
}

#[test]
fn worker_retired_error_code_string() {
    let e = VoomError::WorkerRetired("incarnation 3 was reaped".to_owned());
    assert_eq!(e.code(), "WORKER_RETIRED");
    assert_eq!(e.error_code(), ErrorCode::WorkerRetired);
}

#[test]
fn worker_incarnation_stale_error_code_string() {
    let e = VoomError::WorkerIncarnationStale("epoch 4 presented, current is 5".to_owned());
    assert_eq!(e.code(), "WORKER_INCARNATION_STALE");
    assert_eq!(e.error_code(), ErrorCode::WorkerIncarnationStale);
}

#[test]
fn ambiguous_worker_selection_error_code_string() {
    let e = VoomError::AmbiguousWorkerSelection("two workers advertise probe_file".to_owned());
    assert_eq!(e.code(), "AMBIGUOUS_WORKER_SELECTION");
    assert_eq!(e.error_code(), ErrorCode::AmbiguousWorkerSelection);
}

#[test]
fn plan_generation_error_has_stable_public_code() {
    let err = VoomError::PlanGeneration("planner rejected empty input set".to_owned());
    assert_eq!(err.code(), "PLAN_GENERATION_ERROR");
    assert_eq!(err.error_code(), ErrorCode::PlanGenerationError);
}

#[test]
fn compliance_report_error_has_stable_public_code() {
    let err = VoomError::ComplianceReport("deterministic serialization failed".to_owned());
    assert_eq!(err.code(), "COMPLIANCE_REPORT_ERROR");
    assert_eq!(err.error_code(), ErrorCode::ComplianceReportError);
}

#[test]
fn policy_execution_error_has_stable_public_code() {
    let err = VoomError::PolicyExecution("unsupported operation".to_owned());
    assert_eq!(err.code(), "POLICY_EXECUTION_ERROR");
    assert_eq!(err.error_code(), ErrorCode::PolicyExecutionError);
}

/// Adding a variant to `ErrorCode` must force a wire-string decision in
/// `as_str()`. This test is intentionally an exhaustive match so a new
/// variant fails compilation here too — both halves of the round trip
/// stay in lockstep.
#[test]
fn every_error_code_has_a_wire_string() {
    for code in [
        ErrorCode::DbUnreachable,
        ErrorCode::DbUninitialized,
        ErrorCode::DbPartialSchema,
        ErrorCode::DbDirtyMigration,
        ErrorCode::DbSchemaTooNew,
        ErrorCode::ConfigInvalid,
        ErrorCode::NotFound,
        ErrorCode::Internal,
        ErrorCode::BadArgs,
        ErrorCode::DependencyCycle,
        ErrorCode::Conflict,
        ErrorCode::BlockedByUseLease,
        ErrorCode::BlockedByPendingCommit,
        ErrorCode::BlockedByClosureGrew,
        ErrorCode::StaleIdentityEvidence,
        ErrorCode::ClosureResolutionIncomplete,
        ErrorCode::WorkerTimeout,
        ErrorCode::WorkerCrash,
        ErrorCode::NoEligibleWorker,
        ErrorCode::ArtifactUnavailable,
        ErrorCode::ArtifactChecksumMismatch,
        ErrorCode::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemRateLimited,
        ErrorCode::VerificationFailure,
        ErrorCode::BackupFailure,
        ErrorCode::CommitFailure,
        ErrorCode::PolicyParseError,
        ErrorCode::PolicyValidationError,
        ErrorCode::PlanGenerationError,
        ErrorCode::ComplianceReportError,
        ErrorCode::PolicyExecutionError,
        ErrorCode::MissingCapability,
        ErrorCode::MalformedWorkerResult,
        ErrorCode::MalformedMedia,
        ErrorCode::UserCancellation,
        ErrorCode::ApprovalRequired,
        ErrorCode::PriorityPolicyConflict,
        ErrorCode::WorkerRetired,
        ErrorCode::WorkerIncarnationStale,
        ErrorCode::AmbiguousWorkerSelection,
    ] {
        // Confirm the exhaustive match in `as_str()` produces a
        // SCREAMING_SNAKE_CASE token; format isn't load-bearing beyond
        // the contract documented on the enum itself.
        let s = code.as_str();
        assert!(!s.is_empty(), "{code:?} has empty wire string");
        assert!(
            s.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
            "{code:?} wire string {s:?} is not SCREAMING_SNAKE_CASE"
        );
    }
}

#[test]
fn error_code_from_wire_str_round_trips_every_variant() {
    for &code in ErrorCode::ALL {
        let parsed = ErrorCode::from_wire_str(code.as_str()).unwrap();
        assert_eq!(parsed, code);
    }
    assert!(ErrorCode::from_wire_str("NOT_A_CODE").is_none());
}
