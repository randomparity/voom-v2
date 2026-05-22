//! Policy-domain inputs for Sprint 3.

pub mod model;

pub use model::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSetValidationError,
    PolicyInputSourceKind, PolicySyntheticTarget, QualityProfileSelection, TargetKind, TargetRef,
    validate_input_set,
};

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
