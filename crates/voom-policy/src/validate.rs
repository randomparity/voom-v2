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
                "on_error" => self.validate_on_error(statement, text.as_ref()),
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
            match operation.keyword().value.as_str() {
                "container" => self.validate_container(operation, text.as_ref()),
                "keep" | "remove" => self.validate_track_operation(operation, text.as_ref()),
                "order" => self.validate_order(operation, text.as_ref()),
                "defaults" => self.validate_defaults(operation, text.as_ref()),
                "actions" => self.validate_actions(operation, text.as_ref()),
                "clear_tags" => {
                    self.validate_clear_tags(operation, text.as_ref());
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
                    if let Some(key) = self.validate_set_tag(operation, text.as_ref()) {
                        set_tags.insert(key);
                    }
                }
                "delete_tag" => {
                    if let Some(key) = self.validate_delete_tag(operation, text.as_ref()) {
                        delete_tags.insert(key);
                    }
                }
                "when" => {
                    self.validate_condition(operation, text.as_ref());
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
                match statement.keyword().value.as_str() {
                    "actions" => self.validate_actions(statement, text.as_ref()),
                    "set_tag" => {
                        let _ = self.validate_set_tag(statement, text.as_ref());
                    }
                    "delete_tag" => {
                        let _ = self.validate_delete_tag(statement, text.as_ref());
                    }
                    _ => self.validate_clear_tags(statement, text.as_ref()),
                }
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
        let tokens = words(text);
        if tokens.get(1).is_none_or(|container| *container != "mkv") {
            self.error(
                DiagnosticCode::UnsupportedContainer,
                statement.span(),
                "Sprint 4 only supports mkv containers",
            );
        }
        if tokens.len() > 2 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "container operation does not accept extra arguments",
            );
        }
    }

    fn validate_track_operation(&mut self, statement: &StatementAst, text: &str) {
        let tokens = words(text);
        self.validate_track_target(statement.span(), tokens.get(1).copied().unwrap_or_default());
        self.validate_language_tokens(statement, text);
        self.validate_field_paths(statement, text);
        if text.contains(" where ") {
            if tokens.get(2).copied() != Some("where") {
                self.error(
                    DiagnosticCode::UnknownPhaseStatementOrOperation,
                    statement.span(),
                    "track filter must follow the track target",
                );
            }
        } else if tokens.len() > 2 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "track operation does not accept extra arguments without `where`",
            );
        }
        if let Some((_, filter)) = text.split_once(" where ")
            && !is_valid_track_filter(filter.trim())
        {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "unknown track filter predicate",
            );
        }
    }

    fn validate_order(&mut self, statement: &StatementAst, text: &str) {
        if words(text).get(1).copied() != Some("tracks") {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "order operation must use `order tracks`",
            );
            return;
        }
        let targets = list_values(text);
        if targets.is_empty() {
            self.error(
                DiagnosticCode::InvalidTrackTarget,
                statement.span(),
                "order tracks requires at least one track target",
            );
        }
        for target in targets {
            self.validate_track_target(statement.span(), target);
        }
        if text_after_list(text).is_some_and(|value| !value.is_empty()) {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "order tracks does not accept extra arguments after the target list",
            );
        }
    }

    fn validate_defaults(&mut self, statement: &StatementAst, text: &str) {
        let tokens = words(text);
        self.validate_track_target(
            statement.span(),
            tokens
                .get(1)
                .map_or("", |value| value.trim_end_matches(':')),
        );
        if tokens
            .get(2)
            .is_none_or(|strategy| !matches!(*strategy, "first" | "best" | "none" | "preserve"))
        {
            self.error(
                DiagnosticCode::InvalidDefaultStrategy,
                statement.span(),
                "default strategy must be first, best, none, or preserve",
            );
        }
        if tokens.len() > 3 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "defaults operation does not accept extra arguments",
            );
        }
    }

    fn validate_actions(&mut self, statement: &StatementAst, text: &str) {
        let tokens = words(text);
        self.validate_track_target(statement.span(), tokens.get(1).copied().unwrap_or_default());
        if tokens.get(2).copied() != Some("clear") {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "track actions operation must use `clear`",
            );
        }
        if tokens.len() > 3 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "track actions operation does not accept extra arguments",
            );
        }
    }

    fn validate_on_error(&mut self, statement: &StatementAst, text: &str) {
        if setting_value(text).is_none_or(|value| !matches!(value, "abort" | "continue" | "skip")) {
            self.error(
                DiagnosticCode::InvalidOnErrorValue,
                statement.span(),
                "on_error must be abort, continue, or skip",
            );
        }
        if !text.contains(':') && words(text).len() > 2 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "on_error does not accept extra arguments",
            );
        }
    }

    fn validate_set_tag(&mut self, statement: &StatementAst, text: &str) -> Option<String> {
        let Some(key) = quoted_value(text) else {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "set_tag requires a quoted tag key",
            );
            return None;
        };
        if text_after_quoted_value(text).is_none_or(str::is_empty) {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "set_tag requires a value",
            );
        } else if text_after_quoted_value(text).is_some_and(|value| !is_single_value(value)) {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "set_tag accepts exactly one value",
            );
        }
        self.validate_field_paths(statement, text);
        Some(key)
    }

    fn validate_delete_tag(&mut self, statement: &StatementAst, text: &str) -> Option<String> {
        let key = quoted_value(text);
        if key.is_none() {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "delete_tag requires a quoted tag key",
            );
        } else if text_after_quoted_value(text).is_some_and(|value| !value.is_empty()) {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "delete_tag does not accept extra arguments",
            );
        }
        key
    }

    fn validate_clear_tags(&mut self, statement: &StatementAst, text: &str) {
        if words(text).len() > 1 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "clear_tags does not accept extra arguments",
            );
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
                    continue;
                }
                let StatementAst::Block { statements, .. } = rule else {
                    self.error(
                        DiagnosticCode::UnknownPhaseStatementOrOperation,
                        rule.span(),
                        "rule must be a block",
                    );
                    continue;
                };
                for nested in statements {
                    self.validate_nested_operation(nested);
                }
            }
        }
    }

    fn validate_condition(&mut self, statement: &StatementAst, text: &str) {
        let condition = text.trim_start_matches("when").trim();
        self.validate_condition_expression(statement, condition);
        if let StatementAst::Block { statements, .. } = statement {
            for nested in statements {
                self.validate_nested_operation(nested);
            }
        }
    }

    fn validate_skip_condition(&mut self, statement: &StatementAst, text: &str) {
        let condition = text
            .trim_start_matches("skip")
            .trim()
            .trim_start_matches("when")
            .trim();
        self.validate_condition_expression(statement, condition);
    }

    fn validate_condition_expression(&mut self, statement: &StatementAst, text: &str) {
        self.validate_field_paths(statement, text);
        if !self.is_valid_condition_expression(statement, text) {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "invalid condition expression",
            );
        }
    }

    fn is_valid_condition_expression(&mut self, statement: &StatementAst, text: &str) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        if let Some(parts) = split_bool_condition(text, " or ") {
            return parts
                .into_iter()
                .all(|part| self.is_valid_condition_expression(statement, part));
        }
        if let Some(parts) = split_bool_condition(text, " and ") {
            return parts
                .into_iter()
                .all(|part| self.is_valid_condition_expression(statement, part));
        }
        if let Some(inner) = text.trim().strip_prefix("not ") {
            return self.is_valid_condition_expression(statement, inner.trim());
        }

        let tokens = words(text);
        match tokens.as_slice() {
            ["exists", target, ..] => {
                self.validate_track_target(statement.span(), target);
                if !is_track_target_name(target) {
                    return true;
                }
                if let Some((_, filter)) = text.split_once(" where ") {
                    is_valid_track_filter(filter.trim())
                } else {
                    tokens.len() == 2
                }
            }
            ["count", target, op, value] => {
                self.validate_track_target(statement.span(), target);
                is_track_target_name(target) && is_comparison_op(op) && value.parse::<u64>().is_ok()
            }
            _ => {
                if let Some(index) = tokens.iter().position(|token| is_comparison_op(token)) {
                    return index > 0
                        && tokens.get(index + 1).is_some_and(|value| !value.is_empty())
                        && tokens.first().is_some_and(|path| path.contains('.'));
                }
                tokens.len() == 1
                    && tokens
                        .first()
                        .is_some_and(|token| token.contains('.') || is_reference_token(token))
            }
        }
    }

    fn validate_track_target(&mut self, span: SourceSpan, target: &str) {
        if !is_track_target_name(target) {
            self.error(
                DiagnosticCode::InvalidTrackTarget,
                span,
                "invalid track target",
            );
        }
    }

    fn validate_language_tokens(&mut self, statement: &StatementAst, text: &str) {
        if !(text.contains(" lang ") || text.contains(" language ") || text.contains("languages "))
        {
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
fn text_after_list(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text[start + 1..].find(']')?;
    Some(text[start + 1 + end + 1..].trim())
}

#[must_use]
fn dependency_values(text: &str) -> Vec<String> {
    let list = list_values(text);
    if !list.is_empty() || text.contains('[') {
        return list.into_iter().map(str::to_owned).collect();
    }
    words(text).into_iter().skip(1).map(str::to_owned).collect()
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
fn text_after_quoted_value(text: &str) -> Option<&str> {
    let start = text.find('"')?;
    let end = text[start + 1..].find('"')?;
    Some(text[start + 1 + end + 1..].trim())
}

#[must_use]
fn is_single_value(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with('"') {
        return quoted_text_end(text).is_some_and(|end| text[end..].trim().is_empty());
    }
    words(text).len() == 1
}

#[must_use]
fn quoted_text_end(text: &str) -> Option<usize> {
    let mut cursor = 1usize;
    let mut escaped = false;
    while cursor < text.len() {
        let ch = text[cursor..].chars().next()?;
        if escaped {
            escaped = false;
            cursor += ch.len_utf8();
            continue;
        }
        if ch == '\\' {
            escaped = true;
            cursor += ch.len_utf8();
            continue;
        }
        cursor += ch.len_utf8();
        if ch == '"' {
            return Some(cursor);
        }
    }
    None
}

#[must_use]
fn field_path_tokens(text: &str) -> Vec<String> {
    let text = without_quoted_text(text);
    text.split(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, '"' | '\'' | '[' | ']' | '(' | ')' | '{' | '}')
    })
    .map(|token| token.trim_matches(|ch: char| matches!(ch, ',' | ':')))
    .filter(|token| token.contains('.'))
    .map(str::to_owned)
    .collect()
}

#[must_use]
fn without_quoted_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if escaped {
            out.push(' ');
            escaped = false;
            continue;
        }
        if in_string && ch == '\\' {
            out.push(' ');
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            out.push(' ');
            continue;
        }
        out.push(if in_string { ' ' } else { ch });
    }

    out
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

#[must_use]
fn is_track_target_name(target: &str) -> bool {
    matches!(
        target,
        "video" | "audio" | "subtitle" | "subtitles" | "attachment" | "attachments"
    )
}

#[must_use]
fn is_comparison_op(token: &str) -> bool {
    matches!(
        token,
        "==" | "=" | "!=" | "<" | "<=" | ">" | ">=" | "contains" | "matches"
    )
}

#[must_use]
fn is_valid_track_filter(text: &str) -> bool {
    if let Some(parts) = split_bool_filter(text, " or ") {
        return parts.into_iter().all(is_valid_track_filter);
    }
    if let Some(parts) = split_bool_filter(text, " and ") {
        return parts.into_iter().all(is_valid_track_filter);
    }
    if let Some(inner) = text.trim().strip_prefix("not ") {
        return is_valid_track_filter(inner.trim());
    }

    let tokens = words(text);
    match tokens.as_slice() {
        ["lang" | "language" | "codec", "in", ..] => !list_values(text).is_empty(),
        ["commentary" | "forced" | "default" | "font"] => true,
        ["title", "contains", ..] => title_filter_value(text, "contains").is_some(),
        ["title", "matches", ..] => title_filter_value(text, "matches").is_some(),
        _ => false,
    }
}

#[must_use]
fn split_bool_filter<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    split_bool_expression(text, delimiter)
}

#[must_use]
fn split_bool_condition<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    if text.contains(" where ") {
        return None;
    }
    split_bool_expression(text, delimiter)
}

#[must_use]
fn split_bool_expression<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    let parts = split_outside_quotes(text, delimiter);
    if parts.len() > 1 { Some(parts) } else { None }
}

#[must_use]
fn split_outside_quotes<'a>(text: &'a str, delimiter: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut cursor = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    while cursor < text.len() {
        let Some(ch) = text[cursor..].chars().next() else {
            break;
        };
        if escaped {
            escaped = false;
            cursor += ch.len_utf8();
            continue;
        }
        if in_string && ch == '\\' {
            escaped = true;
            cursor += ch.len_utf8();
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            cursor += ch.len_utf8();
            continue;
        }
        if !in_string && text[cursor..].starts_with(delimiter) {
            let part = text[start..cursor].trim();
            if !part.is_empty() {
                parts.push(part);
            }
            cursor += delimiter.len();
            start = cursor;
            continue;
        }
        cursor += ch.len_utf8();
    }

    let part = text[start..].trim();
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}

#[must_use]
fn title_filter_value<'a>(text: &'a str, op: &str) -> Option<&'a str> {
    let prefix = format!("title {op} ");
    let value = text.trim().strip_prefix(&prefix)?.trim();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
#[path = "validate_test.rs"]
mod tests;
