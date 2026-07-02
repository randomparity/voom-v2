use super::*;

#[test]
fn all_contains_every_failure_class_once() {
    use std::collections::HashSet;

    let all = FailureClass::ALL;
    assert_eq!(all.len(), 23);
    let unique = all.iter().copied().collect::<HashSet<_>>();
    assert_eq!(unique.len(), all.len());
    assert!(unique.contains(&FailureClass::WorkerTimeout));
    assert!(unique.contains(&FailureClass::WorkerCrash));
    assert!(unique.contains(&FailureClass::NoEligibleWorker));
    assert!(unique.contains(&FailureClass::ArtifactUnavailable));
    assert!(unique.contains(&FailureClass::ArtifactChecksumMismatch));
    assert!(unique.contains(&FailureClass::ExternalSystemUnavailable));
    assert!(unique.contains(&FailureClass::ExternalSystemRateLimited));
    assert!(unique.contains(&FailureClass::VerificationFailure));
    assert!(unique.contains(&FailureClass::BackupFailure));
    assert!(unique.contains(&FailureClass::CommitFailure));
    assert!(unique.contains(&FailureClass::PolicyParseError));
    assert!(unique.contains(&FailureClass::PolicyValidationError));
    assert!(unique.contains(&FailureClass::MissingCapability));
    assert!(unique.contains(&FailureClass::MalformedWorkerResult));
    assert!(unique.contains(&FailureClass::MalformedMedia));
    assert!(unique.contains(&FailureClass::UserCancellation));
    assert!(unique.contains(&FailureClass::StaleIdentityEvidence));
    assert!(unique.contains(&FailureClass::ClosureResolutionIncomplete));
    assert!(unique.contains(&FailureClass::BlockedByActiveUseLease));
    assert!(unique.contains(&FailureClass::ApprovalRequired));
    assert!(unique.contains(&FailureClass::PriorityPolicyConflict));
    assert!(unique.contains(&FailureClass::ProgressTimeout));
    assert!(unique.contains(&FailureClass::AmbiguousWorkerSelection));
}

#[test]
fn all_variants_have_retry_and_error_code_mappings() {
    for class in FailureClass::ALL {
        let _ = class.retry_class();
        let _ = class.into_error_code();
    }
}

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
        FailureClass::MalformedMedia,
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
fn from_error_code_worker_timeout_returns_worker_timeout_not_progress_timeout() {
    // ProgressTimeout has no dedicated ErrorCode; it intentionally aliases
    // WorkerTimeout on the wire. The round-trip is therefore lossy by design:
    // a ProgressTimeout failure surfaces as WORKER_TIMEOUT and cannot be
    // recovered as ProgressTimeout from the code alone.
    assert_eq!(
        FailureClass::ProgressTimeout.into_error_code(),
        ErrorCode::WorkerTimeout
    );
    assert_eq!(
        FailureClass::WorkerTimeout.into_error_code(),
        ErrorCode::WorkerTimeout
    );
    assert_eq!(
        FailureClass::from_error_code(ErrorCode::WorkerTimeout),
        Some(FailureClass::WorkerTimeout)
    );
}

#[test]
fn from_error_code_maps_failure_taxonomy_and_rejects_unclassified_codes() {
    assert_eq!(
        FailureClass::from_error_code(ErrorCode::WorkerCrash),
        Some(FailureClass::WorkerCrash)
    );
    assert_eq!(
        FailureClass::from_error_code(ErrorCode::BlockedByUseLease),
        Some(FailureClass::BlockedByActiveUseLease)
    );
    assert_eq!(FailureClass::from_error_code(ErrorCode::Internal), None);
}

#[test]
fn malformed_media_is_non_retriable_and_round_trips_its_own_error_code() {
    assert_eq!(
        FailureClass::MalformedMedia.retry_class(),
        FailureRetryClass::NonRetriable
    );
    assert!(!FailureClass::MalformedMedia.is_retriable());
    assert_eq!(
        FailureClass::MalformedMedia.into_error_code(),
        ErrorCode::MalformedMedia
    );
    assert_eq!(
        FailureClass::from_error_code(ErrorCode::MalformedMedia),
        Some(FailureClass::MalformedMedia)
    );
    // Distinct from MalformedWorkerResult — a corrupt source is not a corrupt
    // worker result.
    assert_ne!(
        FailureClass::MalformedMedia,
        FailureClass::MalformedWorkerResult
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

#[test]
fn stale_identity_evidence_maps_to_its_own_error_code() {
    assert_eq!(
        FailureClass::StaleIdentityEvidence.into_error_code(),
        ErrorCode::StaleIdentityEvidence,
    );
}

#[test]
fn closure_resolution_incomplete_maps_to_its_own_error_code() {
    assert_eq!(
        FailureClass::ClosureResolutionIncomplete.into_error_code(),
        ErrorCode::ClosureResolutionIncomplete,
    );
}

#[test]
fn blocked_by_active_use_lease_maps_to_blocked_by_use_lease_error_code() {
    assert_eq!(
        FailureClass::BlockedByActiveUseLease.into_error_code(),
        ErrorCode::BlockedByUseLease,
    );
}

#[test]
fn approval_required_still_maps_to_approval_required_error_code() {
    assert_eq!(
        FailureClass::ApprovalRequired.into_error_code(),
        ErrorCode::ApprovalRequired,
    );
}
