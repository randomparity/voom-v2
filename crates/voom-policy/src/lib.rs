//! Policy-domain inputs for Sprint 3.

pub mod fixtures;
pub mod model;

pub use fixtures::{FixtureName, load_fixture};
pub use model::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSetValidationError, PolicyInputSourceKind,
    PolicySyntheticTarget, QualityProfileSelection, TargetKind, TargetRef, validate_input_set,
};

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;

#[cfg(test)]
#[path = "fixtures_test.rs"]
mod fixture_tests;
