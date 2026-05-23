# VOOM Sprint 4 Policy DSL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Sprint 4 `.voom` parser, validator, compiled policy IR, deterministic fixtures, durable policy registry, and control-plane policy use cases described in `docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md`.

**Architecture:** `voom-policy` owns source parsing, diagnostics, semantic validation, and compiled policy projections without a database dependency. `voom-store` owns migration 0007 and policy document/version persistence with database-backed identity invariants. `voom-control-plane` composes compile-only and accepted-version persistence use cases without adding CLI/API commands or event vocabulary.

**Tech Stack:** Rust 2024, serde/serde_json with deterministic `BTreeMap`-backed projections, blake3 source hashing, sqlx SQLite, tokio tests, existing sibling unit-test layout, `just ci`.

---

## File Map

- `crates/voom-core/src/ids.rs`, `ids_test.rs`, `lib.rs`: add `PolicyDocumentId` and `PolicyVersionId`.
- `crates/voom-policy/Cargo.toml`: add `blake3 = { workspace = true }`.
- `crates/voom-policy/src/lib.rs`: export Sprint 4 modules while preserving Sprint 3 input exports.
- `crates/voom-policy/src/span.rs` and `span_test.rs`: source span and line/column helpers.
- `crates/voom-policy/src/diagnostic.rs` and `diagnostic_test.rs`: stable diagnostics and compile result types.
- `crates/voom-policy/src/ast.rs` and `ast_test.rs`: syntax tree for the accepted Sprint 4 subset.
- `crates/voom-policy/src/parser.rs` and `parser_test.rs`: hand-written lexer/parser for v1-style block syntax.
- `crates/voom-policy/src/validate.rs` and `validate_test.rs`: semantic validation and warnings.
- `crates/voom-policy/src/compiled.rs` and `compiled_test.rs`: v2-native compiled policy IR, source hashing, deterministic JSON projection.
- `crates/voom-policy/src/pipeline.rs` and `pipeline_test.rs`: public `parse_policy`, `validate_policy`, and `compile_policy` functions.
- `crates/voom-policy/fixtures/policies/*.voom`: valid and invalid source fixtures.
- `crates/voom-policy/fixtures/compiled/minimal.json`: deterministic compiled projection for `minimal.voom`.
- `crates/voom-policy/fixtures/compiled/container-metadata.json`: deterministic compiled projection for `container-metadata.voom`.
- `crates/voom-policy/fixtures/compiled/production-normalize-reduced.json`: deterministic compiled projection for `production-normalize-reduced.voom`.
- `crates/voom-policy/fixtures/diagnostics/invalid-deferred-transcode.json`: deterministic diagnostics for deferred execution.
- `crates/voom-policy/fixtures/diagnostics/invalid-extends.json`: deterministic diagnostics for deferred composition.
- `crates/voom-policy/fixtures/diagnostics/invalid-extend-phase.json`: deterministic diagnostics for deferred phase inheritance.
- `crates/voom-policy/fixtures/diagnostics/invalid-unknown-core-field.json`: deterministic diagnostics for invalid field paths.
- `crates/voom-policy/src/policy_fixtures.rs` and `policy_fixtures_test.rs`: fixture loader for policy-language tests.
- `migrations/0007_policy_registry.sql`: policy document/version tables and triggers.
- `crates/voom-store/src/migrator.rs`: register migration 0007.
- `crates/voom-store/src/repo/policies.rs` and `policies_test.rs`: policy registry repository.
- `crates/voom-store/src/repo/identity.rs` and `identity_test.rs`: reject non-version values for `accepted_policy_id` once policy versions exist.
- `crates/voom-store/src/repo/mod.rs`: export policy registry repo types.
- `crates/voom-control-plane/src/lib.rs`: add `SqlitePolicyRepo`.
- `crates/voom-control-plane/src/cases/policies.rs` and `policies_test.rs`: compile/create/add/get/list use cases.
- `crates/voom-control-plane/src/cases/mod.rs`: expose the new case module.
- `docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md`: add closeout traceability notes only if implementation discovers a legitimate deferral.

## Task 1: Core Policy Ids

**Files:**
- Modify: `crates/voom-core/src/ids.rs`
- Modify: `crates/voom-core/src/ids_test.rs`
- Check: `crates/voom-core/src/lib.rs`

- [ ] **Step 1: Write failing id tests**

Add to `crates/voom-core/src/ids_test.rs`:

```rust
#[test]
fn policy_document_id_displays_inner_u64() {
    assert_eq!(PolicyDocumentId(42).to_string(), "42");
}

#[test]
fn policy_version_id_round_trips_through_json() {
    let id = PolicyVersionId(7);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "7");
    assert_eq!(serde_json::from_str::<PolicyVersionId>(&json).unwrap(), id);
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test -p voom-core policy_document_id`

Expected: compile failure naming missing `PolicyDocumentId` and `PolicyVersionId`.

- [ ] **Step 3: Add ids**

Add to `crates/voom-core/src/ids.rs` after the Sprint 3 policy input ids:

```rust
// Policy registry layer (Sprint 4).
define_id!(PolicyDocumentId);
define_id!(PolicyVersionId);
```

Add both ids to the `pub use ids::{...};` list in `crates/voom-core/src/lib.rs`.

- [ ] **Step 4: Run tests and commit**

Run: `cargo test -p voom-core policy_`

Expected: PASS for the new id tests.

Commit:

```bash
git add crates/voom-core/src/ids.rs crates/voom-core/src/ids_test.rs crates/voom-core/src/lib.rs
git commit -m "feat: add policy registry ids"
```

## Task 2: Policy Spans And Diagnostics

**Files:**
- Modify: `crates/voom-policy/src/lib.rs`
- Create: `crates/voom-policy/src/span.rs`
- Create: `crates/voom-policy/src/span_test.rs`
- Create: `crates/voom-policy/src/diagnostic.rs`
- Create: `crates/voom-policy/src/diagnostic_test.rs`

- [ ] **Step 1: Write span tests**

Create `span_test.rs`:

```rust
use super::*;

#[test]
fn line_column_maps_byte_offsets() {
    let source = "policy \"a\" {\n  phase one {}\n}\n";
    let location = line_column(source, 15);
    assert_eq!(location.line, 2);
    assert_eq!(location.column, 3);
}

#[test]
fn span_contains_start_and_end_bytes() {
    let span = SourceSpan::new(2, 5);
    assert_eq!(span.start, 2);
    assert_eq!(span.end, 5);
    assert_eq!(span.len(), 3);
}
```

- [ ] **Step 2: Write diagnostic tests**

Create `diagnostic_test.rs`:

```rust
use super::*;
use crate::span::SourceSpan;

#[test]
fn diagnostic_serializes_stable_fields() {
    let diagnostic = PolicyDiagnostic::error(
        DiagnosticCode::DuplicatePhaseName,
        DiagnosticStage::Validate,
        SourceSpan::new(10, 15),
        SourceLocation { line: 2, column: 5 },
        "duplicate phase name",
    );

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["code"], "duplicate_phase_name");
    assert_eq!(json["severity"], "error");
    assert_eq!(json["stage"], "validate");
    assert_eq!(json["span"]["start"], 10);
}
```

- [ ] **Step 3: Run tests and verify failure**

Run: `cargo test -p voom-policy span diagnostic`

Expected: compile failure because the modules and types do not exist.

- [ ] **Step 4: Implement `span.rs`**

Create:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

impl SourceSpan {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

#[must_use]
pub fn line_column(source: &str, byte_offset: usize) -> SourceLocation {
    let mut line = 1;
    let mut column = 1;
    for (idx, ch) in source.char_indices() {
        if idx >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    SourceLocation { line, column }
}

#[cfg(test)]
#[path = "span_test.rs"]
mod tests;
```

- [ ] **Step 5: Implement `diagnostic.rs`**

Define `DiagnosticCode` with every code from the Sprint 4 spec and `as_str()` returning snake_case strings:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    UnexpectedToken,
    SourceSizeExceeded,
    UnknownTopLevelBlock,
    UnknownPhaseStatementOrOperation,
    DeferredPhaseInheritance,
    DuplicatePhaseName,
    UnknownPhaseReference,
    SelfDependency,
    DependencyCycle,
    InvalidRunIfTrigger,
    InvalidOnErrorValue,
    UnsupportedContainer,
    InvalidTrackTarget,
    InvalidDefaultStrategy,
    InvalidLanguageCode,
    InvalidCoreFieldPath,
    InvalidRuleMatchMode,
    UnknownExtensionNamespace,
    TagOrderingError,
    AmbiguousTagOperationConflict,
    DeferredComposition,
    DeferredExecutionOperation,
}
```

Also define `DiagnosticSeverity`, `DiagnosticStage`, `RelatedSpan`, `PolicyDiagnostic`, and constructors:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PolicyDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub stage: DiagnosticStage,
    pub span: crate::span::SourceSpan,
    pub location: crate::span::SourceLocation,
    pub message: String,
    pub suggestion: Option<String>,
    pub related: Vec<RelatedSpan>,
}

impl PolicyDiagnostic {
    #[must_use]
    pub fn error(
        code: DiagnosticCode,
        stage: DiagnosticStage,
        span: crate::span::SourceSpan,
        location: crate::span::SourceLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.as_str().to_owned(),
            severity: DiagnosticSeverity::Error,
            stage,
            span,
            location,
            message: message.into(),
            suggestion: None,
            related: Vec::new(),
        }
    }
}
```

Use `#[serde(rename_all = "snake_case")]` on the severity and stage enums. Add `#[cfg(test)] #[path = "diagnostic_test.rs"] mod tests;`.

- [ ] **Step 6: Export modules and run tests**

Update `lib.rs`:

```rust
pub mod diagnostic;
pub mod span;

pub use diagnostic::{
    DiagnosticCode, DiagnosticSeverity, DiagnosticStage, PolicyDiagnostic, RelatedSpan,
};
pub use span::{SourceLocation, SourceSpan, line_column};
```

Run: `cargo test -p voom-policy span diagnostic`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-policy/src/lib.rs crates/voom-policy/src/span.rs crates/voom-policy/src/span_test.rs crates/voom-policy/src/diagnostic.rs crates/voom-policy/src/diagnostic_test.rs
git commit -m "feat: add policy diagnostics"
```

## Task 3: AST And Parser

**Files:**
- Modify: `crates/voom-policy/src/lib.rs`
- Create: `crates/voom-policy/src/ast.rs`
- Create: `crates/voom-policy/src/ast_test.rs`
- Create: `crates/voom-policy/src/parser.rs`
- Create: `crates/voom-policy/src/parser_test.rs`

- [ ] **Step 1: Write parser tests**

Create `parser_test.rs` with focused acceptance cases:

```rust
use super::*;

#[test]
fn parses_minimal_policy_with_phase() {
    let ast = parse_policy_source("policy \"minimal\" { phase inspect { container mkv } }").unwrap();
    assert_eq!(ast.name.value, "minimal");
    assert_eq!(ast.phases.len(), 1);
    assert_eq!(ast.phases[0].name.value, "inspect");
}

#[test]
fn parses_comments_and_free_form_whitespace() {
    let ast = parse_policy_source(
        "policy \"comments\" {\n// comment\nphase normalize {\n keep audio where lang in [eng, und]\n}\n}",
    )
    .unwrap();
    assert_eq!(ast.phases[0].operations.len(), 1);
}

#[test]
fn reports_parse_diagnostic_for_unclosed_block() {
    let err = parse_policy_source("policy \"broken\" { phase one {").unwrap_err();
    assert_eq!(err.diagnostics[0].code, "unexpected_token");
    assert_eq!(err.diagnostics[0].stage, crate::DiagnosticStage::Parse);
}
```

- [ ] **Step 2: Define AST**

`ast.rs` should keep syntax close to source and defer semantic decisions to validation:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: crate::SourceSpan,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyAst {
    pub name: Spanned<String>,
    pub extends: Option<Spanned<String>>,
    pub metadata: Vec<SettingAst>,
    pub config: Vec<StatementAst>,
    pub phases: Vec<PhaseAst>,
    pub unknown_top_level: Vec<StatementAst>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseAst {
    pub name: Spanned<String>,
    pub controls: Vec<StatementAst>,
    pub operations: Vec<StatementAst>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingAst {
    pub key: Spanned<String>,
    pub value: ExprAst,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementAst {
    Raw {
        keyword: Spanned<String>,
        text: String,
        span: crate::SourceSpan,
    },
    Block {
        keyword: Spanned<String>,
        name: Option<Spanned<String>>,
        statements: Vec<StatementAst>,
        span: crate::SourceSpan,
    },
}
```

Also add `ExprAst` for strings, identifiers, numbers, booleans, lists, and raw field paths. Add the sibling test module declaration.

- [ ] **Step 3: Implement a hand-written lexer/parser**

Implement `parse_policy_source(source: &str) -> Result<PolicyAst, ParseError>`. The parser must:

- skip whitespace and `//` comments;
- parse exactly one `policy "<name>"` root;
- parse optional `extends "<parent>"` and preserve it in AST;
- parse `metadata { key: value }`, `config { ... }`, and one or more `phase <identifier> { ... }`;
- preserve unknown top-level and phase statements as raw statements so validation emits stable validation diagnostics;
- return `unexpected_token` parse diagnostics for malformed strings, missing braces, and missing policy name.

Use this error shape:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub diagnostics: Vec<crate::PolicyDiagnostic>,
}
```

- [ ] **Step 4: Export and run tests**

Update `lib.rs`:

```rust
pub mod ast;
pub mod parser;

pub use ast::{ExprAst, PhaseAst, PolicyAst, SettingAst, Spanned, StatementAst};
pub use parser::{ParseError, parse_policy_source};
```

Run: `cargo test -p voom-policy parser`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-policy/src/lib.rs crates/voom-policy/src/ast.rs crates/voom-policy/src/ast_test.rs crates/voom-policy/src/parser.rs crates/voom-policy/src/parser_test.rs
git commit -m "feat: parse sprint 4 policy syntax"
```

## Task 4: Semantic Validation

**Files:**
- Modify: `crates/voom-policy/src/lib.rs`
- Create: `crates/voom-policy/src/validate.rs`
- Create: `crates/voom-policy/src/validate_test.rs`

- [ ] **Step 1: Write validation tests**

Create tests for each load-bearing rule:

```rust
use crate::{DiagnosticCode, parse_policy_source};
use super::*;

fn codes(source: &str) -> Vec<String> {
    let ast = parse_policy_source(source).unwrap();
    validate_policy_ast(source, &ast)
        .diagnostics
        .into_iter()
        .map(|d| d.code)
        .collect()
}

#[test]
fn rejects_duplicate_phase_names() {
    assert!(codes("policy \"p\" { phase a {} phase a {} }").contains(&"duplicate_phase_name".to_owned()));
}

#[test]
fn rejects_unknown_dependency() {
    assert!(codes("policy \"p\" { phase a { depends_on: [missing] } }").contains(&"unknown_phase_reference".to_owned()));
}

#[test]
fn rejects_deferred_execution_operations() {
    assert!(codes("policy \"p\" { phase a { transcode video to hevc {} } }").contains(&"deferred_execution_operation".to_owned()));
}

#[test]
fn warns_for_unknown_plugin_namespace() {
    let ast = parse_policy_source("policy \"p\" { phase a { set_tag \"title\" plugin.radarr.title } }").unwrap();
    let result = validate_policy_ast("", &ast);
    assert!(result.diagnostics.iter().any(|d| d.code == "unknown_extension_namespace"));
    assert!(result.diagnostics.iter().all(|d| d.severity == crate::DiagnosticSeverity::Warning));
}

#[test]
fn rejects_unknown_core_field_root() {
    assert!(codes("policy \"p\" { phase a { when vidio.codec == hevc { container mkv } } }")
        .contains(&"invalid_core_field_path".to_owned()));
}
```

- [ ] **Step 2: Implement validation result**

Create:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationResult {
    pub diagnostics: Vec<crate::PolicyDiagnostic>,
}

impl ValidationResult {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == crate::DiagnosticSeverity::Error)
    }
}
```

- [ ] **Step 3: Implement validators**

Implement `validate_policy_ast(source: &str, ast: &PolicyAst) -> ValidationResult` with these exact rule groups:

- policy name trim-empty check;
- source byte length `> 1_048_576`;
- `extends` produces `deferred_composition`;
- `extend` statement produces `deferred_phase_inheritance`;
- duplicate phase names;
- `depends_on` and `run_if` phase references exist and are not self-references;
- phase dependency graph is acyclic;
- `run_if` trigger is `modified` or `completed`;
- `on_error` is `abort`, `continue`, or `skip`;
- `container` is `mkv` for Sprint 4;
- track targets are `video`, `audio`, `subtitle`, `subtitles`, `attachment`, or `attachments`;
- default strategies are `first`, `best`, `none`, or `preserve`;
- language identifiers are `eng`, `und`, or valid three-letter ASCII lowercase codes;
- `rules` mode is `first` or `all`;
- core field roots match the spec list;
- extension field roots `plugin` and `external` warn below the root with `unknown_extension_namespace`;
- `set_tag` before `clear_tags` in one phase errors;
- `set_tag` and `delete_tag` on the same tag key in one phase errors;
- unknown top-level blocks, unknown phase statements, and unknown operation names error.

- [ ] **Step 4: Export and run tests**

Update `lib.rs`:

```rust
pub mod validate;
pub use validate::{ValidationResult, validate_policy_ast};
```

Run: `cargo test -p voom-policy validate`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-policy/src/lib.rs crates/voom-policy/src/validate.rs crates/voom-policy/src/validate_test.rs
git commit -m "feat: validate sprint 4 policy subset"
```

## Task 5: Compiled Policy IR And Pipeline

**Files:**
- Modify: `crates/voom-policy/Cargo.toml`
- Modify: `crates/voom-policy/src/lib.rs`
- Create: `crates/voom-policy/src/compiled.rs`
- Create: `crates/voom-policy/src/compiled_test.rs`
- Create: `crates/voom-policy/src/pipeline.rs`
- Create: `crates/voom-policy/src/pipeline_test.rs`

- [ ] **Step 1: Add hash dependency**

Add to `crates/voom-policy/Cargo.toml`:

```toml
blake3 = { workspace = true }
```

- [ ] **Step 2: Write compiler tests**

Create `compiled_test.rs`:

```rust
use super::*;

#[test]
fn source_hash_uses_exact_bytes() {
    let a = source_hash("policy \"p\" { phase a {} }");
    let b = source_hash("policy \"p\" {\n phase a {}\n}");
    assert_ne!(a, b);
}

#[test]
fn compiled_json_is_deterministic() {
    let policy = CompiledPolicy::minimal_for_test("p", "hash");
    let first = deterministic_json(&policy).unwrap();
    let second = deterministic_json(&policy).unwrap();
    assert_eq!(first, second);
}
```

Create `pipeline_test.rs`:

```rust
use super::*;

#[test]
fn compile_policy_returns_validation_error_diagnostics() {
    let err = compile_policy("policy \"p\" { phase a { transcode video to hevc {} } }").unwrap_err();
    assert_eq!(err.code(), voom_core::VoomError::PolicyValidationError("x".to_owned()).code());
    assert!(err.diagnostics.iter().any(|d| d.code == "deferred_execution_operation"));
}

#[test]
fn compile_policy_produces_phase_order() {
    let out = compile_policy("policy \"p\" { phase a {} phase b { depends_on: [a] } }").unwrap();
    assert_eq!(out.policy.phase_order, ["a", "b"]);
}
```

- [ ] **Step 3: Implement compiled types**

In `compiled.rs`, define serde-enabled v2 IR:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledPolicy {
    pub policy_name: String,
    pub slug: String,
    pub source_hash: String,
    pub schema_version: u32,
    pub metadata: std::collections::BTreeMap<String, serde_json::Value>,
    pub config: std::collections::BTreeMap<String, serde_json::Value>,
    pub phases: Vec<CompiledPhase>,
    pub phase_order: Vec<String>,
    pub warnings: Vec<crate::PolicyDiagnostic>,
    pub provenance: PolicyProvenance,
}
```

Define the accepted operation and condition shapes explicitly so the compiler cannot fall back to raw JSON:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledOperation {
    SetContainer { container: String },
    KeepTracks { target: TrackTarget, filter: Option<TrackFilter> },
    RemoveTracks { target: TrackTarget, filter: Option<TrackFilter> },
    ReorderTracks { targets: Vec<TrackTarget> },
    SetDefaults { target: TrackTarget, strategy: DefaultStrategy },
    ClearTrackActions { target: TrackTarget },
    ClearTags,
    SetTag { key: String, value: CompiledValue },
    DeleteTag { key: String },
    Conditional { condition: CompiledCondition, operations: Vec<CompiledOperation> },
    Rules { mode: RuleMatchMode, rules: Vec<CompiledRule> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackTarget {
    Video,
    Audio,
    Subtitle,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrackFilter {
    LanguageIn { values: Vec<String> },
    CodecIn { values: Vec<String> },
    Commentary,
    Forced,
    Default,
    Font,
    TitleContains { value: String },
    TitleMatches { value: String },
    Not { inner: Box<TrackFilter> },
    And { filters: Vec<TrackFilter> },
    Or { filters: Vec<TrackFilter> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledCondition {
    Exists { target: TrackTarget, filter: Option<TrackFilter> },
    Count { target: TrackTarget, op: ComparisonOp, value: u64 },
    FieldComparison { path: Vec<String>, op: ComparisonOp, value: CompiledValue },
    FieldExists { path: Vec<String> },
    Predicate { name: String },
    Not { inner: Box<CompiledCondition> },
    And { conditions: Vec<CompiledCondition> },
    Or { conditions: Vec<CompiledCondition> },
}
```

Also define `CompiledRule`, `CompiledValue`, `ComparisonOp`, `DefaultStrategy`, `RuleMatchMode`, and `ErrorStrategy` with `#[serde(rename_all = "snake_case")]` where applicable. Keep raw source text out of the compiled IR; persistence stores it separately.

- [ ] **Step 4: Implement hash and deterministic JSON**

Add:

```rust
#[must_use]
pub fn source_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

pub fn deterministic_json(policy: &CompiledPolicy) -> Result<serde_json::Value, voom_core::VoomError> {
    serde_json::to_value(policy)
        .map_err(|e| voom_core::VoomError::Internal(format!("compiled policy serialize: {e}")))
}
```

Use `BTreeMap` for metadata/config/provenance maps so object keys are deterministic.

Implement `CompiledPolicy::minimal_for_test(policy_name: &str, source_hash: &str)` behind `#[cfg(test)]` so the determinism test compiles without making a test helper part of the production API.

- [ ] **Step 4a: Implement operation lowering**

Lower validated `StatementAst` values into `CompiledOperation` variants with exhaustive `match` arms for the accepted Sprint 4 operation keywords:

- `container mkv` -> `SetContainer { container: "mkv" }`;
- `keep audio where lang in [eng, und]` -> `KeepTracks`;
- `remove attachments where not font` -> `RemoveTracks`;
- `order tracks [video, audio, subtitle]` -> `ReorderTracks`;
- `defaults audio: first` -> `SetDefaults`;
- `actions audio clear` -> `ClearTrackActions`;
- `clear_tags` -> `ClearTags`;
- `set_tag "title" plugin.radarr.title` -> `SetTag`;
- `delete_tag "encoder"` -> `DeleteTag`;
- `when <condition> { ... }` -> `Conditional`;
- `rules first { rule "name" { when <condition> { ... } } }` -> `Rules`.

If a statement reaches lowering without a matching accepted variant, return `VoomError::PolicyValidationError` with an `unknown_phase_statement_or_operation` diagnostic. This is a defensive check; normal callers should have seen the same problem during validation.

- [ ] **Step 5: Implement pipeline**

`pipeline.rs` exposes:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct CompileOutput {
    pub policy: crate::CompiledPolicy,
    pub diagnostics: Vec<crate::PolicyDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyCompileError {
    pub error: voom_core::VoomError,
    pub diagnostics: Vec<crate::PolicyDiagnostic>,
}

impl PolicyCompileError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}

pub fn parse_policy(source: &str) -> Result<crate::PolicyAst, PolicyCompileError>;
pub fn validate_policy(source: &str) -> Result<crate::ValidationResult, PolicyCompileError>;
pub fn compile_policy(source: &str) -> Result<CompileOutput, PolicyCompileError>;
```

Parse errors map to `VoomError::PolicyParseError`; validation/compile errors map to `VoomError::PolicyValidationError`.

- [ ] **Step 6: Export and run tests**

Update `lib.rs` exports for compiled and pipeline types. Run:

`cargo test -p voom-policy compiled pipeline`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-policy/Cargo.toml crates/voom-policy/src/lib.rs crates/voom-policy/src/compiled.rs crates/voom-policy/src/compiled_test.rs crates/voom-policy/src/pipeline.rs crates/voom-policy/src/pipeline_test.rs
git commit -m "feat: compile policies to v2 ir"
```

## Task 6: Policy Fixtures And Golden Projections

**Files:**
- Modify: `crates/voom-policy/src/lib.rs`
- Create: `crates/voom-policy/src/policy_fixtures.rs`
- Create: `crates/voom-policy/src/policy_fixtures_test.rs`
- Create: `crates/voom-policy/fixtures/policies/minimal.voom`
- Create: `crates/voom-policy/fixtures/policies/container-metadata.voom`
- Create: `crates/voom-policy/fixtures/policies/production-normalize-reduced.voom`
- Create: `crates/voom-policy/fixtures/policies/invalid-deferred-transcode.voom`
- Create: `crates/voom-policy/fixtures/policies/invalid-extends.voom`
- Create: `crates/voom-policy/fixtures/policies/invalid-extend-phase.voom`
- Create: `crates/voom-policy/fixtures/policies/invalid-unknown-core-field.voom`
- Create: `crates/voom-policy/fixtures/compiled/minimal.json`
- Create: `crates/voom-policy/fixtures/compiled/container-metadata.json`
- Create: `crates/voom-policy/fixtures/compiled/production-normalize-reduced.json`
- Create: `crates/voom-policy/fixtures/diagnostics/invalid-deferred-transcode.json`
- Create: `crates/voom-policy/fixtures/diagnostics/invalid-extends.json`
- Create: `crates/voom-policy/fixtures/diagnostics/invalid-extend-phase.json`
- Create: `crates/voom-policy/fixtures/diagnostics/invalid-unknown-core-field.json`

- [ ] **Step 1: Write fixture loader tests**

Create tests that load each valid fixture, compile it, and compare to the checked-in JSON projection:

```rust
use super::*;

#[test]
fn valid_policy_fixtures_match_compiled_goldens() {
    for fixture in valid_policy_fixtures() {
        let source = load_policy_fixture(fixture.source_path).unwrap();
        let compiled = crate::compile_policy(&source).unwrap();
        let actual = crate::deterministic_json(&compiled.policy).unwrap();
        let expected = load_json_fixture(fixture.expected_json_path).unwrap();
        assert_eq!(actual, expected, "fixture {}", fixture.source_path);
    }
}

#[test]
fn invalid_policy_fixtures_match_diagnostic_goldens() {
    for fixture in invalid_policy_fixtures() {
        let source = load_policy_fixture(fixture.source_path).unwrap();
        let err = crate::compile_policy(&source).unwrap_err();
        let actual = serde_json::to_value(&err.diagnostics).unwrap();
        let expected = load_json_fixture(fixture.expected_json_path).unwrap();
        assert_eq!(actual, expected, "fixture {}", fixture.source_path);
    }
}
```

- [ ] **Step 2: Add valid source fixtures**

Use accepted Sprint 4 syntax only. `production-normalize-reduced.voom` must omit `transcode`, `synthesize`, and `verify`.

- [ ] **Step 3: Add invalid source fixtures**

Create the four listed invalid fixtures so golden tests cover deferred execution operation, deferred composition, unknown core field root, and phase inheritance through `extend`.

- [ ] **Step 4: Generate and review goldens**

Run a focused helper command:

`cargo test -p voom-policy policy_fixtures -- --nocapture`

Implement the fixture test so missing or mismatched expected JSON prints the actual deterministic JSON with the fixture path before panicking. Create the golden files from that output and rerun. Do not check in placeholders or hand-wavy expected files.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p voom-policy`

Expected: PASS.

Commit:

```bash
git add crates/voom-policy/src/lib.rs crates/voom-policy/src/policy_fixtures.rs crates/voom-policy/src/policy_fixtures_test.rs crates/voom-policy/fixtures
git commit -m "test: add policy language golden fixtures"
```

## Task 7: Policy Registry Migration

**Files:**
- Create: `migrations/0007_policy_registry.sql`
- Modify: `crates/voom-store/src/migrator.rs`
- Modify: `crates/voom-store/tests/migration_inventory.rs`

- [ ] **Step 1: Write migration SQL**

Create:

```sql
-- Sprint 4 - Durable policy documents and immutable policy versions.

CREATE TABLE policy_documents (
    id                          INTEGER PRIMARY KEY,
    slug                        TEXT NOT NULL UNIQUE,
    display_name                TEXT NOT NULL,
    created_at                  TEXT NOT NULL,
    current_accepted_version_id INTEGER,
    epoch                       INTEGER NOT NULL DEFAULT 0,
    CHECK (length(trim(slug)) > 0),
    CHECK (length(trim(display_name)) > 0),
    CHECK (epoch >= 0)
);

CREATE TABLE policy_versions (
    id                 INTEGER PRIMARY KEY,
    policy_document_id INTEGER NOT NULL REFERENCES policy_documents(id) ON DELETE RESTRICT,
    version_number     INTEGER NOT NULL,
    source_text        TEXT NOT NULL,
    source_hash        TEXT NOT NULL,
    schema_version     INTEGER NOT NULL,
    compiled_json      TEXT NOT NULL CHECK (json_valid(compiled_json)),
    created_at         TEXT NOT NULL,
    CHECK (version_number > 0),
    CHECK (length(source_hash) = 64),
    CHECK (schema_version > 0),
    UNIQUE (policy_document_id, version_number),
    UNIQUE (policy_document_id, source_hash),
    UNIQUE (policy_document_id, id)
);

CREATE INDEX policy_documents_by_slug
    ON policy_documents (slug);

CREATE INDEX policy_versions_by_document
    ON policy_versions (policy_document_id, version_number);

CREATE TRIGGER policy_documents_current_version_same_document_insert
BEFORE INSERT ON policy_documents
WHEN NEW.current_accepted_version_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'policy current version must belong to document')
    WHERE NOT EXISTS (
        SELECT 1 FROM policy_versions
        WHERE id = NEW.current_accepted_version_id
          AND policy_document_id = NEW.id
    );
END;

CREATE TRIGGER policy_documents_current_version_same_document_update
BEFORE UPDATE OF current_accepted_version_id ON policy_documents
WHEN NEW.current_accepted_version_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'policy current version must belong to document')
    WHERE NOT EXISTS (
        SELECT 1 FROM policy_versions
        WHERE id = NEW.current_accepted_version_id
          AND policy_document_id = NEW.id
    );
END;

CREATE TRIGGER policy_versions_are_immutable
BEFORE UPDATE ON policy_versions
BEGIN
    SELECT RAISE(ABORT, 'policy versions are immutable');
END;

CREATE TRIGGER policy_versions_are_not_deleted
BEFORE DELETE ON policy_versions
BEGIN
    SELECT RAISE(ABORT, 'policy versions are immutable');
END;
```

- [ ] **Step 2: Register migration 0007**

Add `MIGRATION_0007_SQL` include and a `Migration::new(7, Cow::Borrowed("policy_registry"), ...)` entry after migration 0006 in `migrator.rs`.

- [ ] **Step 3: Run migration inventory**

Run: `cargo test -p voom-store --test migration_inventory`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add migrations/0007_policy_registry.sql crates/voom-store/src/migrator.rs crates/voom-store/tests/migration_inventory.rs
git commit -m "feat: add policy registry migration"
```

## Task 8: Policy Registry Repository

**Files:**
- Modify: `crates/voom-store/src/repo/mod.rs`
- Create: `crates/voom-store/src/repo/policies.rs`
- Create: `crates/voom-store/src/repo/policies_test.rs`

- [ ] **Step 1: Write repository tests**

Create tests for create/get/list/add-version/dedup/current-version:

```rust
#[tokio::test]
async fn create_document_with_first_version_round_trips() {
    let repo = repo().await;
    let draft = draft("production-normalize", "policy \"production-normalize\" { phase a {} }");
    let created = repo.create_document_with_version(draft).await.unwrap();
    assert_eq!(created.document.slug, "production-normalize");
    assert_eq!(created.version.version_number, 1);
    assert_eq!(created.document.current_accepted_version_id, Some(created.version.id));
}

#[tokio::test]
async fn duplicate_source_returns_existing_version() {
    let repo = repo().await;
    let draft = draft("same", "policy \"same\" { phase a {} }");
    let first = repo.create_document_with_version(draft.clone()).await.unwrap();
    let second = repo.add_version(first.document.id, draft.source_text).await.unwrap();
    assert_eq!(second.id, first.version.id);
    assert_eq!(second.version_number, 1);
}

#[tokio::test]
async fn cross_document_current_version_is_rejected() {
    let pool = pool().await;
    let repo = SqlitePolicyRepo::new(pool.clone());
    let a = repo.create_document_with_version(draft("a", "policy \"a\" { phase a {} }")).await.unwrap();
    let b = repo.create_document_with_version(draft("b", "policy \"b\" { phase b {} }")).await.unwrap();
    let err = sqlx::query("UPDATE policy_documents SET current_accepted_version_id = ? WHERE id = ?")
        .bind(i64::try_from(a.version.id.0).unwrap())
        .bind(i64::try_from(b.document.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("policy current version must belong to document"));
}

#[tokio::test]
async fn concurrent_add_version_has_one_winner() {
    let pool = pool().await;
    let repo_a = SqlitePolicyRepo::new(pool.clone());
    let repo_b = SqlitePolicyRepo::new(pool.clone());
    let created = repo_a
        .create_document_with_version(draft("race", "policy \"race\" { phase a {} }"))
        .await
        .unwrap();

    let source = "policy \"race\" { phase a {} phase b { depends_on: [a] } }";
    let (left, right) = tokio::join!(
        repo_a.add_version(created.document.id, source.to_owned()),
        repo_b.add_version(created.document.id, source.to_owned())
    );

    assert!(
        left.is_ok() || right.is_ok(),
        "at least one concurrent writer should create or observe version 2"
    );
    let versions = repo_a.list_versions(created.document.id).await.unwrap();
    assert_eq!(versions.iter().map(|v| v.version_number).collect::<Vec<_>>(), [1, 2]);
    let version2 = versions.last().unwrap();
    for result in [&left, &right] {
        match result {
            Ok(returned) => assert_eq!(returned.id, version2.id),
            Err(err) => assert_eq!(err.code(), "CONFLICT"),
        }
    }
}
```

- [ ] **Step 2: Implement repository types**

Expose:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDocument {
    pub id: voom_core::PolicyDocumentId,
    pub slug: String,
    pub display_name: String,
    pub created_at: time::OffsetDateTime,
    pub current_accepted_version_id: Option<voom_core::PolicyVersionId>,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyVersion {
    pub id: voom_core::PolicyVersionId,
    pub policy_document_id: voom_core::PolicyDocumentId,
    pub version_number: u64,
    pub source_text: String,
    pub source_hash: String,
    pub schema_version: u32,
    pub compiled_json: serde_json::Value,
    pub created_at: time::OffsetDateTime,
}
```

Add the remaining repository contract types:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct NewPolicyDocumentVersion {
    pub slug: String,
    pub display_name: Option<String>,
    pub source_text: String,
    pub created_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedPolicyVersion {
    pub document: PolicyDocument,
    pub version: PolicyVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDocumentSummary {
    pub id: voom_core::PolicyDocumentId,
    pub slug: String,
    pub display_name: String,
    pub created_at: time::OffsetDateTime,
    pub current_accepted_version_id: Option<voom_core::PolicyVersionId>,
    pub epoch: u64,
}
```

Define the trait and SQLite implementation shell:

```rust
#[async_trait::async_trait]
pub trait PolicyRepo: super::Repository {
    async fn create_document_with_version(
        &self,
        draft: NewPolicyDocumentVersion,
    ) -> Result<CreatedPolicyVersion, voom_core::VoomError>;

    async fn add_version(
        &self,
        document_id: voom_core::PolicyDocumentId,
        source_text: String,
    ) -> Result<PolicyVersion, voom_core::VoomError>;

    async fn get_document(
        &self,
        id: voom_core::PolicyDocumentId,
    ) -> Result<Option<PolicyDocument>, voom_core::VoomError>;

    async fn list_documents(&self) -> Result<Vec<PolicyDocumentSummary>, voom_core::VoomError>;

    async fn get_version(
        &self,
        id: voom_core::PolicyVersionId,
    ) -> Result<Option<PolicyVersion>, voom_core::VoomError>;

    async fn list_versions(
        &self,
        document_id: voom_core::PolicyDocumentId,
    ) -> Result<Vec<PolicyVersion>, voom_core::VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqlitePolicyRepo {
    pool: sqlx::SqlitePool,
}
```

- [ ] **Step 3: Implement create/add-version transaction**

`create_document_with_version` must:

1. validate stable slug with the same character rule as Sprint 3;
2. open the transaction with `pool.begin_with("BEGIN IMMEDIATE")`;
3. insert `policy_documents`;
4. insert version number 1;
5. update `current_accepted_version_id` and increment epoch;
6. commit once.

`add_version` must:

1. compute source hash before parser normalization;
2. compile source through `voom_policy::compile_policy`;
3. return existing version if `(policy_document_id, source_hash)` already exists;
4. otherwise open the transaction with `pool.begin_with("BEGIN IMMEDIATE")`;
5. select `MAX(version_number) + 1` inside the transaction;
6. insert immutable version;
7. update current version and epoch;
8. map SQLite unique violations on `(policy_document_id, version_number)` and `(policy_document_id, source_hash)` to `VoomError::Conflict` after re-reading by source hash when possible.

- [ ] **Step 4: Export and run tests**

Add `pub mod policies;` and public re-exports in `repo/mod.rs`.

Run: `cargo test -p voom-store policies`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-store/src/repo/mod.rs crates/voom-store/src/repo/policies.rs crates/voom-store/src/repo/policies_test.rs
git commit -m "feat: persist accepted policy versions"
```

## Task 9: Identity Accepted Policy Guard

**Files:**
- Modify: `crates/voom-store/src/repo/identity.rs`
- Modify: `crates/voom-store/src/repo/identity_test.rs`

- [ ] **Step 1: Write failing guard test**

Add a test that creates a `policy_input_sets` row, records an identity evidence row, then attempts to accept the evidence through the new policy-stamped repository method using `PolicyVersionId(policy_input_set_id.0)`. Expected error code is `POLICY_VALIDATION_ERROR`, and a follow-up `get_identity_evidence` assertion must prove `accepted_policy_id` and `accepted_at` remain `None`.

- [ ] **Step 2: Add typed acceptance input**

Extend the public accepted-pin model with a typed policy version id:

```rust
#[derive(Debug, Clone, Default)]
pub struct AcceptedPin {
    pub file_version_ids: Option<JsonValue>,
    pub hashes: Option<JsonValue>,
    pub locations: Option<JsonValue>,
    pub policy_version_id: Option<voom_core::PolicyVersionId>,
}
```

This keeps the existing `accept_identity_evidence_in_tx` signature stable while making the only policy-stamped input type-safe.

Update every existing named `AcceptedPin { ... }` literal in repository, control-plane, and integration tests to include `..AcceptedPin::default()` so adding `policy_version_id` does not break unrelated tests.

- [ ] **Step 3: Add policy-version existence guard**

Do not widen existing acceptance paths to accept raw `u64`. Add a helper that validates accepted policy references:

```rust
async fn ensure_policy_version_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: voom_core::PolicyVersionId,
) -> Result<(), VoomError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM policy_versions WHERE id = ?")
        .bind(super::common::i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("policy_versions existence check: {e}")))?;
    if exists.is_none() {
        return Err(VoomError::PolicyValidationError(format!(
            "accepted_policy_id {id} is not a policy version"
        )));
    }
    Ok(())
}
```

- [ ] **Step 4: Write `accepted_policy_id` atomically**

Inside `accept_identity_evidence_in_tx`, before the update, call `ensure_policy_version_exists` when `pinned.policy_version_id` is `Some`. Then include `accepted_policy_id = ?` in the same `UPDATE identity_evidence` statement that writes `accepted_at`, `accepted_user_id`, and pinned JSON:

```rust
let accepted_policy_id = if let Some(id) = pinned.policy_version_id {
    ensure_policy_version_exists(tx, id).await?;
    Some(super::common::i64_from_u64(id.0))
} else {
    None
};
```

Bind `accepted_policy_id` into the update. The validation query and update must run inside the caller's transaction so a failed policy-version check leaves the evidence row untouched.

- [ ] **Step 5: Add positive policy-version acceptance test**

After Task 8 exists, add a repository test that creates a real policy version and accepts evidence with `AcceptedPin { policy_version_id: Some(version.id), ..AcceptedPin::default() }`. Assert the returned and reloaded evidence rows have `accepted_policy_id == Some(version.id.0)`.

- [ ] **Step 6: Run tests and commit**

Run: `cargo test -p voom-store identity accepted_policy`

Expected: PASS.

Commit:

```bash
git add crates/voom-store/src/repo/identity.rs crates/voom-store/src/repo/identity_test.rs
git commit -m "fix: guard accepted policy evidence ids"
```

## Task 10: Control-Plane Policy Use Cases

**Files:**
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Create: `crates/voom-control-plane/src/cases/policies.rs`
- Create: `crates/voom-control-plane/src/cases/policies_test.rs`

- [ ] **Step 1: Write use-case tests**

Create tests:

```rust
use super::cp;

#[tokio::test]
async fn compile_policy_source_without_persisting() {
    let (cp, _tmp) = cp().await;
    let out = cp.compile_policy_source("policy \"p\" { phase a {} }").await.unwrap();
    assert_eq!(out.policy.policy_name, "p");
    assert!(cp.list_policy_documents().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_and_add_policy_versions() {
    let (cp, _tmp) = cp().await;
    let created = cp
        .create_policy_document("p", "policy \"p\" { phase a {} }")
        .await
        .unwrap();
    let version2 = cp
        .add_policy_version(created.document.id, "policy \"p\" { phase a {} phase b { depends_on: [a] } }")
        .await
        .unwrap();
    assert_eq!(version2.version_number, 2);
    assert_eq!(cp.get_policy_document(created.document.id).await.unwrap().unwrap().current_accepted_version_id, Some(version2.id));
}
```

- [ ] **Step 2: Add repo field**

In `ControlPlane`, add:

```rust
pub(crate) policies: SqlitePolicyRepo,
```

Initialize it in `new_unchecked` with `SqlitePolicyRepo::new(pool.clone())`, include it in `Debug`, and import it from `voom_store::repo::policies`.

- [ ] **Step 3: Implement use cases**

`cases/policies.rs` should expose:

```rust
impl ControlPlane {
    pub async fn compile_policy_source(&self, source: &str) -> Result<voom_policy::CompileOutput, voom_policy::PolicyCompileError>;
    pub async fn create_policy_document(&self, slug: &str, source: &str) -> Result<CreatedPolicyVersion, VoomError>;
    pub async fn add_policy_version(&self, document_id: PolicyDocumentId, source: &str) -> Result<PolicyVersion, VoomError>;
    pub async fn get_policy_document(&self, id: PolicyDocumentId) -> Result<Option<PolicyDocument>, VoomError>;
    pub async fn list_policy_documents(&self) -> Result<Vec<PolicyDocumentSummary>, VoomError>;
    pub async fn get_policy_version(&self, id: PolicyVersionId) -> Result<Option<PolicyVersion>, VoomError>;
    pub async fn list_policy_versions(&self, document_id: PolicyDocumentId) -> Result<Vec<PolicyVersion>, VoomError>;
}
```

No events are emitted in Sprint 4.

- [ ] **Step 4: Export module and run tests**

Add `pub mod policies;` in `cases/mod.rs`.

Run: `cargo test -p voom-control-plane policies`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/cases/mod.rs crates/voom-control-plane/src/cases/policies.rs crates/voom-control-plane/src/cases/policies_test.rs
git commit -m "feat: add policy control-plane use cases"
```

## Task 11: Closeout Verification

**Files:**
- Check: `docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md`

- [ ] **Step 1: Run focused test suite**

Run:

```bash
cargo test -p voom-core policy_
cargo test -p voom-policy
cargo test -p voom-store policies
cargo test -p voom-store identity accepted_policy
cargo test -p voom-control-plane policies
cargo test -p voom-store --test migration_inventory
just check-test-layout
```

Expected: all commands PASS. No skipped command may be reported as passing.

- [ ] **Step 2: Run full CI**

Run: `just ci`

Expected: PASS.

- [ ] **Step 3: Scan for incomplete-work markers**

Run:

```bash
rg -n "TODO|TBD|FIXME|todo!|unimplemented!" crates/voom-policy crates/voom-store/src/repo/policies.rs crates/voom-control-plane/src/cases/policies.rs docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md
```

Expected: no output.

- [ ] **Step 4: Update design closeout only for real deferrals**

If implementation discovers a legitimate deferral, add one concise closeout note to the Sprint 4 design with the exact deferral and reason. Do not add status narration for work that shipped as designed.

- [ ] **Step 5: Commit final verification notes if any**

If the design doc changed:

```bash
git add docs/superpowers/specs/2026-05-22-voom-sprint-4-design.md
git commit -m "docs: close out sprint 4 policy design"
```

If the design doc did not change, do not create an empty commit.

## Self-Review Checklist

- Spec coverage: Tasks 1-5 cover ids, parser, diagnostics, validator, compiled IR, source hashing, and deterministic projections. Task 6 covers valid/invalid fixture acceptance. Tasks 7-8 cover migration and durable registry invariants. Task 9 covers accepted policy id separation. Task 10 covers control-plane use cases. Task 11 covers CI and incomplete marker scan.
- Scope control: No CLI, API, worker payload, planner, scheduler, UI, remote loading, or event vocabulary is included.
- Architecture: `voom-policy` has no database dependency; `voom-store` owns SQLite; `voom-control-plane` composes use cases.
- Test layout: Every unit test file is a sibling `*_test.rs` linked by `#[path]`.
- Execution safety: Each task has a focused test command before its commit and `just ci` is the final gate.
