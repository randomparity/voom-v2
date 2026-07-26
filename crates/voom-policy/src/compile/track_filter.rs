use super::compiled::{ComparisonOp, TrackFilter};

const MAX_FILTER_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ParseError;

pub(super) fn parse_track_filter(text: &str) -> Result<TrackFilter, ParseError> {
    parse_expression(text.trim(), MAX_FILTER_DEPTH)
}

pub(super) fn parse_optional_filter(text: &str) -> Result<Option<TrackFilter>, ParseError> {
    let Some((_, filter)) = text.split_once(" where ") else {
        return Ok(None);
    };
    parse_track_filter(filter).map(Some)
}

pub(super) fn parse_required_filter(text: &str) -> Result<TrackFilter, ParseError> {
    parse_track_filter(text)
}

fn parse_expression(text: &str, remaining: usize) -> Result<TrackFilter, ParseError> {
    if text.is_empty() {
        return Err(ParseError);
    }
    if let Some(parts) = split_top_level(text, " or ")? {
        let remaining = descend(remaining)?;
        let filters = parse_children(parts, remaining)?;
        return Ok(TrackFilter::Or { filters });
    }
    if let Some(parts) = split_top_level(text, " and ")? {
        let remaining = descend(remaining)?;
        let filters = parse_children(parts, remaining)?;
        return Ok(TrackFilter::And { filters });
    }
    if let Some(inner) = text.strip_prefix("not ") {
        let inner = parse_expression(inner.trim(), descend(remaining)?)?;
        return Ok(TrackFilter::Not {
            inner: Box::new(inner),
        });
    }
    if owns_outer_group(text) {
        let inner = &text[1..text.len() - 1];
        return parse_expression(inner.trim(), descend(remaining)?);
    }
    parse_leaf(text)
}

fn parse_children(parts: Vec<&str>, remaining: usize) -> Result<Vec<TrackFilter>, ParseError> {
    parts
        .into_iter()
        .map(|part| parse_expression(part, remaining))
        .collect()
}

const fn descend(remaining: usize) -> Result<usize, ParseError> {
    match remaining.checked_sub(1) {
        Some(remaining) => Ok(remaining),
        None => Err(ParseError),
    }
}

fn parse_leaf(text: &str) -> Result<TrackFilter, ParseError> {
    if let Some(value) = text.strip_prefix("language == ") {
        return Ok(TrackFilter::LanguageIn {
            values: vec![parse_quoted_token(value)?],
        });
    }
    if let Some(value) = text.strip_prefix("language in ") {
        return Ok(TrackFilter::LanguageIn {
            values: parse_quoted_token_list(value)?,
        });
    }
    if let Some(value) = text.strip_prefix("codec in ") {
        return Ok(TrackFilter::CodecIn {
            values: parse_quoted_token_list(value)?,
        });
    }
    parse_scalar_leaf(text)
}

fn parse_scalar_leaf(text: &str) -> Result<TrackFilter, ParseError> {
    if let Some(value) = text.strip_prefix("channels ") {
        return parse_channels(value);
    }
    match text {
        "commentary" => Ok(TrackFilter::Commentary),
        "forced" => Ok(TrackFilter::Forced),
        "default" => Ok(TrackFilter::Default),
        "font" => Ok(TrackFilter::Font),
        _ => parse_title(text),
    }
}

fn parse_channels(text: &str) -> Result<TrackFilter, ParseError> {
    let mut tokens = text.split_ascii_whitespace();
    let op = match tokens.next() {
        Some("==") => ComparisonOp::Eq,
        Some("!=") => ComparisonOp::Ne,
        Some("<") => ComparisonOp::Lt,
        Some("<=") => ComparisonOp::Lte,
        Some(">") => ComparisonOp::Gt,
        Some(">=") => ComparisonOp::Gte,
        _ => return Err(ParseError),
    };
    let value = tokens.next().ok_or(ParseError)?;
    if tokens.next().is_some()
        || value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ParseError);
    }
    Ok(TrackFilter::Channels {
        op,
        value: value.parse::<u64>().map_err(|_| ParseError)?,
    })
}

fn parse_title(text: &str) -> Result<TrackFilter, ParseError> {
    let value = text.strip_prefix("title contains ").ok_or(ParseError)?;
    let value = value.trim();
    let end = quoted_string_end(value)?;
    if end != value.len() || end == 2 {
        return Err(ParseError);
    }
    Ok(TrackFilter::TitleContains {
        value: value.trim_matches('"').to_owned(),
    })
}

fn quoted_string_end(text: &str) -> Result<usize, ParseError> {
    if !text.starts_with('"') {
        return Err(ParseError);
    }
    let mut escaped = false;
    for (index, ch) in text[1..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Ok(index + 2);
        }
    }
    Err(ParseError)
}

fn parse_quoted_token(text: &str) -> Result<String, ParseError> {
    let text = text.trim();
    let (token, end) = quoted_token_at(text, 0)?;
    if !text[end..].trim().is_empty() {
        return Err(ParseError);
    }
    Ok(token)
}

fn parse_quoted_token_list(text: &str) -> Result<Vec<String>, ParseError> {
    let text = text.trim();
    if !text.starts_with('[') {
        return Err(ParseError);
    }
    let mut cursor = skip_whitespace(text, 1);
    let mut values = Vec::new();
    loop {
        let (value, end) = quoted_token_at(text, cursor)?;
        values.push(value);
        cursor = skip_whitespace(text, end);
        match text.as_bytes().get(cursor) {
            Some(b',') => cursor = skip_whitespace(text, cursor + 1),
            Some(b']') if text[cursor + 1..].trim().is_empty() => return Ok(values),
            _ => return Err(ParseError),
        }
    }
}

fn quoted_token_at(text: &str, start: usize) -> Result<(String, usize), ParseError> {
    if text.as_bytes().get(start) != Some(&b'"') {
        return Err(ParseError);
    }
    let rest = &text[start + 1..];
    let end = rest.find('"').ok_or(ParseError)?;
    let token = &rest[..end];
    if token.is_empty() || !token.bytes().all(is_stable_token_byte) {
        return Err(ParseError);
    }
    Ok((token.to_owned(), start + end + 2))
}

const fn is_stable_token_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
}

fn skip_whitespace(text: &str, mut cursor: usize) -> usize {
    while text
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    cursor
}

fn split_top_level<'a>(text: &'a str, delimiter: &str) -> Result<Option<Vec<&'a str>>, ParseError> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    let mut state = ScanState::default();
    while cursor < text.len() {
        let ch = text[cursor..].chars().next().ok_or(ParseError)?;
        if state.is_top_level() && text[cursor..].starts_with(delimiter) {
            parts.push(text[start..cursor].trim());
            cursor += delimiter.len();
            start = cursor;
            continue;
        }
        state.advance(ch)?;
        cursor += ch.len_utf8();
    }
    state.finish()?;
    if parts.is_empty() {
        Ok(None)
    } else {
        parts.push(text[start..].trim());
        Ok(Some(parts))
    }
}

fn owns_outer_group(text: &str) -> bool {
    if !text.starts_with('(') || !text.ends_with(')') {
        return false;
    }
    let mut state = ScanState::default();
    for (index, ch) in text.char_indices() {
        if state.advance(ch).is_err() {
            return false;
        }
        if ch == ')' && state.is_top_level() {
            return index + ch.len_utf8() == text.len();
        }
    }
    false
}

#[derive(Default)]
struct ScanState {
    depth: usize,
    in_string: bool,
    escaped: bool,
}

impl ScanState {
    const fn is_top_level(&self) -> bool {
        !self.in_string && self.depth == 0
    }

    fn advance(&mut self, ch: char) -> Result<(), ParseError> {
        if self.escaped {
            self.escaped = false;
            return Ok(());
        }
        if self.in_string {
            self.advance_string(ch);
            return Ok(());
        }
        match ch {
            '"' => self.in_string = true,
            '(' => self.depth += 1,
            ')' => self.depth = self.depth.checked_sub(1).ok_or(ParseError)?,
            _ => {}
        }
        Ok(())
    }

    fn advance_string(&mut self, ch: char) {
        match ch {
            '\\' => self.escaped = true,
            '"' => self.in_string = false,
            _ => {}
        }
    }

    const fn finish(&self) -> Result<(), ParseError> {
        if self.in_string || self.escaped || self.depth != 0 {
            Err(ParseError)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "track_filter_test.rs"]
mod tests;
