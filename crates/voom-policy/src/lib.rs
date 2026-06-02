#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::float_cmp,
        clippy::panic,
        reason = "policy tests assert deterministic fixtures directly and use unwrap/panic for concise failures"
    )
)]
//! Policy-domain inputs for Sprint 3.

pub mod compile;
pub mod data;
pub mod diagnostic;
#[path = "fixtures/mod.rs"]
mod fixture_domain;
pub mod syntax;

pub use compile::{compiled, pipeline, validate};
pub use data::{model, video_profile};
pub use fixture_domain::{fixtures, policy_fixtures};
pub(crate) use syntax::text;
pub use syntax::{ast, parser, span};

pub use ast::{ExprAst, PhaseAst, PolicyAst, SettingAst, Spanned, StatementAst};
pub use compiled::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPhase, CompiledPolicy,
    CompiledRule, CompiledValue, DefaultStrategy, ErrorStrategy, PolicyProvenance, RuleMatchMode,
    TrackFilter, TrackTarget, deterministic_json, source_hash,
};
pub use diagnostic::{
    DiagnosticCode, DiagnosticSeverity, DiagnosticStage, PolicyDiagnostic, RelatedSpan,
};
pub use fixtures::{FixtureName, load_fixture};
pub use model::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSetValidationError, PolicyInputSourceKind,
    PolicySyntheticTarget, QualityProfileSelection, TargetKind, TargetRef, validate_input_set,
};
pub use parser::{ParseError, parse_policy_source};
pub use pipeline::{
    CompileOutput, PolicyCompileError, compile_policy, parse_policy, validate_policy,
};
pub use policy_fixtures::{
    PolicyFixture, invalid_policy_fixtures, load_json_fixture, load_policy_fixture,
    valid_policy_fixtures,
};
pub use span::{SourceLocation, SourceSpan, line_column};
pub use validate::{ValidationResult, validate_policy_ast};
pub use video_profile::{VideoProfileRef, VideoProfileSettings};
