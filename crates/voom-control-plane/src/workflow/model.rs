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
                operation("remux", OperationKind::Remux, &["quality"]),
                operation("transcode", OperationKind::TranscodeVideo, &["quality"]),
                operation("backup", OperationKind::BackUpFile, &["remux", "transcode"]),
                operation("verify", OperationKind::VerifyArtifact, &["backup"]),
                operation("commit", OperationKind::CommitArtifact, &["verify"]),
                operation("sync", OperationKind::SyncExternalSystem, &["commit"]),
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
    })
}

fn reject_cycles(nodes: &HashMap<&str, &WorkflowNode>) -> Result<(), WorkflowPlanError> {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for id in nodes.keys().copied() {
        visit(id, nodes, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit<'a>(
    id: &'a str,
    nodes: &HashMap<&'a str, &'a WorkflowNode>,
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
        visit(dependency, nodes, visiting, visited)?;
    }
    visiting.remove(id);
    visited.insert(id);
    Ok(())
}
