use std::collections::BTreeMap;

use crate::text::{list_values, quoted_value, statement_text, text_after_quoted_value, words};
use crate::{
    DiagnosticCode, DiagnosticStage, ExprAst, PolicyDiagnostic, SourceSpan, StatementAst,
    line_column,
};

use super::super::compiled::{
    CompiledOperation, CompiledRule, DefaultStrategy, RuleMatchMode, TrackTarget,
};
use super::conditions::{
    compiled_value, condition_from_text, strip_quotes, track_filter, track_target,
};

pub(super) fn lower_operations(
    source: &str,
    statements: &[StatementAst],
) -> Result<Vec<CompiledOperation>, Vec<PolicyDiagnostic>> {
    let mut operations = Vec::with_capacity(statements.len());
    for statement in statements {
        operations.push(lower_operation(source, statement)?);
    }
    Ok(operations)
}

fn lower_operation(
    source: &str,
    statement: &StatementAst,
) -> Result<CompiledOperation, Vec<PolicyDiagnostic>> {
    if let StatementAst::TranscodeInline {
        header, settings, ..
    } = statement
    {
        return Ok(lower_transcode_inline(header, settings));
    }
    let text = statement_text(statement);
    let tokens = words(text.as_ref());
    let Some(keyword) = tokens.first().copied() else {
        return Err(vec![unknown_operation(source, statement.span())]);
    };
    match keyword {
        "container" => Ok(CompiledOperation::SetContainer {
            container: token_string(&tokens, 1, "mkv"),
        }),
        "keep" => Ok(CompiledOperation::KeepTracks {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            filter: track_filter(text.as_ref()),
        }),
        "remove" => Ok(CompiledOperation::RemoveTracks {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            filter: track_filter(text.as_ref()),
        }),
        "order" if tokens.get(1).copied() == Some("tracks") => {
            Ok(CompiledOperation::ReorderTracks {
                targets: list_values(text.as_ref())
                    .into_iter()
                    .filter_map(|target| track_target(Some(target)))
                    .collect(),
            })
        }
        "defaults" => Ok(CompiledOperation::SetDefaults {
            target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            strategy: default_strategy(tokens.get(2).copied()).unwrap_or(DefaultStrategy::First),
        }),
        "actions" if tokens.get(2).copied() == Some("clear") => {
            Ok(CompiledOperation::ClearTrackActions {
                target: track_target(tokens.get(1).copied()).unwrap_or(TrackTarget::Audio),
            })
        }
        "clear_tags" => Ok(CompiledOperation::ClearTags),
        "set_tag" => Ok(CompiledOperation::SetTag {
            key: quoted_value(text.as_ref()).unwrap_or_default(),
            value: compiled_value(text_after_quoted_value(text.as_ref()).unwrap_or_default()),
        }),
        "delete_tag" => Ok(CompiledOperation::DeleteTag {
            key: quoted_value(text.as_ref()).unwrap_or_default(),
        }),
        "transcode" if tokens.get(1).copied() == Some("audio") => {
            Ok(CompiledOperation::TranscodeAudio {
                target_codec: token_string(&tokens, 3, "opus"),
                container: "mkv".to_owned(),
                filter: track_filter(text.as_ref()),
            })
        }
        "transcode" => Ok(lower_transcode_raw(&tokens)),
        "extract" if tokens.get(1).copied() == Some("audio") => {
            Ok(CompiledOperation::ExtractAudio {
                target_codec: "opus".to_owned(),
                container: "ogg".to_owned(),
                filter: track_filter(text.as_ref()),
            })
        }
        "when" => Ok(CompiledOperation::Conditional {
            condition: condition_from_text(text.as_ref().trim_start_matches("when").trim()),
            operations: match statement {
                StatementAst::Block { statements, .. } => lower_operations(source, statements)?,
                StatementAst::Raw { .. } | StatementAst::TranscodeInline { .. } => Vec::new(),
            },
        }),
        "rules" => Ok(CompiledOperation::Rules {
            mode: rule_match_mode(tokens.get(1).copied()).unwrap_or(RuleMatchMode::First),
            rules: lower_rules(source, statement)?,
        }),
        _ => Err(vec![unknown_operation(source, statement.span())]),
    }
}

fn lower_transcode_raw(tokens: &[&str]) -> CompiledOperation {
    let codec = tokens.get(3).copied().unwrap_or("hevc").to_owned();
    let profile = match tokens.get(4..) {
        Some(["using", "profile", name]) => crate::VideoProfileRef::Named(strip_quotes(name)),
        _ => crate::VideoProfileRef::Named(format!("default-{codec}")),
    };
    CompiledOperation::TranscodeVideo {
        target_codec: codec,
        container: "mkv".to_owned(),
        profile,
        resolved_profile: None,
    }
}

fn lower_transcode_inline(header: &str, settings: &[crate::SettingAst]) -> CompiledOperation {
    let tokens = words(header);
    let codec = tokens.get(3).copied().unwrap_or("hevc").to_owned();
    let inline = inline_settings_from(settings);
    let container = inline
        .output_container
        .clone()
        .unwrap_or_else(|| "mkv".to_owned());
    CompiledOperation::TranscodeVideo {
        target_codec: codec,
        container,
        profile: crate::VideoProfileRef::Inline(inline),
        resolved_profile: None,
    }
}

fn inline_settings_from(settings: &[crate::SettingAst]) -> crate::VideoProfileSettings {
    let mut by_key = BTreeMap::new();
    for setting in settings {
        by_key.insert(setting.key.value.as_str(), &setting.value);
    }
    let str_at = |key: &str| by_key.get(key).map(|expr| expr_scalar_string(expr));
    let u32_at = |key: &str| str_at(key).and_then(|value| value.parse::<u32>().ok());
    crate::VideoProfileSettings {
        encoder: str_at("encoder").unwrap_or_default(),
        crf: str_at("crf")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default(),
        preset: str_at("preset").unwrap_or_default(),
        tune: str_at("tune"),
        codec_profile: str_at("codec_profile"),
        codec_level: str_at("codec_level"),
        pixel_format: str_at("pixel_format"),
        max_width: u32_at("max_width"),
        max_height: u32_at("max_height"),
        output_container: str_at("output_container"),
        copy_compatible: by_key.get("copy_compatible").and_then(|expr| match expr {
            ExprAst::Boolean(value) => Some(value.value),
            _ => None,
        }),
    }
}

fn expr_scalar_string(expr: &ExprAst) -> String {
    match expr {
        ExprAst::String(value)
        | ExprAst::Identifier(value)
        | ExprAst::Number(value)
        | ExprAst::FieldPath(value) => value.value.clone(),
        ExprAst::Boolean(value) => value.value.to_string(),
        ExprAst::List { .. } => String::new(),
    }
}

fn lower_rules(
    source: &str,
    statement: &StatementAst,
) -> Result<Vec<CompiledRule>, Vec<PolicyDiagnostic>> {
    let StatementAst::Block { statements, .. } = statement else {
        return Ok(Vec::new());
    };
    let mut rules = Vec::with_capacity(statements.len());
    for rule in statements {
        let StatementAst::Block {
            name, statements, ..
        } = rule
        else {
            return Err(vec![unknown_operation(source, rule.span())]);
        };
        let mut condition = None;
        let mut operations = Vec::new();
        for nested in statements {
            if nested.keyword().value == "when" {
                let text = statement_text(nested);
                condition = Some(condition_from_text(text.trim_start_matches("when").trim()));
                if let StatementAst::Block { statements, .. } = nested {
                    operations.extend(lower_operations(source, statements)?);
                }
            } else {
                operations.push(lower_operation(source, nested)?);
            }
        }
        rules.push(CompiledRule {
            name: name
                .as_ref()
                .map_or_else(String::new, |name| strip_quotes(&name.value)),
            condition,
            operations,
        });
    }
    Ok(rules)
}

fn default_strategy(token: Option<&str>) -> Option<DefaultStrategy> {
    match token {
        Some("first") => Some(DefaultStrategy::First),
        Some("best") => Some(DefaultStrategy::Best),
        Some("none") => Some(DefaultStrategy::None),
        Some("preserve") => Some(DefaultStrategy::Preserve),
        _ => None,
    }
}

fn rule_match_mode(token: Option<&str>) -> Option<RuleMatchMode> {
    match token {
        Some("first") => Some(RuleMatchMode::First),
        Some("all") => Some(RuleMatchMode::All),
        _ => None,
    }
}

fn token_string(tokens: &[&str], index: usize, fallback: &str) -> String {
    tokens
        .get(index)
        .map_or(fallback, |value| *value)
        .to_owned()
}

fn unknown_operation(source: &str, span: SourceSpan) -> PolicyDiagnostic {
    PolicyDiagnostic::error(
        DiagnosticCode::UnknownPhaseStatementOrOperation,
        DiagnosticStage::Compile,
        span,
        line_column(source, span.start),
        "unknown phase statement or operation",
    )
}
