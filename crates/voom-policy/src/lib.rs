#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::float_cmp,
        reason = "policy tests assert deterministic fixtures directly and use unwrap for concise failures"
    )
)]
//! Policy-domain inputs for Sprint 3.

pub mod diagnostic;
pub mod fixtures;
pub mod model;
pub mod span;

pub use diagnostic::{
    DiagnosticCode, DiagnosticSeverity, DiagnosticStage, PolicyDiagnostic, RelatedSpan,
};
pub use fixtures::{FixtureName, load_fixture};
pub use model::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSetValidationError, PolicyInputSourceKind,
    PolicySyntheticTarget, QualityProfileSelection, TargetKind, TargetRef, validate_input_set,
};
pub use span::{SourceLocation, SourceSpan, line_column};
