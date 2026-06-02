use std::collections::BTreeMap;

use crate::PolicyDiagnostic;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledPolicy {
    pub policy_name: String,
    pub slug: String,
    pub source_hash: String,
    pub schema_version: u32,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub config: BTreeMap<String, serde_json::Value>,
    pub phases: Vec<CompiledPhase>,
    pub phase_order: Vec<String>,
    pub warnings: Vec<PolicyDiagnostic>,
    pub provenance: PolicyProvenance,
}

impl CompiledPolicy {
    #[cfg(test)]
    #[must_use]
    pub fn minimal_for_test(policy_name: &str, source_hash: &str) -> Self {
        Self {
            policy_name: policy_name.to_owned(),
            slug: slug(policy_name),
            source_hash: source_hash.to_owned(),
            schema_version: 2,
            metadata: BTreeMap::new(),
            config: BTreeMap::new(),
            phases: Vec::new(),
            phase_order: Vec::new(),
            warnings: Vec::new(),
            provenance: PolicyProvenance::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyProvenance {
    pub compiler: String,
    pub format: String,
    pub flags: BTreeMap<String, serde_json::Value>,
}

impl Default for PolicyProvenance {
    fn default() -> Self {
        Self {
            compiler: "voom-policy".to_owned(),
            format: "sprint4-v2".to_owned(),
            flags: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledPhase {
    pub name: String,
    pub depends_on: Vec<String>,
    pub run_if: Option<CompiledCondition>,
    pub skip_if: Option<CompiledCondition>,
    pub on_error: Option<ErrorStrategy>,
    pub operations: Vec<CompiledOperation>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "TranscodeVideo.resolved_profile is a pinned cross-phase contract field (Phase 6 fills it in-memory); boxing would diverge from the Sprint 15 plan's typed signature"
)]
pub enum CompiledOperation {
    SetContainer {
        container: String,
    },
    KeepTracks {
        target: TrackTarget,
        filter: Option<TrackFilter>,
    },
    RemoveTracks {
        target: TrackTarget,
        filter: Option<TrackFilter>,
    },
    ReorderTracks {
        targets: Vec<TrackTarget>,
    },
    SetDefaults {
        target: TrackTarget,
        strategy: DefaultStrategy,
    },
    ClearTrackActions {
        target: TrackTarget,
    },
    ClearTags,
    SetTag {
        key: String,
        value: CompiledValue,
    },
    DeleteTag {
        key: String,
    },
    TranscodeVideo {
        target_codec: String,
        container: String,
        profile: crate::VideoProfileRef,
        /// Populated in-memory by the control plane's resolution step
        /// (Phase 6) before planning; never written to `compiled_json`
        /// (skipped when `None`, defaults to `None` on read) so stored
        /// rows and `source_hash` are unaffected and legacy bare-string
        /// policies still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_profile: Option<voom_core::TranscodeVideoProfile>,
    },
    TranscodeAudio {
        target_codec: String,
        container: String,
        filter: Option<TrackFilter>,
    },
    ExtractAudio {
        target_codec: String,
        container: String,
        filter: Option<TrackFilter>,
    },
    Conditional {
        condition: CompiledCondition,
        operations: Vec<CompiledOperation>,
    },
    Rules {
        mode: RuleMatchMode,
        rules: Vec<CompiledRule>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackTarget {
    Video,
    Audio,
    Subtitle,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrackFilter {
    LanguageIn { values: Vec<String> },
    CodecIn { values: Vec<String> },
    Channels { op: ComparisonOp, value: u64 },
    Commentary,
    Forced,
    Default,
    Font,
    TitleContains { value: String },
    TitleMatches { value: String },
    Not { inner: Box<TrackFilter> },
    And { filters: Vec<TrackFilter> },
    Or { filters: Vec<TrackFilter> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledCondition {
    Exists {
        target: TrackTarget,
        filter: Option<TrackFilter>,
    },
    Count {
        target: TrackTarget,
        op: ComparisonOp,
        value: u64,
    },
    FieldComparison {
        path: Vec<String>,
        op: ComparisonOp,
        value: CompiledValue,
    },
    FieldExists {
        path: Vec<String>,
    },
    Predicate {
        name: String,
    },
    Not {
        inner: Box<CompiledCondition>,
    },
    And {
        conditions: Vec<CompiledCondition>,
    },
    Or {
        conditions: Vec<CompiledCondition>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledRule {
    pub name: String,
    pub condition: Option<CompiledCondition>,
    pub operations: Vec<CompiledOperation>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledValue {
    String { value: String },
    Number { value: String },
    Boolean { value: bool },
    FieldPath { path: Vec<String> },
    List { values: Vec<CompiledValue> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    Matches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultStrategy {
    First,
    Best,
    None,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatchMode {
    First,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorStrategy {
    Abort,
    Continue,
    Skip,
}

#[must_use]
pub fn source_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

pub fn deterministic_json(
    policy: &CompiledPolicy,
) -> Result<serde_json::Value, voom_core::VoomError> {
    serde_json::to_value(policy)
        .map_err(|e| voom_core::VoomError::Internal(format!("compiled policy serialize: {e}")))
}

pub(super) fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
#[path = "compiled_test.rs"]
mod tests;
