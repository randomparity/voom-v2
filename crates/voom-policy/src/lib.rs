#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::float_cmp,
        reason = "policy tests assert deterministic fixtures directly and use unwrap for concise failures"
    )
)]
//! Policy-domain inputs for Sprint 3.

pub mod fixtures;
pub mod model;

pub use fixtures::{FixtureName, load_fixture};
pub use model::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSetValidationError, PolicyInputSourceKind,
    PolicySyntheticTarget, QualityProfileSelection, TargetKind, TargetRef, validate_input_set,
};
