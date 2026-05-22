use std::collections::{HashMap, HashSet};

use voom_worker_protocol::OperationKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPlan {
    pub id: String,
    pub seed: u64,
    pub nodes: Vec<WorkflowNode>,
    pub fan_out: FanOutPolicy,
    pub concurrency: ConcurrencyPolicy,
    pub timing: TimingPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowNode {
    Operation(OperationNode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationNode {
    pub id: String,
    pub operation: OperationKind,
    pub depends_on: Vec<String>,
    pub depends_on_selected: Vec<String>,
    pub provides_selected: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanOutPolicy {
    pub max_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyPolicy {
    pub max_in_flight_dispatches: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl WorkflowNode {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Operation(node) => &node.id,
        }
    }

    #[must_use]
    pub fn operation(&self) -> OperationKind {
        match self {
            Self::Operation(node) => node.operation,
        }
    }

    #[must_use]
    pub fn depends_on(&self) -> &[String] {
        match self {
            Self::Operation(node) => &node.depends_on,
        }
    }

    #[must_use]
    pub fn depends_on_selected(&self) -> &[String] {
        match self {
            Self::Operation(node) => &node.depends_on_selected,
        }
    }

    #[must_use]
    pub fn provides_selected(&self) -> Option<&str> {
        match self {
            Self::Operation(node) => node.provides_selected.as_deref(),
        }
    }
}

impl WorkflowPlanError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

fn operation(id: &str, operation: OperationKind, depends_on: &[&str]) -> WorkflowNode {
    WorkflowNode::Operation(OperationNode {
        id: id.to_owned(),
        operation,
        depends_on: depends_on.iter().map(ToString::to_string).collect(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    })
}

fn selected_operation(
    id: &str,
    operation: OperationKind,
    depends_on: &[&str],
    provides_selected: &str,
) -> WorkflowNode {
    WorkflowNode::Operation(OperationNode {
        id: id.to_owned(),
        operation,
        depends_on: depends_on.iter().map(ToString::to_string).collect(),
        depends_on_selected: Vec::new(),
        provides_selected: Some(provides_selected.to_owned()),
    })
}

fn operation_after_selected(
    id: &str,
    operation: OperationKind,
    depends_on_selected: &[&str],
) -> WorkflowNode {
    WorkflowNode::Operation(OperationNode {
        id: id.to_owned(),
        operation,
        depends_on: Vec::new(),
        depends_on_selected: depends_on_selected
            .iter()
            .map(ToString::to_string)
            .collect(),
        provides_selected: None,
    })
}

fn reject_cycles(nodes: &HashMap<&str, &WorkflowNode>) -> Result<(), WorkflowPlanError> {
    let selected_providers = selected_providers(nodes.values().copied());
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for id in nodes.keys().copied() {
        visit(id, nodes, &selected_providers, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn selected_providers<'a>(
    nodes: impl Iterator<Item = &'a WorkflowNode>,
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
    nodes: &HashMap<&'a str, &'a WorkflowNode>,
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
