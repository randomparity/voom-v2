use std::borrow::Cow;

use crate::StatementAst;

#[must_use]
pub(crate) fn statement_text(statement: &StatementAst) -> Cow<'_, str> {
    match statement {
        StatementAst::Raw { text, .. } => Cow::Borrowed(text),
        StatementAst::TranscodeInline { header, .. }
        | StatementAst::SynthesizeInline { header, .. } => Cow::Borrowed(header.as_str()),
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
pub(crate) fn words(text: &str) -> Vec<&str> {
    text.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '[' | ']' | ',' | ':'))
        .filter(|word| !word.is_empty())
        .collect()
}

#[must_use]
pub(crate) fn list_values(text: &str) -> Vec<&str> {
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
pub(crate) fn text_after_list(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text[start + 1..].find(']')?;
    Some(text[start + 1 + end + 1..].trim())
}

#[must_use]
pub(crate) fn dependency_values(text: &str) -> Vec<String> {
    let list = list_values(text);
    if !list.is_empty() || text.contains('[') {
        return list.into_iter().map(str::to_owned).collect();
    }
    words(text).into_iter().skip(1).map(str::to_owned).collect()
}

#[must_use]
pub(crate) fn setting_value(text: &str) -> Option<&str> {
    text.split_once(':')
        .map(|(_, value)| value.trim())
        .or_else(|| words(text).get(1).copied())
}

#[must_use]
pub(crate) fn quoted_value(text: &str) -> Option<String> {
    let start = text.find('"')?;
    let end = text[start + 1..].find('"')?;
    Some(text[start + 1..start + 1 + end].to_owned())
}

#[must_use]
pub(crate) fn text_after_quoted_value(text: &str) -> Option<&str> {
    let start = text.find('"')?;
    let end = text[start + 1..].find('"')?;
    Some(text[start + 1 + end + 1..].trim())
}

#[must_use]
pub(crate) fn is_single_value(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with('"') {
        return quoted_text_end(text).is_some_and(|end| text[end..].trim().is_empty());
    }
    words(text).len() == 1
}

#[must_use]
pub(crate) fn comparison_rhs<'a>(text: &'a str, op: &str) -> Option<&'a str> {
    let spaced_op = format!(" {op} ");
    let rhs = text
        .split_once(&spaced_op)
        .map(|(_, rhs)| rhs.trim())
        .or_else(|| text.split_once(op).map(|(_, rhs)| rhs.trim()))?;
    if rhs.is_empty() { None } else { Some(rhs) }
}

#[must_use]
pub(crate) fn split_bool_expression<'a>(text: &'a str, delimiter: &str) -> Option<Vec<&'a str>> {
    let parts = split_outside_quotes(text, delimiter);
    if parts.len() > 1 { Some(parts) } else { None }
}

#[must_use]
pub(crate) fn strip_outer_group(text: &str) -> &str {
    let mut text = text.trim();
    loop {
        let Some(inner) = text
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
        else {
            return text;
        };
        if !is_balanced_parenthesized(text) {
            return text;
        }
        text = inner.trim();
    }
}

#[must_use]
pub(crate) fn title_filter_value<'a>(text: &'a str, op: &str) -> Option<&'a str> {
    let prefix = format!("title {op} ");
    let value = text.trim().strip_prefix(&prefix)?.trim();
    if value.is_empty() { None } else { Some(value) }
}

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

fn split_outside_quotes<'a>(text: &'a str, delimiter: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut cursor = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut paren_depth = 0usize;

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
        if !in_string {
            if ch == '(' {
                paren_depth = paren_depth.saturating_add(1);
                cursor += ch.len_utf8();
                continue;
            }
            if ch == ')' {
                paren_depth = paren_depth.saturating_sub(1);
                cursor += ch.len_utf8();
                continue;
            }
        }
        if !in_string && paren_depth == 0 && text[cursor..].starts_with(delimiter) {
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

fn is_balanced_parenthesized(text: &str) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if ch == '(' {
            depth = depth.saturating_add(1);
        } else if ch == ')' {
            depth = depth.saturating_sub(1);
            if depth == 0 && index + ch.len_utf8() != text.len() {
                return false;
            }
        }
    }

    depth == 0
}
