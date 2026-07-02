use crate::text::{
    comparison_rhs, is_single_value, list_values, split_bool_expression, strip_outer_group,
    text_after_list, title_filter_value, words,
};
use crate::{DiagnosticCode, SourceSpan, StatementAst};

use super::Validator;

impl Validator<'_> {
    pub(super) fn validate_condition(
        &mut self,
        statement: &StatementAst,
        text: &str,
        tag_effects: &mut super::TagEffects,
    ) {
        let condition = text.trim_start_matches("when").trim();
        self.validate_condition_expression(statement, condition);
        if let StatementAst::Block { statements, .. } = statement {
            for nested in statements {
                self.validate_nested_operation(nested, tag_effects);
            }
        }
    }

    pub(super) fn validate_skip_condition(&mut self, statement: &StatementAst, text: &str) {
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
        let text = strip_outer_group(text.trim());
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
                        && tokens.first().is_some_and(|path| path.contains('.'))
                        && comparison_rhs(text, tokens[index]).is_some_and(is_single_value);
                }
                tokens.len() == 1
                    && tokens
                        .first()
                        .is_some_and(|token| token.contains('.') || is_reference_token(token))
            }
        }
    }

    pub(super) fn validate_track_target(&mut self, span: SourceSpan, target: &str) {
        if !is_track_target_name(target) {
            self.error(
                DiagnosticCode::InvalidTrackTarget,
                span,
                "invalid track target",
            );
        }
    }

    pub(super) fn validate_language_tokens(&mut self, statement: &StatementAst, text: &str) {
        if !(text.contains(" lang ") || text.contains(" language ") || text.contains("languages "))
        {
            return;
        }
        let mut values: Vec<String> = list_values(text).into_iter().map(str::to_owned).collect();
        values.extend(language_equality_values(text));
        for value in values {
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

    pub(super) fn validate_field_paths(&mut self, statement: &StatementAst, text: &str) {
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
            } else if !is_valid_core_field_path(root, rest) {
                self.error(
                    DiagnosticCode::InvalidCoreFieldPath,
                    statement.span(),
                    "unknown core field path",
                );
            }
        }
    }
}

/// Language codes on the right-hand side of `lang|language == <token>` filters.
/// `list_values` only reads bracketed `in [...]` lists, so the equality form
/// needs its own extraction to be language-code validated.
#[must_use]
fn language_equality_values(text: &str) -> Vec<String> {
    let tokens = words(text);
    let mut values = Vec::new();
    for window in tokens.windows(3) {
        if matches!(window[0], "lang" | "language") && window[1] == "==" {
            values.push(window[2].trim_matches('"').to_owned());
        }
    }
    values
}

#[must_use]
pub(super) fn is_reference_token(token: &str) -> bool {
    token
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
}

#[must_use]
pub(super) fn is_valid_track_filter(text: &str) -> bool {
    let text = strip_outer_group(text.trim());
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
        ["lang" | "language" | "codec", "in", ..] => {
            !list_values(text).is_empty() && text_after_list(text).is_some_and(str::is_empty)
        }
        ["lang" | "language", "==", value] => !value.is_empty(),
        ["channels", op, value] => is_comparison_op(op) && value.parse::<u64>().is_ok(),
        ["commentary" | "forced" | "default" | "font"] => true,
        ["title", "contains", ..] => {
            title_filter_value(text, "contains").is_some_and(is_single_value)
        }
        ["title", "matches", ..] => {
            title_filter_value(text, "matches").is_some_and(is_single_value)
        }
        _ => false,
    }
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
            | "media"
            | "identity"
            | "quality"
            | "issue"
            | "bundle"
    )
}

#[must_use]
fn is_valid_core_field_path(root: &str, rest: &str) -> bool {
    let parts = rest.split('.').collect::<Vec<_>>();
    if parts.iter().any(|part| part.is_empty()) {
        return false;
    }
    match root {
        "video" => matches!(
            parts.as_slice(),
            ["codec"
                | "title"
                | "width"
                | "height"
                | "hdr"
                | "bitrate"
                | "duration"
                | "duration_millis"
                | "health_flags"]
        ),
        "audio" => matches!(
            parts.as_slice(),
            ["codec"
                | "lang"
                | "language"
                | "languages"
                | "channels"
                | "commentary"
                | "forced"
                | "default"
                | "title"]
        ),
        "subtitle" | "subtitles" => {
            matches!(
                parts.as_slice(),
                ["lang"
                    | "language"
                    | "languages"
                    | "forced"
                    | "default"
                    | "title"
                    | "disposition"]
            )
        }
        "attachment" | "attachments" => {
            matches!(parts.as_slice(), ["font" | "title" | "disposition"])
        }
        "container" => matches!(parts.as_slice(), ["name" | "value"]),
        "media" => matches!(parts.as_slice(), ["container" | "duration_millis"]),
        "identity" => matches!(
            parts.as_slice(),
            ["title"
                | "assertion_type"
                | "provider"
                | "provider_version"
                | "confidence"
                | "provenance"
                | "observed_at"]
        ),
        "quality" => matches!(
            parts.as_slice(),
            ["profile_name" | "profile_version" | "dimension_weights"]
        ),
        "issue" => matches!(
            parts.as_slice(),
            ["kind" | "severity" | "priority" | "state" | "reason"]
        ),
        "bundle" => matches!(
            parts.as_slice(),
            ["role"
                | "desired_state"
                | "language"
                | "label"
                | "disposition"
                | "artifact_expectation"]
        ),
        _ => false,
    }
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
