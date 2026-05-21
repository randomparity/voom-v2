use std::collections::HashSet;

use thiserror::Error;
use voom_core::{ErrorCode, FailureClass, FailureRetryClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureFixture {
    pub name: &'static str,
    pub class: FailureClass,
    pub code: ErrorCode,
    pub retry: FailureRetryClass,
    pub source: FixtureSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureSource {
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
    #[error("failure taxonomy fixture {name} has stale mapping for {class:?}")]
    StaleMapping {
        name: &'static str,
        class: FailureClass,
    },
}

const REGISTRY: &[FailureFixture] = &[
    fixture(
        "failure_taxonomy_worker_timeout",
        FailureClass::WorkerTimeout,
        FixtureSource::ChaosWorkerScenario,
    ),
    fixture(
        "failure_taxonomy_worker_crash",
        FailureClass::WorkerCrash,
        FixtureSource::ChaosWorkerScenario,
    ),
    fixture(
        "failure_taxonomy_no_eligible_worker",
        FailureClass::NoEligibleWorker,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_artifact_unavailable",
        FailureClass::ArtifactUnavailable,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_artifact_checksum_mismatch",
        FailureClass::ArtifactChecksumMismatch,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_external_system_unavailable",
        FailureClass::ExternalSystemUnavailable,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_external_system_rate_limited",
        FailureClass::ExternalSystemRateLimited,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_verification_failure",
        FailureClass::VerificationFailure,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_backup_failure",
        FailureClass::BackupFailure,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_commit_failure",
        FailureClass::CommitFailure,
        FixtureSource::FakeProviderErrorFrame,
    ),
    fixture(
        "failure_taxonomy_policy_parse_error",
        FailureClass::PolicyParseError,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_policy_validation_error",
        FailureClass::PolicyValidationError,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_missing_capability",
        FailureClass::MissingCapability,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_malformed_worker_result",
        FailureClass::MalformedWorkerResult,
        FixtureSource::ChaosWorkerScenario,
    ),
    fixture(
        "failure_taxonomy_user_cancellation",
        FailureClass::UserCancellation,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_stale_identity_evidence",
        FailureClass::StaleIdentityEvidence,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_closure_resolution_incomplete",
        FailureClass::ClosureResolutionIncomplete,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_blocked_by_active_use_lease",
        FailureClass::BlockedByActiveUseLease,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_approval_required",
        FailureClass::ApprovalRequired,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_priority_policy_conflict",
        FailureClass::PriorityPolicyConflict,
        FixtureSource::SyntheticFrame,
    ),
    fixture(
        "failure_taxonomy_progress_timeout",
        FailureClass::ProgressTimeout,
        FixtureSource::ChaosWorkerScenario,
    ),
    fixture(
        "failure_taxonomy_ambiguous_worker_selection",
        FailureClass::AmbiguousWorkerSelection,
        FixtureSource::SyntheticFrame,
    ),
];

#[must_use]
pub fn registry() -> &'static [FailureFixture] {
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
    for fixture in REGISTRY {
        if fixture.code == fixture.class.into_error_code()
            && fixture.retry == fixture.class.retry_class()
        {
            result.pass(fixture.name);
        } else {
            result.fail(
                fixture.name,
                format!(
                    "expected {:?}/{:?}, got {:?}/{:?}",
                    fixture.class.into_error_code(),
                    fixture.class.retry_class(),
                    fixture.code,
                    fixture.retry
                ),
            );
        }
    }
    result
}

#[cfg(test)]
pub(crate) fn validate_registry_with(
    fixtures: &[FailureFixture],
) -> Result<(), FailureTaxonomyError> {
    validate_registry_with_inner(fixtures)
}

#[cfg(not(test))]
fn validate_registry_with(fixtures: &[FailureFixture]) -> Result<(), FailureTaxonomyError> {
    validate_registry_with_inner(fixtures)
}

fn validate_registry_with_inner(fixtures: &[FailureFixture]) -> Result<(), FailureTaxonomyError> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    for fixture in fixtures {
        if !seen.insert(fixture.class) {
            duplicates.push(fixture.class);
        }
        if fixture.code != fixture.class.into_error_code()
            || fixture.retry != fixture.class.retry_class()
        {
            return Err(FailureTaxonomyError::StaleMapping {
                name: fixture.name,
                class: fixture.class,
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

const fn fixture(name: &'static str, class: FailureClass, source: FixtureSource) -> FailureFixture {
    FailureFixture {
        name,
        class,
        code: class.into_error_code(),
        retry: class.retry_class(),
        source,
    }
}

#[cfg(test)]
#[path = "failure_taxonomy_test.rs"]
mod tests;
