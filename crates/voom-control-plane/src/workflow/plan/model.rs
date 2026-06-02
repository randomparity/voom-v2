use std::collections::{HashMap, HashSet};

use serde_json::Value;
use voom_core::OperationKind;
use voom_plan::TargetRef;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkflowPlan {
    pub id: String,
    pub seed: u64,
    pub nodes: Vec<OperationNode>,
    pub fan_out: FanOutPolicy,
    pub concurrency: ConcurrencyPolicy,
    pub timing: TimingPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OperationNode {
    pub id: String,
    pub operation: OperationKind,
    pub policy_target: Option<TargetRef>,
    pub operation_payload: Value,
    pub depends_on: Vec<String>,
    pub depends_on_selected: Vec<String>,
    pub provides_selected: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FanOutPolicy {
    pub max_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ConcurrencyPolicy {
    pub max_in_flight_dispatches: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct TimingPolicy {
    pub base_duration_ms: u64,
    pub jitter_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPlanError {
    detail: String,
}

impl std::fmt::Display for WorkflowPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for WorkflowPlanError {}

impl WorkflowPlan {
    #[cfg(test)]
    #[must_use]
    pub fn default_ci() -> Self {
        Self {
            id: "sprint-2-phase-7-default".to_owned(),
            seed: 2,
            nodes: vec![
                operation("scan", OperationKind::ScanLibrary, &[]),
                operation("probe", OperationKind::ProbeFile, &["scan"]),
                operation("hash", OperationKind::HashFile, &["scan"]),
                operation("identity", OperationKind::IdentifyMedia, &["scan"]),
                operation("quality", OperationKind::ScoreQuality, &["probe"]),
                selected_operation("remux", OperationKind::Remux, &["quality"], "transform"),
                selected_operation(
                    "transcode",
                    OperationKind::TranscodeVideo,
                    &["quality"],
                    "transform",
                ),
                operation_after_selected("backup", OperationKind::BackUpFile, &["transform"]),
                operation_after_selected(
                    "external-sync",
                    OperationKind::SyncExternalSystem,
                    &["transform"],
                ),
                operation_after_selected("issue", OperationKind::CommitArtifact, &["transform"]),
                operation_after_selected("use-lease", OperationKind::EditTracks, &["transform"]),
                operation("verify", OperationKind::VerifyArtifact, &["backup"]),
            ],
            fan_out: FanOutPolicy { max_files: 3 },
            concurrency: ConcurrencyPolicy {
                max_in_flight_dispatches: 4,
            },
            timing: TimingPolicy {
                base_duration_ms: 25,
                jitter_ms: 10,
            },
        }
    }

    pub fn validate(&self) -> Result<(), WorkflowPlanError> {
        if self.fan_out.max_files == 0 {
            return Err(WorkflowPlanError::new(
                "fan_out.max_files must be greater than 0",
            ));
        }
        if self.concurrency.max_in_flight_dispatches == 0 {
            return Err(WorkflowPlanError::new(
                "concurrency.max_in_flight_dispatches must be greater than 0",
            ));
        }

        let mut seen = HashSet::new();
        let mut by_id = HashMap::new();
        for node in &self.nodes {
            let id = node.id();
            if !seen.insert(id) {
                return Err(WorkflowPlanError::new(format!("duplicate node id `{id}`")));
            }
            by_id.insert(id, node);
        }

        for node in &self.nodes {
            for dependency in node.depends_on() {
                if !by_id.contains_key(dependency.as_str()) {
                    return Err(WorkflowPlanError::new(format!(
                        "missing dependency `{dependency}` for node `{}`",
                        node.id()
                    )));
                }
            }
            for selected_dependency in node.depends_on_selected() {
                let providers = self
                    .nodes
                    .iter()
                    .filter(|provider| {
                        provider.provides_selected() == Some(selected_dependency.as_str())
                    })
                    .count();
                if providers == 0 {
                    return Err(WorkflowPlanError::new(format!(
                        "missing selected dependency group `{selected_dependency}` for node `{}`",
                        node.id()
                    )));
                }
            }
        }

        reject_cycles(&by_id)?;
        Ok(())
    }
}

impl OperationNode {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn operation(&self) -> OperationKind {
        self.operation
    }

    #[must_use]
    pub fn depends_on(&self) -> &[String] {
        &self.depends_on
    }

    #[must_use]
    pub fn depends_on_selected(&self) -> &[String] {
        &self.depends_on_selected
    }

    #[must_use]
    pub fn provides_selected(&self) -> Option<&str> {
        self.provides_selected.as_deref()
    }

    #[must_use]
    pub fn policy_target(&self) -> Option<&TargetRef> {
        self.policy_target.as_ref()
    }

    #[must_use]
    pub fn operation_payload(&self) -> &Value {
        &self.operation_payload
    }
}

impl WorkflowPlanError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
fn operation(id: &str, operation: OperationKind, depends_on: &[&str]) -> OperationNode {
    OperationNode {
        id: id.to_owned(),
        operation,
        policy_target: None,
        operation_payload: Value::Null,
        depends_on: depends_on.iter().map(ToString::to_string).collect(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    }
}

#[cfg(test)]
fn selected_operation(
    id: &str,
    operation: OperationKind,
    depends_on: &[&str],
    provides_selected: &str,
) -> OperationNode {
    OperationNode {
        id: id.to_owned(),
        operation,
        policy_target: None,
        operation_payload: Value::Null,
        depends_on: depends_on.iter().map(ToString::to_string).collect(),
        depends_on_selected: Vec::new(),
        provides_selected: Some(provides_selected.to_owned()),
    }
}

#[cfg(test)]
fn operation_after_selected(
    id: &str,
    operation: OperationKind,
    depends_on_selected: &[&str],
) -> OperationNode {
    OperationNode {
        id: id.to_owned(),
        operation,
        policy_target: None,
        operation_payload: Value::Null,
        depends_on: Vec::new(),
        depends_on_selected: depends_on_selected
            .iter()
            .map(ToString::to_string)
            .collect(),
        provides_selected: None,
    }
}

fn reject_cycles(nodes: &HashMap<&str, &OperationNode>) -> Result<(), WorkflowPlanError> {
    let selected_providers = selected_providers(nodes.values().copied());
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for id in nodes.keys().copied() {
        visit(id, nodes, &selected_providers, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn selected_providers<'a>(
    nodes: impl Iterator<Item = &'a OperationNode>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut providers: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes {
        if let Some(group) = node.provides_selected() {
            providers.entry(group).or_default().push(node.id());
        }
    }
    providers
}

fn visit<'a>(
    id: &'a str,
    nodes: &HashMap<&'a str, &'a OperationNode>,
    selected_providers: &HashMap<&'a str, Vec<&'a str>>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> Result<(), WorkflowPlanError> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(WorkflowPlanError::new(format!(
            "cycle detected through node `{id}`"
        )));
    }

    let Some(node) = nodes.get(id) else {
        return Ok(());
    };
    for dependency in node.depends_on() {
        visit(dependency, nodes, selected_providers, visiting, visited)?;
    }
    for selected_dependency in node.depends_on_selected() {
        if let Some(providers) = selected_providers.get(selected_dependency.as_str()) {
            for provider in providers {
                visit(provider, nodes, selected_providers, visiting, visited)?;
            }
        }
    }
    visiting.remove(id);
    visited.insert(id);
    Ok(())
}

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
