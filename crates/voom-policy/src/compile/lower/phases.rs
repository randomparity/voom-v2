use std::collections::{BTreeMap, BTreeSet};

use crate::text::{dependency_values, setting_value, statement_text};
use crate::{ExprAst, PolicyAst, PolicyDiagnostic, StatementAst};

use super::super::compiled::{CompiledCondition, CompiledPhase, ErrorStrategy};
use super::conditions::condition_from_text;
use super::operations::lower_operations;

pub(super) fn lower_phases(
    source: &str,
    ast: &PolicyAst,
) -> Result<Vec<CompiledPhase>, Vec<PolicyDiagnostic>> {
    let mut phases = Vec::with_capacity(ast.phases.len());
    for phase in &ast.phases {
        phases.push(CompiledPhase {
            name: phase.name.value.clone(),
            depends_on: phase_dependencies(&phase.controls),
            run_if: phase_run_if(&phase.controls),
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

pub(super) fn config_map(statements: &[StatementAst]) -> BTreeMap<String, serde_json::Value> {
    statements
        .iter()
        .map(|statement| {
            let text = statement_text(statement);
            (
                statement.keyword().value.clone(),
                serde_json::Value::String(text.into_owned()),
            )
        })
        .collect()
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

fn phase_run_if(controls: &[StatementAst]) -> Option<CompiledCondition> {
    controls
        .iter()
        .find(|control| control.keyword().value == "run_if")
        .map(|control| {
            let text = statement_text(control);
            CompiledCondition::Predicate {
                name: text.trim_start_matches("run_if").trim().to_owned(),
            }
        })
}

fn phase_skip_if(controls: &[StatementAst]) -> Option<CompiledCondition> {
    controls
        .iter()
        .find(|control| control.keyword().value == "skip")
        .map(|control| {
            let text = statement_text(control);
            condition_from_text(
                text.trim_start_matches("skip")
                    .trim_start_matches("when")
                    .trim(),
            )
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
