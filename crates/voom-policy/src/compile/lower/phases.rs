use std::collections::{BTreeMap, BTreeSet};

use crate::text::{dependency_values, setting_value, statement_text};
use crate::{
    DiagnosticCode, DiagnosticStage, ExprAst, PolicyAst, PolicyDiagnostic, StatementAst,
    line_column,
};

use super::super::compiled::{
    CompiledCondition, CompiledConfig, CompiledPhase, CompiledRunIf, ErrorStrategy,
};
use super::conditions::condition_from_text;
use super::operations::lower_operations;

pub(super) fn lower_phases(
    source: &str,
    ast: &PolicyAst,
    phase_order: &[String],
) -> Result<Vec<CompiledPhase>, Vec<PolicyDiagnostic>> {
    let mut phases = Vec::with_capacity(ast.phases.len());
    for phase in &ast.phases {
        phases.push(CompiledPhase {
            name: phase.name.value.clone(),
            depends_on: phase_dependencies(&phase.controls),
            run_if: phase_run_if(source, phase, phase_order)?,
            skip_if: phase_skip_if(&phase.controls),
            on_error: phase_on_error(&phase.controls),
            operations: lower_operations(source, &phase.operations)?,
        });
    }
    Ok(phases)
}

pub(super) fn metadata_map(settings: &[crate::SettingAst]) -> BTreeMap<String, serde_json::Value> {
    settings
        .iter()
        .map(|setting| (setting.key.value.clone(), expr_json(&setting.value)))
        .collect()
}

pub(super) fn compiled_config(settings: &[crate::SettingAst]) -> CompiledConfig {
    let mut config = CompiledConfig::default();
    for setting in settings {
        match (&*setting.key.value, &setting.value) {
            ("languages", ExprAst::List { values, .. }) => {
                config.languages = values
                    .iter()
                    .filter_map(|value| match value {
                        ExprAst::String(value) => Some(value.value.clone()),
                        _ => None,
                    })
                    .collect();
            }
            ("on_error", ExprAst::Identifier(value)) => {
                config.on_error = error_strategy(Some(&value.value));
            }
            _ => {}
        }
    }
    config
}

pub(super) fn phase_order(ast: &PolicyAst) -> Vec<String> {
    let dependencies_by_phase = ast
        .phases
        .iter()
        .map(|phase| {
            (
                phase.name.value.as_str(),
                phase_dependencies(&phase.controls),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut visited = BTreeSet::new();
    let mut order = Vec::with_capacity(ast.phases.len());

    for phase in &ast.phases {
        visit_phase(
            phase.name.value.as_str(),
            &dependencies_by_phase,
            &mut visited,
            &mut order,
        );
    }

    order
}

fn expr_json(expr: &ExprAst) -> serde_json::Value {
    match expr {
        ExprAst::String(value) | ExprAst::Identifier(value) | ExprAst::Number(value) => {
            serde_json::Value::String(value.value.clone())
        }
        ExprAst::Boolean(value) => serde_json::Value::Bool(value.value),
        ExprAst::FieldPath(value) => serde_json::Value::String(value.value.clone()),
        ExprAst::List { values, .. } => {
            serde_json::Value::Array(values.iter().map(expr_json).collect())
        }
    }
}

fn phase_dependencies(controls: &[StatementAst]) -> Vec<String> {
    controls
        .iter()
        .filter(|control| control.keyword().value == "depends_on")
        .flat_map(|control| {
            let text = statement_text(control);
            dependency_values(text.as_ref())
        })
        .collect()
}

fn visit_phase(
    name: &str,
    dependencies_by_phase: &BTreeMap<&str, Vec<String>>,
    visited: &mut BTreeSet<String>,
    order: &mut Vec<String>,
) {
    if !visited.insert(name.to_owned()) {
        return;
    }
    if let Some(dependencies) = dependencies_by_phase.get(name) {
        for dependency in dependencies {
            if dependencies_by_phase.contains_key(dependency.as_str()) {
                visit_phase(dependency, dependencies_by_phase, visited, order);
            }
        }
    }
    order.push(name.to_owned());
}

fn phase_run_if(
    source: &str,
    phase: &crate::PhaseAst,
    phase_order: &[String],
) -> Result<Option<CompiledRunIf>, Vec<PolicyDiagnostic>> {
    let Some(control) = phase
        .controls
        .iter()
        .find(|control| control.keyword().value == "run_if")
    else {
        return Ok(None);
    };
    let text = statement_text(control);
    let name = text.trim_start_matches("run_if").trim();
    let gate = CompiledRunIf::parse(name).map_err(|message| {
        vec![PolicyDiagnostic::error(
            DiagnosticCode::InvalidRunIfTrigger,
            DiagnosticStage::Compile,
            control.span(),
            line_column(source, control.span().start),
            message,
        )]
    })?;
    let current_index = phase_order
        .iter()
        .position(|name| name == &phase.name.value);
    let referenced_index = phase_order.iter().position(|name| name == &gate.phase);
    if referenced_index
        .zip(current_index)
        .is_none_or(|(referenced, current)| referenced >= current)
    {
        return Err(vec![PolicyDiagnostic::error(
            DiagnosticCode::UnknownPhaseReference,
            DiagnosticStage::Compile,
            control.span(),
            line_column(source, control.span().start),
            "run_if must reference a predecessor in phase order",
        )]);
    }
    Ok(Some(gate))
}

fn phase_skip_if(controls: &[StatementAst]) -> Option<CompiledCondition> {
    controls
        .iter()
        .find(|control| control.keyword().value == "skip")
        .map(|control| {
            let text = statement_text(control);
            let condition = text.strip_prefix("skip").unwrap_or(text.as_ref()).trim();
            let condition = condition.strip_prefix("when").unwrap_or(condition).trim();
            condition_from_text(condition)
        })
}

fn phase_on_error(controls: &[StatementAst]) -> Option<ErrorStrategy> {
    controls
        .iter()
        .find(|control| control.keyword().value == "on_error")
        .and_then(|control| {
            let text = statement_text(control);
            error_strategy(setting_value(text.as_ref()))
        })
}

fn error_strategy(token: Option<&str>) -> Option<ErrorStrategy> {
    match token {
        Some("abort") => Some(ErrorStrategy::Abort),
        Some("continue") => Some(ErrorStrategy::Continue),
        Some("skip") => Some(ErrorStrategy::Skip),
        _ => None,
    }
}
