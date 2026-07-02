use std::collections::{BTreeMap, BTreeSet};

use crate::text::{
    is_single_value, list_values, quoted_value, setting_value, statement_text, text_after_list,
    text_after_quoted_value, words,
};
use crate::{DiagnosticCode, ExprAst, SourceSpan, StatementAst};

use super::conditions::is_valid_track_filter;
use super::{TagEffects, Validator};

impl Validator<'_> {
    pub(super) fn validate_nested_operation(
        &mut self,
        statement: &StatementAst,
        tag_effects: &mut TagEffects,
    ) {
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
                        if let Some(key) = self.validate_set_tag(statement, text.as_ref()) {
                            tag_effects.saw_set_tag = true;
                            tag_effects.set_tags.insert(key);
                        }
                    }
                    "delete_tag" => {
                        if let Some(key) = self.validate_delete_tag(statement, text.as_ref()) {
                            tag_effects.delete_tags.insert(key);
                        }
                    }
                    _ => {
                        self.validate_clear_tags(statement, text.as_ref());
                        self.record_clear_tags(tag_effects, statement.span());
                    }
                }
            }
            "when" => {
                let text = statement_text(statement);
                self.validate_condition(statement, text.as_ref(), tag_effects);
            }
            "transcode" => self.validate_transcode_statement(statement),
            "extract" => self.validate_extract_statement(statement),
            "verify" => self.validate_verify_statement(statement),
            "synthesize" => self.error(
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

    pub(super) fn record_clear_tags(&mut self, tag_effects: &mut TagEffects, span: SourceSpan) {
        if tag_effects.saw_set_tag {
            self.error(
                DiagnosticCode::TagOrderingError,
                span,
                "clear_tags must precede set_tag in a phase",
            );
        }
    }

    pub(super) fn validate_container(&mut self, statement: &StatementAst, text: &str) {
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

    pub(super) fn validate_track_operation(&mut self, statement: &StatementAst, text: &str) {
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

    pub(super) fn validate_order(&mut self, statement: &StatementAst, text: &str) {
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

    pub(super) fn validate_defaults(&mut self, statement: &StatementAst, text: &str) {
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

    pub(super) fn validate_actions(&mut self, statement: &StatementAst, text: &str) {
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

    pub(super) fn validate_on_error(&mut self, statement: &StatementAst, text: &str) {
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

    pub(super) fn validate_set_tag(
        &mut self,
        statement: &StatementAst,
        text: &str,
    ) -> Option<String> {
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

    pub(super) fn validate_delete_tag(
        &mut self,
        statement: &StatementAst,
        text: &str,
    ) -> Option<String> {
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

    pub(super) fn validate_clear_tags(&mut self, statement: &StatementAst, text: &str) {
        if words(text).len() > 1 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "clear_tags does not accept extra arguments",
            );
        }
    }

    pub(super) fn validate_transcode_statement(&mut self, statement: &StatementAst) {
        match statement {
            StatementAst::Raw { text, .. } => {
                let text = text.clone();
                self.validate_transcode_header(statement, &text);
            }
            StatementAst::TranscodeInline {
                header, settings, ..
            } => {
                if header.contains("using profile") {
                    self.error(
                        DiagnosticCode::UnsupportedTranscodeShape,
                        statement.span(),
                        "`using profile` and an inline body are mutually exclusive",
                    );
                    return;
                }
                let tokens = words(header);
                let Some(codec) = self.transcode_target_codec(statement, &tokens) else {
                    return;
                };
                self.validate_inline_video_profile(statement, codec, settings);
            }
            StatementAst::Block { .. } => self.error(
                DiagnosticCode::UnsupportedTranscodeShape,
                statement.span(),
                "transcode does not take a nested statement block",
            ),
        }
    }

    fn validate_transcode_header(&mut self, statement: &StatementAst, text: &str) {
        let tokens = words(text);
        if tokens.get(0..2) == Some(&["transcode", "video"]) {
            self.validate_transcode_video_header(statement, &tokens);
            return;
        }
        if tokens
            .get(0..4)
            .is_some_and(|prefix| matches!(prefix, ["transcode", "audio", "to", "aac" | "opus"]))
        {
            if self.validate_optional_track_filter(statement, text, 4) {
                self.validate_language_tokens(statement, text);
            }
            return;
        }
        self.error(
            DiagnosticCode::UnsupportedTranscodeShape,
            statement.span(),
            "unsupported transcode operation shape",
        );
    }

    fn validate_transcode_video_header(&mut self, statement: &StatementAst, tokens: &[&str]) {
        if self.transcode_target_codec(statement, tokens).is_none() {
            return;
        }
        match tokens.get(4..) {
            None | Some([]) => {}
            Some(["using", "profile", _name]) => {}
            Some(_) => self.error(
                DiagnosticCode::UnsupportedTranscodeShape,
                statement.span(),
                "unsupported transcode video header",
            ),
        }
    }

    fn transcode_target_codec(
        &mut self,
        statement: &StatementAst,
        tokens: &[&str],
    ) -> Option<&'static str> {
        let codec = tokens.get(3).copied();
        match codec {
            Some("hevc") => Some("hevc"),
            Some("av1") => Some("av1"),
            _ => {
                self.error(
                    DiagnosticCode::UnsupportedTranscodeShape,
                    statement.span(),
                    "transcode video target codec must be hevc or av1",
                );
                None
            }
        }
    }

    fn validate_inline_video_profile(
        &mut self,
        statement: &StatementAst,
        codec: &str,
        settings: &[crate::SettingAst],
    ) {
        let span = statement.span();
        if !self.validate_inline_setting_keys(span, settings) {
            return;
        }
        let by_key: BTreeMap<&str, &ExprAst> = settings
            .iter()
            .map(|setting| (setting.key.value.as_str(), &setting.value))
            .collect();

        let Some(encoder) = self.inline_required_str(span, &by_key, "encoder") else {
            return;
        };
        let Some(descriptor) = voom_core::encoder_descriptor(&encoder) else {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!("unknown encoder `{encoder}`"),
            );
            return;
        };
        if descriptor.target_codec != codec {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!(
                    "encoder `{encoder}` produces `{}`, not `{codec}`",
                    descriptor.target_codec
                ),
            );
        }
        self.validate_inline_crf_preset(span, descriptor, &by_key);
        self.validate_inline_optionals(span, descriptor, &by_key);
    }

    fn validate_inline_setting_keys(
        &mut self,
        span: SourceSpan,
        settings: &[crate::SettingAst],
    ) -> bool {
        const ALLOWED: &[&str] = &[
            "encoder",
            "crf",
            "preset",
            "tune",
            "codec_profile",
            "codec_level",
            "pixel_format",
            "max_width",
            "max_height",
            "output_container",
            "copy_compatible",
        ];
        let mut seen = BTreeSet::new();
        let mut ok = true;
        for setting in settings {
            let key = setting.key.value.as_str();
            if !ALLOWED.contains(&key) {
                self.error(
                    DiagnosticCode::InvalidVideoProfileSetting,
                    span,
                    format!("unknown inline profile setting `{key}`"),
                );
                ok = false;
            } else if !seen.insert(key) {
                self.error(
                    DiagnosticCode::InvalidVideoProfileSetting,
                    span,
                    format!("duplicate inline profile setting `{key}`"),
                );
                ok = false;
            }
        }
        ok
    }

    fn validate_inline_crf_preset(
        &mut self,
        span: SourceSpan,
        descriptor: &voom_core::EncoderDescriptor,
        by_key: &BTreeMap<&str, &ExprAst>,
    ) {
        if let Some(crf) = self.inline_required_str(span, by_key, "crf") {
            match crf.parse::<u8>() {
                Ok(value) if descriptor.accepts_crf(value) => {}
                _ => self.error(
                    DiagnosticCode::InvalidVideoProfileSetting,
                    span,
                    format!(
                        "crf `{crf}` outside {}..={} for `{}`",
                        descriptor.crf_min, descriptor.crf_max, descriptor.encoder
                    ),
                ),
            }
        }
        if let Some(preset) = self.inline_required_str(span, by_key, "preset")
            && !descriptor.accepts_preset(&preset)
        {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!("preset `{preset}` invalid for `{}`", descriptor.encoder),
            );
        }
    }

    fn validate_inline_optionals(
        &mut self,
        span: SourceSpan,
        descriptor: &voom_core::EncoderDescriptor,
        by_key: &BTreeMap<&str, &ExprAst>,
    ) {
        let tune = self.inline_optional_str(span, by_key, "tune");
        if let Some(tune) = &tune
            && !descriptor.accepts_tune(tune)
        {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!("tune `{tune}` invalid for `{}`", descriptor.encoder),
            );
        }
        let codec_profile = self.inline_optional_str(span, by_key, "codec_profile");
        if let Some(codec_profile) = &codec_profile
            && !descriptor.accepts_codec_profile(codec_profile)
        {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!(
                    "codec_profile `{codec_profile}` invalid for `{}`",
                    descriptor.encoder
                ),
            );
        }
        let codec_level = self.inline_optional_str(span, by_key, "codec_level");
        if let Some(codec_level) = &codec_level
            && !descriptor.accepts_codec_level(codec_level)
        {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!(
                    "codec_level `{codec_level}` invalid for `{}`",
                    descriptor.encoder
                ),
            );
        }
        let pixel_format = self.inline_optional_str(span, by_key, "pixel_format");
        self.validate_inline_pixel_format(
            span,
            descriptor,
            pixel_format.as_deref(),
            codec_profile.as_deref(),
        );
        self.validate_inline_container_and_dimensions(span, by_key);
    }

    fn validate_inline_pixel_format(
        &mut self,
        span: SourceSpan,
        descriptor: &voom_core::EncoderDescriptor,
        pixel_format: Option<&str>,
        codec_profile: Option<&str>,
    ) {
        let Some(pixel_format) = pixel_format else {
            return;
        };
        if !descriptor.accepts_pixel_format(pixel_format) {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!(
                    "pixel_format `{pixel_format}` invalid for `{}`",
                    descriptor.encoder
                ),
            );
        } else if !descriptor.pixel_format_compatible_with_profile(pixel_format, codec_profile) {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!(
                    "pixel_format `{pixel_format}` incompatible with codec_profile `{codec_profile:?}`"
                ),
            );
        }
    }

    fn validate_inline_container_and_dimensions(
        &mut self,
        span: SourceSpan,
        by_key: &BTreeMap<&str, &ExprAst>,
    ) {
        if let Some(container) = self.inline_optional_str(span, by_key, "output_container")
            && !matches!(container.as_str(), "mkv" | "mp4")
        {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!("output_container `{container}` must be mkv or mp4"),
            );
        }
        for key in ["max_width", "max_height"] {
            if let Some(expr) = by_key.get(key) {
                match expr {
                    ExprAst::Number(value) if value.value.parse::<u32>().is_ok_and(|n| n > 0) => {}
                    _ => self.error(
                        DiagnosticCode::InvalidVideoProfileSetting,
                        span,
                        format!("{key} must be a positive integer"),
                    ),
                }
            }
        }
        if let Some(expr) = by_key.get("copy_compatible")
            && !matches!(expr, ExprAst::Boolean(_))
        {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                "copy_compatible must be a boolean",
            );
        }
    }

    fn inline_required_str(
        &mut self,
        span: SourceSpan,
        by_key: &BTreeMap<&str, &ExprAst>,
        key: &str,
    ) -> Option<String> {
        let Some(expr) = by_key.get(key) else {
            self.error(
                DiagnosticCode::InvalidVideoProfileSetting,
                span,
                format!("inline profile is missing mandatory `{key}`"),
            );
            return None;
        };
        self.inline_expr_as_string(span, key, expr)
    }

    fn inline_optional_str(
        &mut self,
        span: SourceSpan,
        by_key: &BTreeMap<&str, &ExprAst>,
        key: &str,
    ) -> Option<String> {
        let expr = by_key.get(key)?;
        self.inline_expr_as_string(span, key, expr)
    }

    fn inline_expr_as_string(
        &mut self,
        span: SourceSpan,
        key: &str,
        expr: &ExprAst,
    ) -> Option<String> {
        match expr {
            ExprAst::Identifier(value)
            | ExprAst::String(value)
            | ExprAst::Number(value)
            | ExprAst::FieldPath(value) => Some(value.value.clone()),
            ExprAst::Boolean(_) | ExprAst::List { .. } => {
                self.error(
                    DiagnosticCode::InvalidVideoProfileSetting,
                    span,
                    format!("`{key}` must be a scalar value"),
                );
                None
            }
        }
    }

    pub(super) fn validate_extract_statement(&mut self, statement: &StatementAst) {
        let text = statement_text(statement);
        let tokens = words(text.as_ref());
        if tokens.get(0..2) == Some(&["extract", "audio"]) {
            if self.validate_optional_track_filter(statement, text.as_ref(), 2) {
                self.validate_language_tokens(statement, text.as_ref());
            }
            return;
        }
        self.error(
            DiagnosticCode::UnknownPhaseStatementOrOperation,
            statement.span(),
            "unsupported extract operation",
        );
    }

    /// Validate `verify artifact`. The spec production takes exactly the fixed
    /// `artifact` target and no further arguments; any other shape is an unknown
    /// operation rather than a deferred one.
    pub(super) fn validate_verify_statement(&mut self, statement: &StatementAst) {
        let text = statement_text(statement);
        let tokens = words(text.as_ref());
        if tokens.get(1).copied() != Some("artifact") {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "verify operation must use `verify artifact`",
            );
            return;
        }
        if tokens.len() > 2 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "verify artifact does not accept extra arguments",
            );
        }
    }

    /// Validate a track filter the spec brackets as optional (`transcode audio`,
    /// `extract audio`). A `where` clause, when present, must sit at
    /// `where_index` and carry a valid filter; when absent, the operation
    /// selects all audio tracks and no trailing tokens are permitted.
    fn validate_optional_track_filter(
        &mut self,
        statement: &StatementAst,
        text: &str,
        where_index: usize,
    ) -> bool {
        let tokens = words(text);
        if !text.contains(" where ") {
            if tokens.len() > where_index {
                self.error(
                    DiagnosticCode::UnknownPhaseStatementOrOperation,
                    statement.span(),
                    "operation does not accept extra arguments without `where`",
                );
                return false;
            }
            return true;
        }
        if tokens.get(where_index).copied() != Some("where") {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "track filter must follow the operation header",
            );
            return false;
        }
        let Some((_, filter)) = text.split_once(" where ") else {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "operation requires a track filter after `where`",
            );
            return false;
        };
        if !is_valid_track_filter(filter.trim()) {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "unknown track filter predicate",
            );
            return false;
        }
        true
    }

    pub(super) fn validate_rules(
        &mut self,
        statement: &StatementAst,
        text: &str,
        tag_effects: &mut TagEffects,
    ) {
        let tokens = words(text);
        let mode = tokens.get(1).copied().unwrap_or_default();
        if !matches!(mode, "first" | "all") {
            self.error(
                DiagnosticCode::InvalidRuleMatchMode,
                statement.span(),
                "rules mode must be first or all",
            );
        }
        if tokens.len() > 2 {
            self.error(
                DiagnosticCode::UnknownPhaseStatementOrOperation,
                statement.span(),
                "rules mode does not accept extra arguments",
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
                    self.validate_nested_operation(nested, tag_effects);
                }
            }
        }
    }
}
