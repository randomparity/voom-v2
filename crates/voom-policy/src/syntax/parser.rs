use crate::{
    DiagnosticCode, DiagnosticStage, ExprAst, PhaseAst, PolicyAst, PolicyDiagnostic, SettingAst,
    SourceSpan, Spanned, StatementAst, line_column,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub diagnostics: Vec<PolicyDiagnostic>,
}

#[must_use]
fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

#[must_use]
fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

pub fn parse_policy_source(source: &str) -> Result<PolicyAst, ParseError> {
    Parser::new(source).parse_policy()
}

/// Maximum block-statement nesting depth. Recursive descent takes one stack
/// frame per level, so an unbounded depth lets pathological input
/// (`when { when { … } }` thousands deep) exhaust the stack and abort the
/// process. 64 is far above any legitimate policy.
const MAX_NESTING_DEPTH: usize = 64;

struct Parser<'a> {
    source: &'a str,
    cursor: usize,
    /// Block-statement nesting level. Incremented for each `inner` parser
    /// spawned to parse a block body; checked in `parse_statement`.
    depth: usize,
}

impl<'a> Parser<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor: 0,
            depth: 0,
        }
    }

    fn parse_policy(mut self) -> Result<PolicyAst, ParseError> {
        self.skip_ws_and_comments();
        self.expect_keyword("policy")?;
        let name = self.parse_string()?;
        self.skip_ws_and_comments();
        self.expect_byte(b'{')?;

        let mut ast = PolicyAst {
            name,
            extends: None,
            metadata: Vec::new(),
            config: Vec::new(),
            phases: Vec::new(),
            unknown_top_level: Vec::new(),
        };

        loop {
            self.skip_ws_and_comments();
            if self.consume_byte(b'}') {
                break;
            }
            if self.is_eof() {
                return Err(self.error_at(self.cursor, "expected `}` to close policy block"));
            }

            let Some(keyword) = self.peek_identifier() else {
                return Err(self.error_at(self.cursor, "expected top-level policy item"));
            };

            match keyword.value.as_str() {
                "extends" => {
                    self.cursor = keyword.span.end;
                    ast.extends = Some(self.parse_string()?);
                }
                "metadata" => {
                    self.cursor = keyword.span.end;
                    ast.metadata = self.parse_metadata_block()?;
                }
                "config" => {
                    self.cursor = keyword.span.end;
                    ast.config = self.parse_statement_block()?;
                }
                "phase" => ast.phases.push(self.parse_phase()?),
                _ => ast.unknown_top_level.push(self.parse_statement()?),
            }
        }

        self.skip_ws_and_comments();
        if self.is_eof() {
            Ok(ast)
        } else {
            Err(self.error_at(self.cursor, "unexpected content after policy block"))
        }
    }

    fn parse_phase(&mut self) -> Result<PhaseAst, ParseError> {
        self.expect_keyword("phase")?;
        let name = self.parse_identifier()?;
        self.skip_ws_and_comments();
        self.expect_byte(b'{')?;

        let mut controls = Vec::new();
        let mut operations = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.consume_byte(b'}') {
                break;
            }
            if self.is_eof() {
                return Err(self.error_at(self.cursor, "expected `}` to close phase block"));
            }

            let statement = self.parse_statement()?;
            if is_phase_control(statement.keyword().value.as_str()) {
                controls.push(statement);
            } else {
                operations.push(statement);
            }
        }

        Ok(PhaseAst {
            name,
            controls,
            operations,
        })
    }

    fn parse_metadata_block(&mut self) -> Result<Vec<SettingAst>, ParseError> {
        self.skip_ws_and_comments();
        self.expect_byte(b'{')?;
        self.parse_setting_list()
    }

    /// Parses a `key: value` setting list up to and through the closing `}`.
    /// Assumes the opening `{` has already been consumed.
    fn parse_setting_list(&mut self) -> Result<Vec<SettingAst>, ParseError> {
        let mut settings = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.consume_byte(b'}') {
                break;
            }
            if self.is_eof() {
                return Err(self.error_at(self.cursor, "expected `}` to close settings block"));
            }

            let key = self.parse_identifier()?;
            self.skip_ws_and_comments();
            self.expect_byte(b':')?;
            let value = self.parse_expr()?;
            settings.push(SettingAst { key, value });
        }
        Ok(settings)
    }

    fn parse_statement_block(&mut self) -> Result<Vec<StatementAst>, ParseError> {
        self.skip_ws_and_comments();
        self.expect_byte(b'{')?;
        let mut statements = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.consume_byte(b'}') {
                break;
            }
            if self.is_eof() {
                return Err(self.error_at(self.cursor, "expected `}` to close block"));
            }
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn parse_statement(&mut self) -> Result<StatementAst, ParseError> {
        if self.depth > MAX_NESTING_DEPTH {
            return Err(self.error_code_at(
                DiagnosticCode::NestingDepthExceeded,
                self.cursor,
                format!("block nesting too deep (max {MAX_NESTING_DEPTH})"),
            ));
        }
        self.skip_ws_and_comments();
        let start = self.cursor;
        let keyword = self.parse_identifier()?;
        let mut idx = self.cursor;
        let mut nested = 0usize;
        let mut block_start = None;
        let mut block_end = None;

        while idx < self.source.len() {
            if nested == 0
                && idx > self.cursor
                && self.starts_statement_keyword_at(idx, keyword.value.as_str())
            {
                break;
            }
            match self.source.as_bytes()[idx] {
                b'"' => idx = self.skip_string_at(idx)?,
                b'/' if self.source.as_bytes().get(idx + 1) == Some(&b'/') && nested == 0 => break,
                b'\n' | b'}' if nested == 0 => break,
                b'{' => {
                    if nested == 0 {
                        block_start = Some(idx);
                    }
                    nested += 1;
                    idx += 1;
                }
                b'}' => {
                    nested -= 1;
                    idx += 1;
                    if nested == 0 {
                        block_end = Some(idx - 1);
                        break;
                    }
                }
                _ => idx += 1,
            }
        }

        let end = idx;
        self.cursor = idx;
        self.consume_line_comment();
        let text = self.source[start..end].trim().to_owned();
        let span = SourceSpan::new(start, end);

        if let Some(open_brace) = block_start {
            let close_brace = block_end.ok_or_else(|| {
                self.error_at(open_brace, "expected `}` to close statement block")
            })?;
            if keyword.value == "transcode" {
                let header = self.source[start..open_brace].trim().to_owned();
                let mut inner = Self {
                    source: self.source,
                    cursor: open_brace + 1,
                    depth: self.depth + 1,
                };
                let settings = inner.parse_setting_list()?;
                return Ok(StatementAst::TranscodeInline {
                    keyword,
                    header,
                    settings,
                    span,
                });
            }
            let name = self.statement_block_name(keyword.span.end, open_brace);
            let mut inner = Self {
                source: self.source,
                cursor: open_brace + 1,
                depth: self.depth + 1,
            };
            let statements = inner.parse_statements_until(close_brace)?;
            Ok(StatementAst::Block {
                keyword,
                name,
                statements,
                span,
            })
        } else {
            Ok(StatementAst::Raw {
                keyword,
                text,
                span,
            })
        }
    }

    fn parse_statements_until(
        &mut self,
        close_brace: usize,
    ) -> Result<Vec<StatementAst>, ParseError> {
        let mut statements = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.cursor >= close_brace {
                break;
            }
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn statement_block_name(
        &self,
        after_keyword: usize,
        open_brace: usize,
    ) -> Option<Spanned<String>> {
        let text = self.source[after_keyword..open_brace].trim();
        if text.is_empty() {
            return None;
        }
        let offset = self.source[after_keyword..open_brace].find(text)?;
        let start = after_keyword + offset;
        Some(Spanned {
            value: text.to_owned(),
            span: SourceSpan::new(start, start + text.len()),
        })
    }

    fn parse_expr(&mut self) -> Result<ExprAst, ParseError> {
        self.skip_ws_and_comments();
        if self.peek_byte() == Some(b'"') {
            return self.parse_string().map(ExprAst::String);
        }
        if self.peek_byte() == Some(b'[') {
            return self.parse_list();
        }

        let start = self.cursor;
        while let Some(byte) = self.peek_byte() {
            if byte.is_ascii_whitespace() || matches!(byte, b',' | b']' | b'}') {
                break;
            }
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(self.error_at(start, "expected value"));
        }
        let text = self.source[start..self.cursor].to_owned();
        let span = SourceSpan::new(start, self.cursor);
        match text.as_str() {
            "true" => Ok(ExprAst::Boolean(Spanned { value: true, span })),
            "false" => Ok(ExprAst::Boolean(Spanned { value: false, span })),
            _ if text.bytes().all(|byte| byte.is_ascii_digit()) => {
                Ok(ExprAst::Number(Spanned { value: text, span }))
            }
            _ if text.contains('.') => Ok(ExprAst::FieldPath(Spanned { value: text, span })),
            _ => Ok(ExprAst::Identifier(Spanned { value: text, span })),
        }
    }

    fn parse_list(&mut self) -> Result<ExprAst, ParseError> {
        let start = self.cursor;
        self.expect_byte(b'[')?;
        let mut values = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.consume_byte(b']') {
                break;
            }
            if self.is_eof() {
                return Err(self.error_at(self.cursor, "expected `]` to close list"));
            }
            values.push(self.parse_expr()?);
            self.skip_ws_and_comments();
            let _ = self.consume_byte(b',');
        }
        Ok(ExprAst::List {
            values,
            span: SourceSpan::new(start, self.cursor),
        })
    }

    fn parse_string(&mut self) -> Result<Spanned<String>, ParseError> {
        self.skip_ws_and_comments();
        let start = self.cursor;
        self.expect_byte(b'"')?;
        let value_start = self.cursor;
        while let Some(byte) = self.peek_byte() {
            match byte {
                b'\\' => self.cursor = self.cursor.saturating_add(2),
                b'"' => {
                    let value = self.source[value_start..self.cursor].to_owned();
                    self.cursor += 1;
                    return Ok(Spanned {
                        value,
                        span: SourceSpan::new(start, self.cursor),
                    });
                }
                _ => self.cursor += 1,
            }
        }
        Err(self.error_at(start, "unterminated string"))
    }

    fn parse_identifier(&mut self) -> Result<Spanned<String>, ParseError> {
        self.skip_ws_and_comments();
        self.peek_identifier()
            .inspect(|ident| {
                self.cursor = ident.span.end;
            })
            .ok_or_else(|| self.error_at(self.cursor, "expected identifier"))
    }

    fn peek_identifier(&self) -> Option<Spanned<String>> {
        let start = self.cursor;
        let first = *self.source.as_bytes().get(start)?;
        if !is_ident_start(first) {
            return None;
        }
        let mut end = start + 1;
        while let Some(byte) = self.source.as_bytes().get(end).copied() {
            if !is_ident_continue(byte) {
                break;
            }
            end += 1;
        }
        Some(Spanned {
            value: self.source[start..end].to_owned(),
            span: SourceSpan::new(start, end),
        })
    }

    fn expect_keyword(&mut self, expected: &str) -> Result<(), ParseError> {
        let ident = self.parse_identifier()?;
        if ident.value == expected {
            Ok(())
        } else {
            Err(self.error_at(ident.span.start, format!("expected `{expected}`")))
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), ParseError> {
        self.skip_ws_and_comments();
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(self.error_at(self.cursor, format!("expected `{}`", char::from(expected))))
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self
                .peek_byte()
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                self.cursor += 1;
            }
            let comment = self.peek_byte() == Some(b'/')
                && self.source.as_bytes().get(self.cursor + 1) == Some(&b'/');
            if !comment {
                break;
            }
            self.cursor += 2;
            while self.peek_byte().is_some_and(|byte| byte != b'\n') {
                self.cursor += 1;
            }
        }
    }

    fn consume_line_comment(&mut self) {
        if self.peek_byte() == Some(b'/')
            && self.source.as_bytes().get(self.cursor + 1) == Some(&b'/')
        {
            while self.peek_byte().is_some_and(|byte| byte != b'\n') {
                self.cursor += 1;
            }
        }
    }

    fn skip_string_at(&self, start: usize) -> Result<usize, ParseError> {
        let mut idx = start + 1;
        while idx < self.source.len() {
            match self.source.as_bytes()[idx] {
                b'\\' => idx = idx.saturating_add(2),
                b'"' => return Ok(idx + 1),
                _ => idx += 1,
            }
        }
        Err(self.error_at(start, "unterminated string"))
    }

    fn starts_statement_keyword_at(&self, idx: usize, current_keyword: &str) -> bool {
        if !self
            .source
            .as_bytes()
            .get(idx.wrapping_sub(1))
            .is_some_and(u8::is_ascii_whitespace)
        {
            return false;
        }
        let Some(first) = self.source.as_bytes().get(idx).copied() else {
            return false;
        };
        if !is_ident_start(first) {
            return false;
        }
        let mut end = idx + 1;
        while let Some(byte) = self.source.as_bytes().get(end).copied() {
            if !is_ident_continue(byte) {
                break;
            }
            end += 1;
        }
        let candidate = &self.source[idx..end];
        if current_keyword == "skip" && candidate == "when" {
            return false;
        }
        matches!(
            candidate,
            "depends_on"
                | "skip"
                | "run_if"
                | "on_error"
                | "container"
                | "keep"
                | "remove"
                | "order"
                | "defaults"
                | "actions"
                | "clear_tags"
                | "set_tag"
                | "delete_tag"
                | "when"
                | "rules"
                | "rule"
                | "extend"
                | "transcode"
                | "synthesize"
                | "verify"
        )
    }

    fn consume_byte(&mut self, byte: u8) -> bool {
        if self.peek_byte() == Some(byte) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.source.as_bytes().get(self.cursor).copied()
    }

    const fn is_eof(&self) -> bool {
        self.cursor >= self.source.len()
    }

    fn error_at(&self, offset: usize, message: impl Into<String>) -> ParseError {
        self.error_code_at(DiagnosticCode::UnexpectedToken, offset, message)
    }

    fn error_code_at(
        &self,
        code: DiagnosticCode,
        offset: usize,
        message: impl Into<String>,
    ) -> ParseError {
        let span = SourceSpan::new(offset, offset.saturating_add(1).min(self.source.len()));
        ParseError {
            diagnostics: vec![PolicyDiagnostic::error(
                code,
                DiagnosticStage::Parse,
                span,
                line_column(self.source, offset),
                message,
            )],
        }
    }
}

#[must_use]
fn is_phase_control(keyword: &str) -> bool {
    matches!(keyword, "depends_on" | "skip" | "run_if" | "on_error")
}

#[cfg(test)]
#[path = "parser_test.rs"]
mod tests;
