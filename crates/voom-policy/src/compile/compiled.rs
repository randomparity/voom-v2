use std::collections::BTreeMap;

use crate::text::{
    comparison_rhs, dependency_values, is_single_value, list_values, quoted_value, setting_value,
    split_bool_expression, statement_text, strip_outer_group, text_after_list,
    text_after_quoted_value, title_filter_value, words,
};
use crate::{
    DiagnosticCode, DiagnosticStage, ExprAst, PolicyAst, PolicyDiagnostic, SourceSpan,
    StatementAst, line_column,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledPolicy {
    pub policy_name: String,
    pub slug: String,
    pub source_hash: String,
    pub schema_version: u32,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub config: BTreeMap<String, serde_json::Value>,
    pub phases: Vec<CompiledPhase>,
    pub phase_order: Vec<String>,
    pub warnings: Vec<PolicyDiagnostic>,
    pub provenance: PolicyProvenance,
}

impl CompiledPolicy {
    #[cfg(test)]
    #[must_use]
    pub fn minimal_for_test(policy_name: &str, source_hash: &str) -> Self {
        Self {
            policy_name: policy_name.to_owned(),
            slug: slug(policy_name),
            source_hash: source_hash.to_owned(),
            schema_version: 2,
            metadata: BTreeMap::new(),
            config: BTreeMap::new(),
            phases: Vec::new(),
            phase_order: Vec::new(),
            warnings: Vec::new(),
            provenance: PolicyProvenance::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyProvenance {
    pub compiler: String,
    pub format: String,
    pub flags: BTreeMap<String, serde_json::Value>,
}

impl Default for PolicyProvenance {
    fn default() -> Self {
        Self {
            compiler: "voom-policy".to_owned(),
            format: "sprint4-v2".to_owned(),
            flags: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledPhase {
    pub name: String,
    pub depends_on: Vec<String>,
    pub run_if: Option<CompiledCondition>,
    pub skip_if: Option<CompiledCondition>,
    pub on_error: Option<ErrorStrategy>,
    pub operations: Vec<CompiledOperation>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "TranscodeVideo.resolved_profile is a pinned cross-phase contract field (Phase 6 fills it in-memory); boxing would diverge from the Sprint 15 plan's typed signature"
)]
pub enum CompiledOperation {
    SetContainer {
        container: String,
    },
    KeepTracks {
        target: TrackTarget,
        filter: Option<TrackFilter>,
    },
    RemoveTracks {
        target: TrackTarget,
        filter: Option<TrackFilter>,
    },
    ReorderTracks {
        targets: Vec<TrackTarget>,
    },
    SetDefaults {
        target: TrackTarget,
        strategy: DefaultStrategy,
    },
    ClearTrackActions {
        target: TrackTarget,
    },
    ClearTags,
    SetTag {
        key: String,
        value: CompiledValue,
    },
    DeleteTag {
        key: String,
    },
    TranscodeVideo {
        target_codec: String,
        container: String,
        profile: crate::VideoProfileRef,
        /// Populated in-memory by the control plane's resolution step
        /// (Phase 6) before planning; never written to `compiled_json`
        /// (skipped when `None`, defaults to `None` on read) so stored
        /// rows and `source_hash` are unaffected and legacy bare-string
        /// policies still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_profile: Option<voom_core::TranscodeVideoProfile>,
    },
    TranscodeAudio {
        target_codec: String,
        container: String,
        filter: Option<TrackFilter>,
    },
    ExtractAudio {
        target_codec: String,
        container: String,
        filter: Option<TrackFilter>,
    },
    Conditional {
        condition: CompiledCondition,
        operations: Vec<CompiledOperation>,
    },
    Rules {
        mode: RuleMatchMode,
        rules: Vec<CompiledRule>,
    },
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
    Channels { op: ComparisonOp, value: u64 },
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
    Exists {
        target: TrackTarget,
        filter: Option<TrackFilter>,
    },
    Count {
        target: TrackTarget,
        op: ComparisonOp,
        value: u64,
    },
    FieldComparison {
        path: Vec<String>,
        op: ComparisonOp,
        value: CompiledValue,
    },
    FieldExists {
        path: Vec<String>,
    },
    Predicate {
        name: String,
    },
    Not {
        inner: Box<CompiledCondition>,
    },
    And {
        conditions: Vec<CompiledCondition>,
    },
    Or {
        conditions: Vec<CompiledCondition>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledRule {
    pub name: String,
    pub condition: Option<CompiledCondition>,
    pub operations: Vec<CompiledOperation>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledValue {
    String { value: String },
    Number { value: String },
    Boolean { value: bool },
    FieldPath { path: Vec<String> },
    List { values: Vec<CompiledValue> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    Matches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultStrategy {
    First,
    Best,
    None,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatchMode {
    First,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorStrategy {
    Abort,
    Continue,
    Skip,
}

#[must_use]
pub fn source_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

pub fn deterministic_json(
    policy: &CompiledPolicy,
) -> Result<serde_json::Value, voom_core::VoomError> {
    serde_json::to_value(policy)
        .map_err(|e| voom_core::VoomError::Internal(format!("compiled policy serialize: {e}")))
}

pub(crate) fn compile_ast(
    source: &str,
    ast: &PolicyAst,
    warnings: Vec<PolicyDiagnostic>,
) -> Result<CompiledPolicy, Vec<PolicyDiagnostic>> {
    let mut phases = Vec::with_capacity(ast.phases.len());
    for phase in &ast.phases {
        phases.push(CompiledPhase {
            name: phase.name.value.clone(),
            depends_on: phase_dependencies(&phase.controls),
            run_if: phase_run_if(&phase.controls),
            skip_if: phase_skip_if(&phase.controls),
            on_error: phase_on_error(&phase.controls),
            operations: lower_operations(source, &phase.operations)?,
        });
    }
    Ok(CompiledPolicy {
        policy_name: ast.name.value.clone(),
        slug: slug(&ast.name.value),
        source_hash: source_hash(source),
        schema_version: 2,
        metadata: metadata_map(&ast.metadata),
        config: config_map(&ast.config),
        phase_order: phase_order(ast),
        phases,
        warnings,
        provenance: PolicyProvenance::default(),
    })
}

fn lower_operations(
    source: &str,
    statements: &[StatementAst],
) -> Result<Vec<CompiledOperation>, Vec<PolicyDiagnostic>> {
    let mut operations = Vec::with_capacity(statements.len());
    for statement in statements {
        operations.push(lower_operation(source, statement)?);
    }
    Ok(operations)
}

fn lower_operation(
    source: &str,
    statement: &StatementAst,
) -> Result<CompiledOperation, Vec<PolicyDiagnostic>> {
    if let StatementAst::TranscodeInline {
        header, settings, ..
    } = statement
    {
        return Ok(lower_transcode_inline(header, settings));
    }
    let text = statement_text(statement);
    let tokens = words(text.as_ref());
    let Some(keyword) = tokens.first().copied() else {
        return Err(vec![unknown_operation(source, statement.span())]);
    };
    match keyword {
        "container" => Ok(CompiledOperation::SetContainer {
            container: token_string(&tokens, 1, "mkv"),
        }),
        "keep" => Ok(CompiledOperation::KeepTracks {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            filter: track_filter(text.as_ref()),
        }),
        "remove" => Ok(CompiledOperation::RemoveTracks {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            filter: track_filter(text.as_ref()),
        }),
        "order" if tokens.get(1).copied() == Some("tracks") => {
            Ok(CompiledOperation::ReorderTracks {
                targets: list_values(text.as_ref())
                    .into_iter()
                    .filter_map(|target| track_target(Some(target)))
                    .collect(),
            })
        }
        "defaults" => Ok(CompiledOperation::SetDefaults {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            strategy: default_strategy(tokens.get(2).copied()).unwrap_or(DefaultStrategy::First),
        }),
        "actions" if tokens.get(2).copied() == Some("clear") => {
            Ok(CompiledOperation::ClearTrackActions {
                target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            })
        }
        "clear_tags" => Ok(CompiledOperation::ClearTags),
        "set_tag" => Ok(CompiledOperation::SetTag {
            key: quoted_value(text.as_ref()).unwrap_or_default(),
            value: compiled_value(text_after_quoted_value(text.as_ref()).unwrap_or_default()),
        }),
        "delete_tag" => Ok(CompiledOperation::DeleteTag {
            key: quoted_value(text.as_ref()).unwrap_or_default(),
        }),
        "transcode" if tokens.get(1).copied() == Some("audio") => {
            Ok(CompiledOperation::TranscodeAudio {
                target_codec: token_string(&tokens, 3, "opus"),
                container: "mkv".to_owned(),
                filter: track_filter(text.as_ref()),
            })
        }
        "transcode" => Ok(lower_transcode_raw(&tokens)),
        "extract" if tokens.get(1).copied() == Some("audio") => {
            Ok(CompiledOperation::ExtractAudio {
                target_codec: "opus".to_owned(),
                container: "ogg".to_owned(),
                filter: track_filter(text.as_ref()),
            })
        }
        "when" => Ok(CompiledOperation::Conditional {
            condition: condition_from_text(text.as_ref().trim_start_matches("when").trim()),
            operations: match statement {
                StatementAst::Block { statements, .. } => lower_operations(source, statements)?,
                StatementAst::Raw { .. } | StatementAst::TranscodeInline { .. } => Vec::new(),
            },
        }),
        "rules" => Ok(CompiledOperation::Rules {
            mode: rule_match_mode(tokens.get(1).copied()).unwrap_or(RuleMatchMode::First),
            rules: lower_rules(source, statement)?,
        }),
        _ => Err(vec![unknown_operation(source, statement.span())]),
    }
}

fn lower_transcode_raw(tokens: &[&str]) -> CompiledOperation {
    let codec = tokens.get(3).copied().unwrap_or("hevc").to_owned();
    let profile = match tokens.get(4..) {
        Some(["using", "profile", name]) => crate::VideoProfileRef::Named(strip_quotes(name)),
        _ => crate::VideoProfileRef::Named(format!("default-{codec}")),
    };
    CompiledOperation::TranscodeVideo {
        target_codec: codec,
        container: "mkv".to_owned(),
        profile,
        resolved_profile: None,
    }
}

fn lower_transcode_inline(header: &str, settings: &[crate::SettingAst]) -> CompiledOperation {
    let tokens = words(header);
    let codec = tokens.get(3).copied().unwrap_or("hevc").to_owned();
    let inline = inline_settings_from(settings);
    let container = inline
        .output_container
        .clone()
        .unwrap_or_else(|| "mkv".to_owned());
    CompiledOperation::TranscodeVideo {
        target_codec: codec,
        container,
        profile: crate::VideoProfileRef::Inline(inline),
        resolved_profile: None,
    }
}

fn inline_settings_from(settings: &[crate::SettingAst]) -> crate::VideoProfileSettings {
    let mut by_key = BTreeMap::new();
    for setting in settings {
        by_key.insert(setting.key.value.as_str(), &setting.value);
    }
    let str_at = |key: &str| by_key.get(key).map(|expr| expr_scalar_string(expr));
    let u32_at = |key: &str| str_at(key).and_then(|value| value.parse::<u32>().ok());
    crate::VideoProfileSettings {
        encoder: str_at("encoder").unwrap_or_default(),
        crf: str_at("crf")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default(),
        preset: str_at("preset").unwrap_or_default(),
        tune: str_at("tune"),
        codec_profile: str_at("codec_profile"),
        codec_level: str_at("codec_level"),
        pixel_format: str_at("pixel_format"),
        max_width: u32_at("max_width"),
        max_height: u32_at("max_height"),
        output_container: str_at("output_container"),
        copy_compatible: by_key.get("copy_compatible").and_then(|expr| match expr {
            ExprAst::Boolean(value) => Some(value.value),
            _ => None,
        }),
    }
}

fn expr_scalar_string(expr: &ExprAst) -> String {
    match expr {
        ExprAst::String(value)
        | ExprAst::Identifier(value)
        | ExprAst::Number(value)
        | ExprAst::FieldPath(value) => value.value.clone(),
        ExprAst::Boolean(value) => value.value.to_string(),
        ExprAst::List { .. } => String::new(),
    }
}

fn lower_rules(
    source: &str,
    statement: &StatementAst,
) -> Result<Vec<CompiledRule>, Vec<PolicyDiagnostic>> {
    let StatementAst::Block { statements, .. } = statement else {
        return Ok(Vec::new());
    };
    let mut rules = Vec::with_capacity(statements.len());
    for rule in statements {
        let StatementAst::Block {
            name, statements, ..
        } = rule
        else {
            return Err(vec![unknown_operation(source, rule.span())]);
        };
        let mut condition = None;
        let mut operations = Vec::new();
        for nested in statements {
            if nested.keyword().value == "when" {
                let text = statement_text(nested);
                condition = Some(condition_from_text(text.trim_start_matches("when").trim()));
                if let StatementAst::Block { statements, .. } = nested {
                    operations.extend(lower_operations(source, statements)?);
                }
            } else {
                operations.push(lower_operation(source, nested)?);
            }
        }
        rules.push(CompiledRule {
            name: name
                .as_ref()
                .map_or_else(String::new, |name| strip_quotes(&name.value)),
            condition,
            operations,
        });
    }
    Ok(rules)
}

fn metadata_map(settings: &[crate::SettingAst]) -> BTreeMap<String, serde_json::Value> {
    settings
        .iter()
        .map(|setting| (setting.key.value.clone(), expr_json(&setting.value)))
        .collect()
}

fn config_map(statements: &[StatementAst]) -> BTreeMap<String, serde_json::Value> {
    statements
        .iter()
        .map(|statement| {
            let text = statement_text(statement);
            (
                statement.keyword().value.clone(),
                serde_json::Value::String(text.into_owned()),
            )
        })
        .collect()
}

fn expr_json(expr: &ExprAst) -> serde_json::Value {
    match expr {
        ExprAst::String(value) | ExprAst::Identifier(value) | ExprAst::Number(value) => {
            serde_json::Value::String(value.value.clone())
        }
        ExprAst::Boolean(value) => serde_json::Value::Bool(value.value),
        ExprAst::FieldPath(value) => serde_json::Value::String(value.value.clone()),
        ExprAst::List { values, .. } => {
            serde_json::Value::Array(values.iter().map(expr_json).collect())
        }
    }
}

fn phase_dependencies(controls: &[StatementAst]) -> Vec<String> {
    controls
        .iter()
        .filter(|control| control.keyword().value == "depends_on")
        .flat_map(|control| {
            let text = statement_text(control);
            dependency_values(text.as_ref())
        })
        .collect()
}

fn phase_order(ast: &PolicyAst) -> Vec<String> {
    let dependencies_by_phase = ast
        .phases
        .iter()
        .map(|phase| {
            (
                phase.name.value.as_str(),
                phase_dependencies(&phase.controls),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut visited = std::collections::BTreeSet::new();
    let mut order = Vec::with_capacity(ast.phases.len());

    for phase in &ast.phases {
        visit_phase(
            phase.name.value.as_str(),
            &dependencies_by_phase,
            &mut visited,
            &mut order,
        );
    }

    order
}

fn visit_phase(
    name: &str,
    dependencies_by_phase: &BTreeMap<&str, Vec<String>>,
    visited: &mut std::collections::BTreeSet<String>,
    order: &mut Vec<String>,
) {
    if !visited.insert(name.to_owned()) {
        return;
    }
    if let Some(dependencies) = dependencies_by_phase.get(name) {
        for dependency in dependencies {
            if dependencies_by_phase.contains_key(dependency.as_str()) {
                visit_phase(dependency, dependencies_by_phase, visited, order);
            }
        }
    }
    order.push(name.to_owned());
}

fn phase_run_if(controls: &[StatementAst]) -> Option<CompiledCondition> {
    controls
        .iter()
        .find(|control| control.keyword().value == "run_if")
        .map(|control| {
            let text = statement_text(control);
            CompiledCondition::Predicate {
                name: text.trim_start_matches("run_if").trim().to_owned(),
            }
        })
}

fn phase_skip_if(controls: &[StatementAst]) -> Option<CompiledCondition> {
    controls
        .iter()
        .find(|control| control.keyword().value == "skip")
        .map(|control| {
            let text = statement_text(control);
            condition_from_text(
                text.trim_start_matches("skip")
                    .trim_start_matches("when")
                    .trim(),
            )
        })
}

fn phase_on_error(controls: &[StatementAst]) -> Option<ErrorStrategy> {
    controls
        .iter()
        .find(|control| control.keyword().value == "on_error")
        .and_then(|control| {
            let text = statement_text(control);
            error_strategy(setting_value(text.as_ref()))
        })
}

fn condition_from_text(text: &str) -> CompiledCondition {
    let text = strip_outer_group(text.trim());
    if let Some(parts) = split_bool_condition(text, " or ") {
        return CompiledCondition::Or {
            conditions: parts.into_iter().map(condition_from_text).collect(),
        };
    }
    if let Some(parts) = split_bool_condition(text, " and ") {
        return CompiledCondition::And {
            conditions: parts.into_iter().map(condition_from_text).collect(),
        };
    }
    let tokens = words(text);
    if tokens.first() == Some(&"not") {
        return CompiledCondition::Not {
            inner: Box::new(condition_from_text(text.trim_start_matches("not").trim())),
        };
    }
    if tokens.first() == Some(&"exists") {
        return CompiledCondition::Exists {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            filter: track_filter(text),
        };
    }
    if tokens.first() == Some(&"count") {
        return CompiledCondition::Count {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            op: comparison_op(tokens.get(2).copied()).unwrap_or(ComparisonOp::Eq),
            value: tokens
                .get(3)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default(),
        };
    }
    if let Some(index) = tokens
        .iter()
        .position(|token| comparison_op(Some(token)).is_some())
    {
        let path = tokens
            .first()
            .map_or_else(Vec::new, |path| field_path_segments(path));
        let op = comparison_op(tokens.get(index).copied()).unwrap_or(ComparisonOp::Eq);
        let value = comparison_rhs(text, tokens[index]).map_or_else(
            || compiled_value(tokens.get(index + 1).copied().unwrap_or_default()),
            compiled_value,
        );
        return CompiledCondition::FieldComparison { path, op, value };
    }
    if let Some(path) = tokens.first().filter(|token| token.contains('.')) {
        return CompiledCondition::FieldExists {
            path: field_path_segments(path),
        };
    }
    CompiledCondition::Predicate {
        name: text.to_owned(),
    }
}

fn split_bool_condition<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    if text.contains(" where ") {
        return None;
    }
    split_bool_expression(text, delimiter)
}

fn track_filter(text: &str) -> Option<TrackFilter> {
    let where_text = text
        .split_once(" where ")
        .map(|(_, filter)| filter.trim())?;
    filter_from_text(where_text)
}

fn filter_from_text(text: &str) -> Option<TrackFilter> {
    let text = strip_outer_group(text.trim());
    if let Some(parts) = split_bool_filter(text, " or ") {
        let filters = parts
            .into_iter()
            .map(filter_from_text)
            .collect::<Option<Vec<_>>>()?;
        return Some(TrackFilter::Or { filters });
    }
    if let Some(parts) = split_bool_filter(text, " and ") {
        let filters = parts
            .into_iter()
            .map(filter_from_text)
            .collect::<Option<Vec<_>>>()?;
        return Some(TrackFilter::And { filters });
    }
    if let Some(inner) = text.trim().strip_prefix("not ") {
        return filter_from_text(inner.trim()).map(|inner| TrackFilter::Not {
            inner: Box::new(inner),
        });
    }
    let tokens = words(text);
    match tokens.as_slice() {
        ["lang" | "language", "in", ..]
            if !list_values(text).is_empty()
                && text_after_list(text).is_some_and(str::is_empty) =>
        {
            Some(TrackFilter::LanguageIn {
                values: list_values(text).into_iter().map(str::to_owned).collect(),
            })
        }
        ["codec", "in", ..]
            if !list_values(text).is_empty()
                && text_after_list(text).is_some_and(str::is_empty) =>
        {
            Some(TrackFilter::CodecIn {
                values: list_values(text).into_iter().map(str::to_owned).collect(),
            })
        }
        ["channels", op, value] => Some(TrackFilter::Channels {
            op: comparison_op(Some(op))?,
            value: value.parse::<u64>().ok()?,
        }),
        ["title", "contains", ..] => title_filter_value(text, "contains")
            .filter(|value| is_single_value(value))
            .map(|value| TrackFilter::TitleContains {
                value: strip_quotes(value),
            }),
        ["title", "matches", ..] => title_filter_value(text, "matches")
            .filter(|value| is_single_value(value))
            .map(|value| TrackFilter::TitleMatches {
                value: strip_quotes(value),
            }),
        [first, ..] => filter_predicate(Some(first)),
        [] => None,
    }
}

fn split_bool_filter<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    split_bool_expression(text, delimiter)
}

fn filter_predicate(token: Option<&str>) -> Option<TrackFilter> {
    match token {
        Some("commentary") => Some(TrackFilter::Commentary),
        Some("forced") => Some(TrackFilter::Forced),
        Some("default") => Some(TrackFilter::Default),
        Some("font") => Some(TrackFilter::Font),
        _ => None,
    }
}

fn compiled_value(text: &str) -> CompiledValue {
    let text = text.trim();
    if text.starts_with('"') && text.ends_with('"') {
        return CompiledValue::String {
            value: strip_quotes(text),
        };
    }
    if text == "true" {
        return CompiledValue::Boolean { value: true };
    }
    if text == "false" {
        return CompiledValue::Boolean { value: false };
    }
    if text.contains('.') {
        return CompiledValue::FieldPath {
            path: field_path_segments(text),
        };
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) && !text.is_empty() {
        return CompiledValue::Number {
            value: text.to_owned(),
        };
    }
    CompiledValue::String {
        value: strip_quotes(text),
    }
}

fn unknown_operation(source: &str, span: SourceSpan) -> PolicyDiagnostic {
    PolicyDiagnostic::error(
        DiagnosticCode::UnknownPhaseStatementOrOperation,
        DiagnosticStage::Compile,
        span,
        line_column(source, span.start),
        "unknown phase statement or operation",
    )
}

fn track_target(token: Option<&str>) -> Option<TrackTarget> {
    match token {
        Some("video") => Some(TrackTarget::Video),
        Some("audio") => Some(TrackTarget::Audio),
        Some("subtitle" | "subtitles") => Some(TrackTarget::Subtitle),
        Some("attachment" | "attachments") => Some(TrackTarget::Attachment),
        _ => None,
    }
}

fn default_strategy(token: Option<&str>) -> Option<DefaultStrategy> {
    match token {
        Some("first") => Some(DefaultStrategy::First),
        Some("best") => Some(DefaultStrategy::Best),
        Some("none") => Some(DefaultStrategy::None),
        Some("preserve") => Some(DefaultStrategy::Preserve),
        _ => None,
    }
}

fn rule_match_mode(token: Option<&str>) -> Option<RuleMatchMode> {
    match token {
        Some("first") => Some(RuleMatchMode::First),
        Some("all") => Some(RuleMatchMode::All),
        _ => None,
    }
}

fn error_strategy(token: Option<&str>) -> Option<ErrorStrategy> {
    match token {
        Some("abort") => Some(ErrorStrategy::Abort),
        Some("continue") => Some(ErrorStrategy::Continue),
        Some("skip") => Some(ErrorStrategy::Skip),
        _ => None,
    }
}

fn comparison_op(token: Option<&str>) -> Option<ComparisonOp> {
    match token {
        Some("==" | "=") => Some(ComparisonOp::Eq),
        Some("!=") => Some(ComparisonOp::Ne),
        Some("<") => Some(ComparisonOp::Lt),
        Some("<=") => Some(ComparisonOp::Lte),
        Some(">") => Some(ComparisonOp::Gt),
        Some(">=") => Some(ComparisonOp::Gte),
        Some("contains") => Some(ComparisonOp::Contains),
        Some("matches") => Some(ComparisonOp::Matches),
        _ => None,
    }
}

fn token_string(tokens: &[&str], index: usize, fallback: &str) -> String {
    tokens
        .get(index)
        .map_or(fallback, |value| *value)
        .to_owned()
}

fn field_path_segments(path: &str) -> Vec<String> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

fn strip_quotes(value: &str) -> String {
    value.trim_matches('"').to_owned()
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
#[path = "compiled_test.rs"]
mod tests;
