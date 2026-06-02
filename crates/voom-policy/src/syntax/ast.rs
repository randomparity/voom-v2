#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: crate::SourceSpan,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyAst {
    pub name: Spanned<String>,
    pub extends: Option<Spanned<String>>,
    pub metadata: Vec<SettingAst>,
    pub config: Vec<StatementAst>,
    pub phases: Vec<PhaseAst>,
    pub unknown_top_level: Vec<StatementAst>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseAst {
    pub name: Spanned<String>,
    pub controls: Vec<StatementAst>,
    pub operations: Vec<StatementAst>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingAst {
    pub key: Spanned<String>,
    pub value: ExprAst,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprAst {
    String(Spanned<String>),
    Identifier(Spanned<String>),
    Number(Spanned<String>),
    Boolean(Spanned<bool>),
    List {
        values: Vec<ExprAst>,
        span: crate::SourceSpan,
    },
    FieldPath(Spanned<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementAst {
    Raw {
        keyword: Spanned<String>,
        text: String,
        span: crate::SourceSpan,
    },
    Block {
        keyword: Spanned<String>,
        name: Option<Spanned<String>>,
        statements: Vec<StatementAst>,
        span: crate::SourceSpan,
    },
    /// A `transcode ... { key: value ... }` statement whose brace body is a list
    /// of typed settings (reusing the metadata `SettingAst` form), not nested
    /// statements.
    TranscodeInline {
        keyword: Spanned<String>,
        /// The header text before the `{`, e.g. `transcode video to av1`.
        header: String,
        settings: Vec<SettingAst>,
        span: crate::SourceSpan,
    },
}

impl StatementAst {
    #[must_use]
    pub const fn span(&self) -> crate::SourceSpan {
        match self {
            Self::Raw { span, .. }
            | Self::Block { span, .. }
            | Self::TranscodeInline { span, .. } => *span,
        }
    }

    #[must_use]
    pub const fn keyword(&self) -> &Spanned<String> {
        match self {
            Self::Raw { keyword, .. }
            | Self::Block { keyword, .. }
            | Self::TranscodeInline { keyword, .. } => keyword,
        }
    }
}

#[cfg(test)]
#[path = "ast_test.rs"]
mod tests;
