#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::float_cmp,
        reason = "policy tests assert deterministic fixtures directly and use unwrap for concise failures"
    )
)]
//! Policy-domain inputs for Sprint 3.

pub mod ast;
pub mod compiled;
pub mod diagnostic;
pub mod fixtures;
pub mod model;
pub mod parser;
pub mod pipeline;
pub mod policy_fixtures;
pub mod span;
pub mod validate;

pub use ast::{ExprAst, PhaseAst, PolicyAst, SettingAst, Spanned, StatementAst};
pub use compiled::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPhase, CompiledPolicy,
    CompiledRule, CompiledValue, DefaultStrategy, ErrorStrategy, PolicyProvenance, RuleMatchMode,
    TrackFilter, TrackTarget, compile_ast, deterministic_json, source_hash,
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
