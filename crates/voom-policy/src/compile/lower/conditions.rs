use crate::text::{
    comparison_rhs, is_single_value, list_values, split_bool_expression, strip_outer_group,
    text_after_list, title_filter_value, words,
};

use super::super::compiled::{
    ComparisonOp, CompiledCondition, CompiledValue, TrackFilter, TrackTarget,
};

pub(super) fn condition_from_text(text: &str) -> CompiledCondition {
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

pub(super) fn track_filter(text: &str) -> Option<TrackFilter> {
    let where_text = text
        .split_once(" where ")
        .map(|(_, filter)| filter.trim())?;
    filter_from_text(where_text)
}

/// Parse a bare track-filter clause (already extracted from its keyword, e.g.
/// the text after `synthesize audio from`). Unlike [`track_filter`], it does not
/// split on ` where `.
pub(super) fn track_filter_clause(text: &str) -> Option<TrackFilter> {
    filter_from_text(text.trim())
}

pub(super) fn compiled_value(text: &str) -> CompiledValue {
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

pub(super) fn track_target(token: Option<&str>) -> Option<TrackTarget> {
    match token {
        Some("video") => Some(TrackTarget::Video),
        Some("audio") => Some(TrackTarget::Audio),
        Some("subtitle" | "subtitles") => Some(TrackTarget::Subtitle),
        Some("attachment" | "attachments") => Some(TrackTarget::Attachment),
        _ => None,
    }
}

pub(super) fn strip_quotes(value: &str) -> String {
    value.trim_matches('"').to_owned()
}

fn split_bool_condition<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    if text.contains(" where ") {
        return None;
    }
    split_bool_expression(text, delimiter)
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
        ["lang" | "language", "==", value] if !value.is_empty() => Some(TrackFilter::LanguageIn {
            values: vec![strip_quotes(value)],
        }),
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

fn field_path_segments(path: &str) -> Vec<String> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}
