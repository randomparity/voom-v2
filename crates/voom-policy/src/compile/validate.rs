use std::collections::{BTreeMap, BTreeSet};

use super::compiled::is_canonical_language_code;
use crate::text::{dependency_values, statement_text, text_after_list, words};
use crate::{
    DiagnosticCode, DiagnosticSeverity, DiagnosticStage, ExprAst, PhaseAst, PolicyAst,
    PolicyDiagnostic, SourceSpan, StatementAst, line_column,
};

mod conditions;
mod operations;

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationResult {
    pub diagnostics: Vec<PolicyDiagnostic>,
}

impl ValidationResult {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error)
    }
}

#[must_use]
pub fn validate_policy_ast(source: &str, ast: &PolicyAst) -> ValidationResult {
    let mut validator = Validator::new(source, ast);
    validator.validate();
    ValidationResult {
        diagnostics: validator.diagnostics,
    }
}

struct Validator<'a> {
    source: &'a str,
    ast: &'a PolicyAst,
    diagnostics: Vec<PolicyDiagnostic>,
}

#[derive(Default)]
struct TagEffects {
    saw_set_tag: bool,
    set_tags: BTreeSet<String>,
    delete_tags: BTreeSet<String>,
}

const fn expr_span(value: &ExprAst) -> SourceSpan {
    match value {
        ExprAst::String(value)
        | ExprAst::Identifier(value)
        | ExprAst::Number(value)
        | ExprAst::FieldPath(value) => value.span,
        ExprAst::Boolean(value) => value.span,
        ExprAst::List { span, .. } => *span,
    }
}

impl<'a> Validator<'a> {
    const fn new(source: &'a str, ast: &'a PolicyAst) -> Self {
        Self {
            source,
            ast,
            diagnostics: Vec::new(),
        }
    }

    fn validate(&mut self) {
        if self.ast.name.value.trim().is_empty() {
            self.error(
                DiagnosticCode::UnexpectedToken,
                self.ast.name.span,
                "policy name must not be empty",
            );
        }
        if self.source.len() > 1_048_576 {
            self.error(
                DiagnosticCode::SourceSizeExceeded,
                SourceSpan::new(0, self.source.len()),
                "policy source exceeds the maximum supported size",
            );
        }
        if self.ast.phases.is_empty() {
            self.error(
                DiagnosticCode::UnexpectedToken,
                self.ast.name.span,
                "policy must declare at least one phase",
            );
        }
        if let Some(parent) = &self.ast.extends {
            self.error(
                DiagnosticCode::DeferredComposition,
                parent.span,
                "policy composition through extends is deferred",
            );
        }
        for statement in &self.ast.unknown_top_level {
            self.error(
                DiagnosticCode::UnknownTopLevelBlock,
                statement.span(),
                "unknown top-level policy block",
            );
        }

        self.validate_config();
        self.validate_metadata();
        self.validate_phase_names();
        self.validate_phase_dependencies();
        for phase in &self.ast.phases {
            self.validate_phase(phase);
        }
    }

    fn validate_config(&mut self) {
        let mut first_by_key = BTreeMap::new();
        for setting in &self.ast.config {
            if let Some(first_span) =
                first_by_key.insert(setting.key.value.as_str(), setting.key.span)
            {
                let mut diagnostic = self.make_error(
                    DiagnosticCode::DuplicateConfigSetting,
                    setting.key.span,
                    format!(
                        "config setting `{}` may be declared only once",
                        setting.key.value
                    ),
                );
                diagnostic.related.push(crate::RelatedSpan {
                    span: first_span,
                    location: line_column(self.source, first_span.start),
                    message: "first config declaration".to_owned(),
                });
                self.diagnostics.push(diagnostic);
                continue;
            }
            match setting.key.value.as_str() {
                "languages" => self.validate_config_languages(setting),
                "on_error" => self.validate_config_on_error(setting),
                _ => self.error(
                    DiagnosticCode::UnknownTopLevelBlock,
                    setting.key.span,
                    "unknown config statement",
                ),
            }
        }
    }

    fn validate_config_languages(&mut self, setting: &crate::SettingAst) {
        let ExprAst::List { values, span } = &setting.value else {
            self.error(
                DiagnosticCode::InvalidLanguageCode,
                expr_span(&setting.value),
                "config languages must be a comma-separated list of quoted language codes",
            );
            return;
        };
        if !self.config_language_list_has_commas(values, *span) {
            self.error(
                DiagnosticCode::InvalidLanguageCode,
                *span,
                "config languages must separate quoted language codes with commas",
            );
        }
        for value in values {
            let ExprAst::String(language) = value else {
                self.error(
                    DiagnosticCode::InvalidLanguageCode,
                    expr_span(value),
                    "config language codes must be quoted",
                );
                continue;
            };
            if !is_canonical_language_code(&language.value) {
                self.error(
                    DiagnosticCode::InvalidLanguageCode,
                    language.span,
                    "language code must be a three-letter lowercase ASCII code",
                );
            }
        }
    }

    fn config_language_list_has_commas(&self, values: &[ExprAst], list_span: SourceSpan) -> bool {
        for pair in values.windows(2) {
            let previous = expr_span(&pair[0]);
            let next = expr_span(&pair[1]);
            let separator = &self.source[previous.end..next.start];
            if !separator.trim_start().starts_with(',') {
                return false;
            }
        }
        let Some(last) = values.last() else {
            return true;
        };
        let suffix = &self.source[expr_span(last).end..list_span.end];
        !suffix.trim_start().starts_with(',')
    }

    fn validate_config_on_error(&mut self, setting: &crate::SettingAst) {
        let ExprAst::Identifier(value) = &setting.value else {
            self.error(
                DiagnosticCode::InvalidOnErrorValue,
                expr_span(&setting.value),
                "config on_error must be abort or continue",
            );
            return;
        };
        if !matches!(value.value.as_str(), "abort" | "continue") {
            self.error(
                DiagnosticCode::InvalidOnErrorValue,
                value.span,
                "config on_error must be abort or continue",
            );
        }
    }

    fn validate_metadata(&mut self) {
        let mut first_requires_tools = None;
        for setting in &self.ast.metadata {
            if setting.key.value != "requires_tools" {
                continue;
            }
            if let Some(first_span) = first_requires_tools {
                let mut diagnostic = self.make_error(
                    DiagnosticCode::InvalidMetadataRequiresTools,
                    setting.key.span,
                    "metadata requires_tools may be declared only once",
                );
                diagnostic.related.push(crate::RelatedSpan {
                    span: first_span,
                    location: line_column(self.source, first_span.start),
                    message: "first requires_tools declaration".to_owned(),
                });
                self.diagnostics.push(diagnostic);
                continue;
            }
            first_requires_tools = Some(setting.key.span);
            self.validate_required_tools_value(&setting.value);
        }
    }

    fn validate_required_tools_value(&mut self, value: &ExprAst) {
        let ExprAst::List { values, .. } = value else {
            self.error(
                DiagnosticCode::InvalidMetadataRequiresTools,
                expr_span(value),
                "metadata requires_tools must be a list of published tool identifiers",
            );
            return;
        };
        for value in values {
            let ExprAst::Identifier(tool) = value else {
                self.error(
                    DiagnosticCode::InvalidMetadataRequiresTools,
                    expr_span(value),
                    "metadata requires_tools entries must be unquoted identifiers",
                );
                continue;
            };
            if !matches!(tool.value.as_str(), "ffmpeg" | "ffprobe" | "mkvtoolnix") {
                self.error(
                    DiagnosticCode::InvalidMetadataRequiresTools,
                    tool.span,
                    format!("unknown metadata tool `{}`", tool.value),
                );
            }
        }
    }

    fn validate_phase_names(&mut self) {
        let mut seen = BTreeMap::<&str, SourceSpan>::new();
        for phase in &self.ast.phases {
            if let Some(first_span) = seen.insert(&phase.name.value, phase.name.span) {
                let mut diagnostic = self.make_error(
                    DiagnosticCode::DuplicatePhaseName,
                    phase.name.span,
                    "duplicate phase name",
                );
                diagnostic.related.push(crate::RelatedSpan {
                    span: first_span,
                    location: line_column(self.source, first_span.start),
                    message: "first phase with this name".to_owned(),
                });
                self.diagnostics.push(diagnostic);
            }
        }
    }

    fn validate_phase_dependencies(&mut self) {
        let phase_names = self
            .ast
            .phases
            .iter()
            .map(|phase| phase.name.value.as_str())
            .collect::<BTreeSet<_>>();
        let mut graph = BTreeMap::<&str, Vec<String>>::new();

        for phase in &self.ast.phases {
            let mut deps = Vec::new();
            for control in &phase.controls {
                if control.keyword().value == "depends_on" {
                    let text = statement_text(control);
                    if text.contains('[')
                        && text_after_list(text.as_ref()).is_some_and(|value| !value.is_empty())
                    {
                        self.error(
                            DiagnosticCode::UnknownPhaseStatementOrOperation,
                            control.span(),
                            "depends_on does not accept extra arguments after the dependency list",
                        );
                    }
                    for dependency in dependency_values(text.as_ref()) {
                        self.validate_phase_reference(
                            &phase.name.value,
                            &dependency,
                            &phase_names,
                            control.span(),
                        );
                        deps.push(dependency);
                    }
                }
                if control.keyword().value == "run_if" {
                    self.validate_run_if(phase, control, &phase_names);
                }
            }
            graph.insert(&phase.name.value, deps);
        }

        for phase in &self.ast.phases {
            let mut visiting = BTreeSet::new();
            let mut visited = BTreeSet::new();
            if has_cycle(&graph, &phase.name.value, &mut visiting, &mut visited) {
                self.error(
                    DiagnosticCode::DependencyCycle,
                    phase.name.span,
                    "phase dependencies contain a cycle",
                );
                break;
            }
        }
    }

    fn validate_phase_reference(
        &mut self,
        phase_name: &str,
        referenced: &str,
        phase_names: &BTreeSet<&str>,
        span: SourceSpan,
    ) {
        if referenced == phase_name {
            self.error(
                DiagnosticCode::SelfDependency,
                span,
                "phase must not depend on itself",
            );
        } else if !phase_names.contains(referenced) {
            self.error(
                DiagnosticCode::UnknownPhaseReference,
                span,
                "phase references an unknown phase",
            );
        }
    }

    fn validate_run_if(
        &mut self,
        phase: &PhaseAst,
        statement: &StatementAst,
        phase_names: &BTreeSet<&str>,
    ) {
        let text = statement_text(statement);
        let tokens = words(text.as_ref());
        if tokens.len() != 3 {
            self.error(
                DiagnosticCode::InvalidRunIfTrigger,
                statement.span(),
                "run_if requires exactly one trigger and one phase",
            );
            return;
        }

        let trigger = tokens[1];
        if trigger != "modified" && trigger != "completed" {
            self.error(
                DiagnosticCode::InvalidRunIfTrigger,
                statement.span(),
                "run_if trigger must be modified or completed",
            );
            return;
        }

        self.validate_phase_reference(&phase.name.value, tokens[2], phase_names, statement.span());
    }

    fn validate_phase(&mut self, phase: &PhaseAst) {
        let mut tag_effects = TagEffects::default();

        for control in &phase.controls {
            let text = statement_text(control);
            self.check_numeric_literals(control, text.as_ref());
            match control.keyword().value.as_str() {
                "depends_on" | "run_if" => {}
                "skip" => self.validate_skip_condition(control, text.as_ref()),
                "on_error" => self.validate_on_error(control),
                _ => self.error(
                    DiagnosticCode::UnknownPhaseStatementOrOperation,
                    control.span(),
                    "unknown phase control",
                ),
            }
        }

        for operation in &phase.operations {
            let text = statement_text(operation);
            self.check_numeric_literals(operation, text.as_ref());
            match operation.keyword().value.as_str() {
                "container" => self.validate_container(operation, text.as_ref()),
                "keep" | "remove" => self.validate_track_operation(operation, text.as_ref()),
                "order" => self.validate_order(operation, text.as_ref()),
                "defaults" => self.validate_defaults(operation, text.as_ref()),
                "actions" => self.validate_actions(operation, text.as_ref()),
                "clear_tags" => {
                    self.validate_clear_tags(operation, text.as_ref());
                    self.record_clear_tags(&mut tag_effects, operation.span());
                }
                "set_tag" => {
                    if let Some(key) = self.validate_set_tag(operation, text.as_ref()) {
                        tag_effects.saw_set_tag = true;
                        tag_effects.set_tags.insert(key);
                    }
                }
                "delete_tag" => {
                    if let Some(key) = self.validate_delete_tag(operation, text.as_ref()) {
                        tag_effects.delete_tags.insert(key);
                    }
                }
                "when" => {
                    self.validate_condition(operation, text.as_ref(), &mut tag_effects);
                }
                "rules" => self.validate_rules(operation, text.as_ref(), &mut tag_effects),
                "extend" => self.error(
                    DiagnosticCode::DeferredPhaseInheritance,
                    operation.span(),
                    "phase inheritance through extend is deferred",
                ),
                "transcode" => self.validate_transcode_statement(operation),
                "extract" => self.validate_extract_statement(operation),
                "verify" => self.validate_verify_statement(operation),
                "synthesize" => self.validate_synthesize_statement(operation),
                _ => self.error(
                    DiagnosticCode::UnknownPhaseStatementOrOperation,
                    operation.span(),
                    "unknown phase statement or operation",
                ),
            }
        }

        let conflicts = tag_effects
            .set_tags
            .intersection(&tag_effects.delete_tags)
            .cloned()
            .collect::<Vec<_>>();
        for key in conflicts {
            self.error(
                DiagnosticCode::AmbiguousTagOperationConflict,
                phase.name.span,
                format!("set_tag and delete_tag both target `{key}`"),
            );
        }
    }

    fn make_error(
        &self,
        code: DiagnosticCode,
        span: SourceSpan,
        message: impl Into<String>,
    ) -> PolicyDiagnostic {
        PolicyDiagnostic::error(
            code,
            DiagnosticStage::Validate,
            span,
            line_column(self.source, span.start),
            message,
        )
    }

    fn error(&mut self, code: DiagnosticCode, span: SourceSpan, message: impl Into<String>) {
        let diagnostic = self.make_error(code, span, message);
        self.diagnostics.push(diagnostic);
    }

    /// Reject all-digit numeric literals that overflow `u64`. Mirrors the
    /// numeric-literal predicate the lowering pass uses (`compiled_value`): a
    /// token of all ASCII digits is a number. An over-long one lowers to a
    /// `Number` the planner silently drops (`parse::<u64>()` -> `None`), so the
    /// condition never matches — a silent wrong answer. A hard compile error is
    /// the safer failure mode.
    fn check_numeric_literals(&mut self, statement: &StatementAst, text: &str) {
        for token in words(text) {
            if !token.is_empty()
                && token.bytes().all(|byte| byte.is_ascii_digit())
                && token.parse::<u64>().is_err()
            {
                self.error(
                    DiagnosticCode::NumericLiteralOutOfRange,
                    statement.span(),
                    format!(
                        "numeric literal `{token}` exceeds the maximum supported value ({})",
                        u64::MAX
                    ),
                );
            }
        }
    }

    fn warning(&mut self, code: DiagnosticCode, span: SourceSpan, message: impl Into<String>) {
        self.diagnostics.push(PolicyDiagnostic::warning(
            code,
            DiagnosticStage::Validate,
            span,
            line_column(self.source, span.start),
            message,
        ));
    }
}

#[must_use]
fn has_cycle(
    graph: &BTreeMap<&str, Vec<String>>,
    node: &str,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> bool {
    if visited.contains(node) {
        return false;
    }
    if !visiting.insert(node.to_owned()) {
        return true;
    }
    for dependency in graph.get(node).into_iter().flatten() {
        if has_cycle(graph, dependency.as_str(), visiting, visited) {
            return true;
        }
    }
    visiting.remove(node);
    visited.insert(node.to_owned());
    false
}

#[cfg(test)]
#[path = "validate_test.rs"]
mod tests;
