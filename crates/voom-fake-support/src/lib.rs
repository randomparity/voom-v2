#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Shared runtime, validation, scenario, and result helpers for fake providers.
//!
//! Its production consumers are the eleven `fake-*` binaries in `voom-fakes`;
//! control-plane integration tests also reuse the provider dispatcher.
//! `chaos-worker`, `benchmark-worker`, and `voom-conformance` do not depend on
//! this crate, keeping their behavior independent of shared fake-provider bugs.

mod catalog;
mod results;
mod runtime;
mod scenario;
mod streaming;
mod validation;

pub use catalog::{ProviderDefinition, provider_definition, provider_definition_for_operation};
pub use results::{FAKE_SCANNER_FIRST_LOCATION_ID, synthetic_artifact_access_evidence};
pub use runtime::{dispatch_provider, run_provider};
pub use scenario::{Scenario, ScenarioError, ScenarioEvent, ScenarioPlayer, load_scenario};

#[cfg(test)]
pub(crate) use validation::{MAX_FAKE_DURATION_MS, MAX_FAKE_FAN_OUT_COUNT};

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
