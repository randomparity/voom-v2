use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanningRequest {
    pub policy: voom_policy::CompiledPolicy,
    pub input: voom_policy::PolicyInputSetDraft,
    pub context: PlanningContext,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanningContext {
    pub schema_version: u32,
    pub policy_document_id: Option<voom_core::PolicyDocumentId>,
    pub policy_version_id: Option<voom_core::PolicyVersionId>,
    pub policy_input_set_id: Option<voom_core::PolicyInputSetId>,
    pub input_source_label: Option<String>,
    pub generated_at: Option<OffsetDateTime>,
    pub feature_flags: BTreeMap<String, bool>,
}

impl Default for PlanningContext {
    fn default() -> Self {
        Self {
            schema_version: 1,
            policy_document_id: None,
            policy_version_id: None,
            policy_input_set_id: None,
            input_source_label: None,
            generated_at: None,
            feature_flags: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub plan_hash: String,
    pub policy: PolicyIdentity,
    pub input: InputIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<OffsetDateTime>,
    pub summary: PlanSummary,
    pub nodes: Vec<PlanNode>,
    pub edges: Vec<Edge>,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<crate::PlanningDiagnostic>,
    pub provenance: PlanProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PolicyIdentity {
    pub slug: String,
    pub source_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<voom_core::PolicyDocumentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<voom_core::PolicyVersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_set_id: Option<voom_core::PolicyInputSetId>,
    pub fixture_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PlanSummary {
    pub total_node_count: u32,
    pub executable_node_count: u32,
    pub no_op_node_count: u32,
    pub blocked_node_count: u32,
    pub target_count: u32,
    pub operation_counts_by_kind: BTreeMap<PlanOperationKind, u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanNode {
    pub node_id: String,
    pub phase_name: String,
    pub ordinal: u32,
    pub target: TargetRef,
    pub operation_kind: PlanOperationKind,
    pub operation_payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_state: Option<serde_json::Value>,
    pub status: NodeStatus,
    pub status_reason: String,
    pub capability_hints: CapabilityHints,
    pub scheduling_hints: SchedulingHints,
    pub resource_estimates: ResourceEstimates,
    pub artifact_expectations: ArtifactExpectations,
    pub safety_hints: SafetyHints,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PlanOperationKind {
    Remux,
    SetContainer,
    KeepTracks,
    RemoveTracks,
    ReorderTracks,
    SetDefaults,
    ClearTrackActions,
    ClearTags,
    SetTag,
    DeleteTag,
    TranscodeVideo,
    TranscodeAudio,
    ExtractAudio,
    Conditional,
    Rules,
}

impl PlanOperationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Remux => "remux",
            Self::SetContainer => "set_container",
            Self::KeepTracks => "keep_tracks",
            Self::RemoveTracks => "remove_tracks",
            Self::ReorderTracks => "reorder_tracks",
            Self::SetDefaults => "set_defaults",
            Self::ClearTrackActions => "clear_track_actions",
            Self::ClearTags => "clear_tags",
            Self::SetTag => "set_tag",
            Self::DeleteTag => "delete_tag",
            Self::TranscodeVideo => "transcode_video",
            Self::TranscodeAudio => "transcode_audio",
            Self::ExtractAudio => "extract_audio",
            Self::Conditional => "conditional",
            Self::Rules => "rules",
        }
    }
}

impl Display for PlanOperationKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Planned,
    NoOp,
    Blocked,
}

pub type TargetRef = voom_policy::TargetRef;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Edge {
    pub edge_id: String,
    pub from_node_id: String,
    pub to_node_id: String,
    pub dependency_kind: DependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    PhaseDependsOn,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct CapabilityHints {
    pub operation_capability: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchedulingHints {
    pub priority_class: String,
    pub estimated_cpu_class: String,
    pub estimated_gpu_class: String,
    pub estimated_disk_bytes: Estimate,
    pub estimated_network_bytes: Estimate,
    pub expected_duration: Estimate,
    pub concurrency_key: Option<String>,
}

impl Default for SchedulingHints {
    fn default() -> Self {
        Self {
            priority_class: "normal".to_owned(),
            estimated_cpu_class: "unknown".to_owned(),
            estimated_gpu_class: "none".to_owned(),
            estimated_disk_bytes: Estimate::Unknown,
            estimated_network_bytes: Estimate::Unknown,
            expected_duration: Estimate::Unknown,
            concurrency_key: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Estimate {
    Unknown,
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ResourceEstimates {
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ArtifactExpectations {
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SafetyHints {
    pub requires_approval: bool,
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanProvenance {
    pub planner: String,
    pub format: String,
}

impl Default for PlanProvenance {
    fn default() -> Self {
        Self {
            planner: "voom-plan".to_owned(),
            format: "sprint5-v1".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
