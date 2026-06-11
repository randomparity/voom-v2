use std::collections::{BTreeMap, BTreeSet};

use crate::text::{dependency_values, statement_text, text_after_list, words};
use crate::{
    DiagnosticCode, DiagnosticSeverity, DiagnosticStage, PhaseAst, PolicyAst, PolicyDiagnostic,
    SourceSpan, StatementAst, line_column,
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
        for statement in &self.ast.config {
            let text = statement_text(statement);
            match statement.keyword().value.as_str() {
                "languages" => self.validate_language_tokens(statement, text.as_ref()),
                "on_error" => self.validate_on_error(statement, text.as_ref()),
                _ => self.error(
                    DiagnosticCode::UnknownTopLevelBlock,
                    statement.span(),
                    "unknown config statement",
                ),
            }
        }
    }

    fn validate_metadata(&mut self) {
        for setting in &self.ast.metadata {
            if setting.key.value == "requires_tools" {
                self.warning(
                    DiagnosticCode::MetadataRequiresToolsDeferred,
                    setting.key.span,
                    "metadata requires_tools is not represented as worker capabilities in Sprint 4",
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
        let Some(trigger) = tokens.get(1).or_else(|| tokens.get(2)) else {
            self.error(
                DiagnosticCode::InvalidRunIfTrigger,
                statement.span(),
                "run_if requires a trigger",
            );
            return;
        };
        if !matches!(*trigger, "modified" | "completed") {
            self.error(
                DiagnosticCode::InvalidRunIfTrigger,
                statement.span(),
                "run_if trigger must be modified or completed",
            );
        }
        for token in tokens.into_iter().skip(2) {
            if conditions::is_reference_token(token) {
                self.validate_phase_reference(
                    &phase.name.value,
                    token,
                    phase_names,
                    statement.span(),
                );
            }
        }
    }

    fn validate_phase(&mut self, phase: &PhaseAst) {
        let mut tag_effects = TagEffects::default();

        for control in &phase.controls {
            let text = statement_text(control);
            self.check_numeric_literals(control, text.as_ref());
            match control.keyword().value.as_str() {
                "depends_on" | "run_if" => {}
                "skip" => self.validate_skip_condition(control, text.as_ref()),
                "on_error" => self.validate_on_error(control, text.as_ref()),
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
                "synthesize" | "verify" => self.error(
                    DiagnosticCode::DeferredExecutionOperation,
                    operation.span(),
                    "execution operation is deferred to a later sprint",
                ),
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
