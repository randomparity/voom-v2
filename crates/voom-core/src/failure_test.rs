use super::*;

#[test]
fn retriable_partition_matches_spec() {
    let retriable = [
        FailureClass::WorkerTimeout,
        FailureClass::WorkerCrash,
        FailureClass::NoEligibleWorker,
        FailureClass::ArtifactUnavailable,
        FailureClass::ArtifactChecksumMismatch,
        FailureClass::ExternalSystemUnavailable,
        FailureClass::ExternalSystemRateLimited,
        FailureClass::VerificationFailure,
        FailureClass::BackupFailure,
        FailureClass::CommitFailure,
    ];
    for c in retriable {
        assert!(c.is_retriable(), "{c:?} should be retriable");
        assert_eq!(c.retry_class(), FailureRetryClass::Retriable);
    }

    let non_retriable = [
        FailureClass::PolicyParseError,
        FailureClass::PolicyValidationError,
        FailureClass::MissingCapability,
        FailureClass::MalformedWorkerResult,
        FailureClass::UserCancellation,
    ];
    for c in non_retriable {
        assert!(!c.is_retriable(), "{c:?} should not be retriable");
        assert_eq!(c.retry_class(), FailureRetryClass::NonRetriable);
    }

    let operator_required = [
        FailureClass::StaleIdentityEvidence,
        FailureClass::ClosureResolutionIncomplete,
        FailureClass::BlockedByActiveUseLease,
        FailureClass::ApprovalRequired,
        FailureClass::PriorityPolicyConflict,
    ];
    for c in operator_required {
        assert!(!c.is_retriable(), "{c:?} should not be retriable");
        assert_eq!(c.retry_class(), FailureRetryClass::OperatorRequired);
    }
}

#[test]
fn issue_severity_and_priority_derived_from_retry_class() {
    // Retriable (only reached terminally with retries exhausted).
    assert_eq!(
        FailureClass::WorkerTimeout.issue_severity(),
        IssueSeverity::Medium
    );
    assert_eq!(
        FailureClass::WorkerTimeout.issue_priority(),
        IssuePriority::Normal
    );

    // Non-retriable.
    assert_eq!(
        FailureClass::MalformedWorkerResult.issue_severity(),
        IssueSeverity::High
    );
    assert_eq!(
        FailureClass::MalformedWorkerResult.issue_priority(),
        IssuePriority::High
    );

    // Operator-required.
    assert_eq!(
        FailureClass::ApprovalRequired.issue_severity(),
        IssueSeverity::High
    );
    assert_eq!(
        FailureClass::ApprovalRequired.issue_priority(),
        IssuePriority::High
    );
}

#[test]
fn into_error_code_round_trips() {
    // Every variant maps to *some* ErrorCode; matching exhaustively
    // means a new FailureClass variant cannot silently default.
    let _ = FailureClass::WorkerTimeout.into_error_code();
    let _ = FailureClass::PriorityPolicyConflict.into_error_code();
    assert_eq!(
        FailureClass::WorkerCrash.into_error_code(),
        ErrorCode::WorkerCrash
    );
}

#[test]
fn serde_round_trips_wire_format() {
    // The on-disk + on-wire shape is snake_case.
    let s = serde_json::to_string(&FailureClass::WorkerTimeout).unwrap();
    assert_eq!(s, "\"worker_timeout\"");
    let back: FailureClass = serde_json::from_str(&s).unwrap();
    assert_eq!(back, FailureClass::WorkerTimeout);
}
