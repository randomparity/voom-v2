use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use crate::{
    DiagnosticCode, DiagnosticSeverity, DiagnosticStage, PhaseAst, PolicyAst, PolicyDiagnostic,
    SourceSpan, StatementAst, line_column,
};

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
                "on_error" => {
                    if let Some(value) = setting_value(text.as_ref())
                        && !matches!(value, "abort" | "continue" | "skip")
                    {
                        self.error(
                            DiagnosticCode::InvalidOnErrorValue,
                            statement.span(),
                            "on_error must be abort, continue, or skip",
                        );
                    }
                }
                _ => self.error(
                    DiagnosticCode::UnknownTopLevelBlock,
                    statement.span(),
                    "unknown config statement",
                ),
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
                    for dependency in list_values(text.as_ref()) {
                        self.validate_phase_reference(
                            &phase.name.value,
                            dependency,
                            &phase_names,
                            control.span(),
                        );
                        deps.push(dependency.to_owned());
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
            if is_reference_token(token) {
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
        let mut saw_set_tag = false;
        let mut set_tags = BTreeSet::new();
        let mut delete_tags = BTreeSet::new();

        for control in &phase.controls {
            let text = statement_text(control);
            match control.keyword().value.as_str() {
                "depends_on" | "run_if" => {}
                "skip" => self.validate_field_paths(control, text.as_ref()),
                "on_error" => {
                    if let Some(value) = setting_value(text.as_ref())
                        && !matches!(value, "abort" | "continue" | "skip")
                    {
                        self.error(
                            DiagnosticCode::InvalidOnErrorValue,
                            control.span(),
                            "on_error must be abort, continue, or skip",
                        );
                    }
                }
                _ => self.error(
                    DiagnosticCode::UnknownPhaseStatementOrOperation,
                    control.span(),
                    "unknown phase control",
                ),
            }
        }

        for operation in &phase.operations {
            let text = statement_text(operation);
            match operation.keyword().value.as_str() {
                "container" => self.validate_container(operation, text.as_ref()),
                "keep" | "remove" => self.validate_track_operation(operation, text.as_ref()),
                "order" => self.validate_order(operation, text.as_ref()),
                "defaults" => self.validate_defaults(operation, text.as_ref()),
                "actions" => self.validate_actions(operation, text.as_ref()),
                "clear_tags" => {
                    if saw_set_tag {
                        self.error(
                            DiagnosticCode::TagOrderingError,
                            operation.span(),
                            "clear_tags must precede set_tag in a phase",
                        );
                    }
                }
                "set_tag" => {
                    saw_set_tag = true;
                    if let Some(key) = quoted_value(text.as_ref()) {
                        set_tags.insert(key);
                    }
                    self.validate_field_paths(operation, text.as_ref());
                }
                "delete_tag" => {
                    if let Some(key) = quoted_value(text.as_ref()) {
                        delete_tags.insert(key);
                    }
                }
                "when" => {
                    self.validate_condition(operation, text.as_ref());
                    if let StatementAst::Block { statements, .. } = operation {
                        for statement in statements {
                            self.validate_nested_operation(statement);
                        }
                    }
                }
                "rules" => self.validate_rules(operation, text.as_ref()),
                "extend" => self.error(
                    DiagnosticCode::DeferredPhaseInheritance,
                    operation.span(),
                    "phase inheritance through extend is deferred",
                ),
                "transcode" | "synthesize" | "verify" => self.error(
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

        for key in set_tags.intersection(&delete_tags) {
            self.error(
                DiagnosticCode::AmbiguousTagOperationConflict,
                phase.name.span,
                format!("set_tag and delete_tag both target `{key}`"),
            );
        }
    }

    fn validate_nested_operation(&mut self, statement: &StatementAst) {
        match statement.keyword().value.as_str() {
            "container" => {
                let text = statement_text(statement);
                self.validate_container(statement, text.as_ref());
            }
            "keep" | "remove" => {
                let text = statement_text(statement);
                self.validate_track_operation(statement, text.as_ref());
            }
            "order" => {
                let text = statement_text(statement);
                self.validate_order(statement, text.as_ref());
            }
            "defaults" => {
                let text = statement_text(statement);
                self.validate_defaults(statement, text.as_ref());
            }
            "actions" | "clear_tags" | "set_tag" | "delete_tag" => {
                let text = statement_text(statement);
                self.validate_field_paths(statement, text.as_ref());
            }
            "when" => {
                let text = statement_text(statement);
                self.validate_condition(statement, text.as_ref());
            }
            "transcode" | "synthesize" | "verify" => self.error(
                DiagnosticCode::DeferredExecutionOperation,
                statement.span(),
                "execution operation is deferred to a later sprint",
            ),
            _ => self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "unknown nested operation",
            ),
        }
    }

    fn validate_container(&mut self, statement: &StatementAst, text: &str) {
        if words(text)
            .get(1)
            .is_some_and(|container| *container != "mkv")
        {
            self.error(
                DiagnosticCode::UnsupportedContainer,
                statement.span(),
                "Sprint 4 only supports mkv containers",
            );
        }
    }

    fn validate_track_operation(&mut self, statement: &StatementAst, text: &str) {
        let tokens = words(text);
        if let Some(target) = tokens.get(1) {
            self.validate_track_target(statement.span(), target);
        }
        self.validate_language_tokens(statement, text);
        self.validate_field_paths(statement, text);
    }

    fn validate_order(&mut self, statement: &StatementAst, text: &str) {
        for target in list_values(text) {
            self.validate_track_target(statement.span(), target);
        }
    }

    fn validate_defaults(&mut self, statement: &StatementAst, text: &str) {
        let tokens = words(text);
        if let Some(target) = tokens.get(1).map(|value| value.trim_end_matches(':')) {
            self.validate_track_target(statement.span(), target);
        }
        if let Some(strategy) = tokens.get(2)
            && !matches!(*strategy, "first" | "best" | "none" | "preserve")
        {
            self.error(
                DiagnosticCode::InvalidDefaultStrategy,
                statement.span(),
                "default strategy must be first, best, none, or preserve",
            );
        }
    }

    fn validate_actions(&mut self, statement: &StatementAst, text: &str) {
        if let Some(target) = words(text).get(1) {
            self.validate_track_target(statement.span(), target);
        }
    }

    fn validate_rules(&mut self, statement: &StatementAst, text: &str) {
        let mode = words(text).get(1).copied().unwrap_or_default();
        if !matches!(mode, "first" | "all") {
            self.error(
                DiagnosticCode::InvalidRuleMatchMode,
                statement.span(),
                "rules mode must be first or all",
            );
        }
        if let StatementAst::Block { statements, .. } = statement {
            for rule in statements {
                if rule.keyword().value != "rule" {
                    self.error(
                        DiagnosticCode::UnknownPhaseStatementOrOperation,
                        rule.span(),
                        "rules block may only contain rule blocks",
                    );
                }
            }
        }
    }

    fn validate_condition(&mut self, statement: &StatementAst, text: &str) {
        self.validate_field_paths(statement, text);
        if let StatementAst::Block { statements, .. } = statement {
            for nested in statements {
                self.validate_nested_operation(nested);
            }
        }
    }

    fn validate_track_target(&mut self, span: SourceSpan, target: &str) {
        if !matches!(
            target,
            "video" | "audio" | "subtitle" | "subtitles" | "attachment" | "attachments"
        ) {
            self.error(
                DiagnosticCode::InvalidTrackTarget,
                span,
                "invalid track target",
            );
        }
    }

    fn validate_language_tokens(&mut self, statement: &StatementAst, text: &str) {
        if !(text.contains(" lang ") || text.contains("languages ")) {
            return;
        }
        for value in list_values(text) {
            if value != "eng"
                && value != "und"
                && !(value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_lowercase()))
            {
                self.error(
                    DiagnosticCode::InvalidLanguageCode,
                    statement.span(),
                    "language code must be eng, und, or a three-letter lowercase ASCII code",
                );
            }
        }
    }

    fn validate_field_paths(&mut self, statement: &StatementAst, text: &str) {
        for token in field_path_tokens(text) {
            let Some((root, rest)) = token.split_once('.') else {
                continue;
            };
            if matches!(root, "plugin" | "external") {
                if !rest.is_empty() {
                    self.warning(
                        DiagnosticCode::UnknownExtensionNamespace,
                        statement.span(),
                        "extension namespace is not registered in Sprint 4",
                    );
                }
            } else if !is_core_field_root(root) {
                self.error(
                    DiagnosticCode::InvalidCoreFieldPath,
                    statement.span(),
                    "unknown core field path root",
                );
            }
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

#[must_use]
fn statement_text(statement: &StatementAst) -> Cow<'_, str> {
    match statement {
        StatementAst::Raw { text, .. } => Cow::Borrowed(text),
        StatementAst::Block { keyword, name, .. } => {
            if let Some(name) = name {
                Cow::Owned(format!("{} {}", keyword.value, name.value))
            } else {
                Cow::Borrowed(keyword.value.as_str())
            }
        }
    }
}

#[must_use]
fn words(text: &str) -> Vec<&str> {
    text.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '[' | ']' | ',' | ':'))
        .filter(|word| !word.is_empty())
        .collect()
}

#[must_use]
fn list_values(text: &str) -> Vec<&str> {
    let Some(start) = text.find('[') else {
        return Vec::new();
    };
    let Some(end) = text[start + 1..].find(']') else {
        return Vec::new();
    };
    text[start + 1..start + 1 + end]
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

#[must_use]
fn setting_value(text: &str) -> Option<&str> {
    text.split_once(':')
        .map(|(_, value)| value.trim())
        .or_else(|| words(text).get(1).copied())
}

#[must_use]
fn quoted_value(text: &str) -> Option<String> {
    let start = text.find('"')?;
    let end = text[start + 1..].find('"')?;
    Some(text[start + 1..start + 1 + end].to_owned())
}

#[must_use]
fn field_path_tokens(text: &str) -> Vec<&str> {
    text.split(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, '"' | '\'' | '[' | ']' | '(' | ')' | '{' | '}')
    })
    .map(|token| token.trim_matches(|ch: char| matches!(ch, ',' | ':')))
    .filter(|token| token.contains('.'))
    .collect()
}

#[must_use]
fn is_core_field_root(root: &str) -> bool {
    matches!(
        root,
        "video"
            | "audio"
            | "subtitle"
            | "subtitles"
            | "attachment"
            | "attachments"
            | "container"
            | "identity"
            | "quality"
            | "issue"
            | "bundle"
    )
}

#[must_use]
fn is_reference_token(token: &str) -> bool {
    token
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
}

#[cfg(test)]
#[path = "validate_test.rs"]
mod tests;
