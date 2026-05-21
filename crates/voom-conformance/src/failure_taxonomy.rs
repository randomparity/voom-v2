use std::collections::HashSet;

use thiserror::Error;
use voom_core::{ErrorCode, FailureClass, FailureRetryClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureTaxonomyEntry {
    pub name: &'static str,
    pub class: FailureClass,
    pub code: ErrorCode,
    pub retry: FailureRetryClass,
    pub planned_source: PlannedCoverageSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedCoverageSource {
    FakeProviderErrorFrame,
    ChaosWorkerScenario,
    SyntheticFrame,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FailureTaxonomyError {
    #[error("failure taxonomy missing coverage: {missing:?}")]
    Missing { missing: Vec<FailureClass> },
    #[error("failure taxonomy duplicate coverage: {duplicates:?}")]
    Duplicate { duplicates: Vec<FailureClass> },
    #[error("failure taxonomy entry {name} has stale mapping for {class:?}")]
    StaleMapping {
        name: &'static str,
        class: FailureClass,
    },
}

const REGISTRY: &[FailureTaxonomyEntry] = &[
    entry(
        "failure_taxonomy_worker_timeout",
        FailureClass::WorkerTimeout,
        PlannedCoverageSource::ChaosWorkerScenario,
    ),
    entry(
        "failure_taxonomy_worker_crash",
        FailureClass::WorkerCrash,
        PlannedCoverageSource::ChaosWorkerScenario,
    ),
    entry(
        "failure_taxonomy_no_eligible_worker",
        FailureClass::NoEligibleWorker,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_artifact_unavailable",
        FailureClass::ArtifactUnavailable,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_artifact_checksum_mismatch",
        FailureClass::ArtifactChecksumMismatch,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_external_system_unavailable",
        FailureClass::ExternalSystemUnavailable,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_external_system_rate_limited",
        FailureClass::ExternalSystemRateLimited,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_verification_failure",
        FailureClass::VerificationFailure,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_backup_failure",
        FailureClass::BackupFailure,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_commit_failure",
        FailureClass::CommitFailure,
        PlannedCoverageSource::FakeProviderErrorFrame,
    ),
    entry(
        "failure_taxonomy_policy_parse_error",
        FailureClass::PolicyParseError,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_policy_validation_error",
        FailureClass::PolicyValidationError,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_missing_capability",
        FailureClass::MissingCapability,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_malformed_worker_result",
        FailureClass::MalformedWorkerResult,
        PlannedCoverageSource::ChaosWorkerScenario,
    ),
    entry(
        "failure_taxonomy_user_cancellation",
        FailureClass::UserCancellation,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_stale_identity_evidence",
        FailureClass::StaleIdentityEvidence,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_closure_resolution_incomplete",
        FailureClass::ClosureResolutionIncomplete,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_blocked_by_active_use_lease",
        FailureClass::BlockedByActiveUseLease,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_approval_required",
        FailureClass::ApprovalRequired,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_priority_policy_conflict",
        FailureClass::PriorityPolicyConflict,
        PlannedCoverageSource::SyntheticFrame,
    ),
    entry(
        "failure_taxonomy_progress_timeout",
        FailureClass::ProgressTimeout,
        PlannedCoverageSource::ChaosWorkerScenario,
    ),
    entry(
        "failure_taxonomy_ambiguous_worker_selection",
        FailureClass::AmbiguousWorkerSelection,
        PlannedCoverageSource::SyntheticFrame,
    ),
];

#[must_use]
pub fn registry() -> &'static [FailureTaxonomyEntry] {
    REGISTRY
}

pub fn validate_registry() -> Result<(), FailureTaxonomyError> {
    validate_registry_with(REGISTRY)
}

#[must_use]
pub fn run() -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    match validate_registry() {
        Ok(()) => result.pass("failure_taxonomy_registry_complete"),
        Err(e) => {
            result.fail("failure_taxonomy_registry_complete", e.to_string());
            return result;
        }
    }
    result.pass("failure_taxonomy_registry_mappings_current");
    result
}

#[cfg(test)]
pub(crate) fn validate_registry_with(
    entries: &[FailureTaxonomyEntry],
) -> Result<(), FailureTaxonomyError> {
    validate_registry_with_inner(entries)
}

#[cfg(not(test))]
fn validate_registry_with(entries: &[FailureTaxonomyEntry]) -> Result<(), FailureTaxonomyError> {
    validate_registry_with_inner(entries)
}

fn validate_registry_with_inner(
    entries: &[FailureTaxonomyEntry],
) -> Result<(), FailureTaxonomyError> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    for entry in entries {
        if !seen.insert(entry.class) {
            duplicates.push(entry.class);
        }
        if entry.code != entry.class.into_error_code() || entry.retry != entry.class.retry_class() {
            return Err(FailureTaxonomyError::StaleMapping {
                name: entry.name,
                class: entry.class,
            });
        }
    }
    if !duplicates.is_empty() {
        return Err(FailureTaxonomyError::Duplicate { duplicates });
    }

    let missing = FailureClass::ALL
        .iter()
        .copied()
        .filter(|class| !seen.contains(class))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(FailureTaxonomyError::Missing { missing })
    }
}

const fn entry(
    name: &'static str,
    class: FailureClass,
    planned_source: PlannedCoverageSource,
) -> FailureTaxonomyEntry {
    FailureTaxonomyEntry {
        name,
        class,
        code: class.into_error_code(),
        retry: class.retry_class(),
        planned_source,
    }
}

#[cfg(test)]
#[path = "failure_taxonomy_test.rs"]
mod tests;
