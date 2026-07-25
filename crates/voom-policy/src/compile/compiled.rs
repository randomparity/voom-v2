use std::collections::BTreeMap;

use crate::PolicyDiagnostic;

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct CompiledConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<ErrorStrategy>,
}

impl<'de> serde::Deserialize<'de> for CompiledConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let values =
            <BTreeMap<String, serde_json::Value> as serde::Deserialize>::deserialize(deserializer)?;
        let languages = values
            .get("languages")
            .map(decode_languages)
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();
        let on_error = values
            .get("on_error")
            .map(decode_on_error)
            .transpose()
            .map_err(serde::de::Error::custom)?
            .flatten();
        Ok(Self {
            languages,
            on_error,
        })
    }
}

fn decode_languages(value: &serde_json::Value) -> Result<Vec<String>, String> {
    let languages = match value {
        serde_json::Value::Array(values) => {
            let mut languages = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let Some(language) = value.as_str() else {
                    return Err(format!("config.languages[{index}] must be a string"));
                };
                languages.push(language.to_owned());
            }
            languages
        }
        serde_json::Value::String(statement) => decode_legacy_languages(statement)?,
        _ => return Err("config.languages must be an array".to_owned()),
    };
    for (index, language) in languages.iter().enumerate() {
        if !is_canonical_language_code(language) {
            return Err(format!(
                "config.languages[{index}] must be a three-letter lowercase ASCII code"
            ));
        }
    }
    Ok(languages)
}

fn decode_legacy_languages(statement: &str) -> Result<Vec<String>, String> {
    let (prefix, values) = statement
        .split_once('[')
        .ok_or_else(|| "legacy config.languages is missing `[`".to_owned())?;
    let prefix = prefix.trim();
    if !matches!(prefix, "languages:" | "languages audio:") {
        return Err(format!(
            "legacy config.languages has unknown statement prefix `{prefix}`"
        ));
    }
    let values = values
        .trim()
        .strip_suffix(']')
        .ok_or_else(|| "legacy config.languages is missing final `]`".to_owned())?;
    if values.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(values
        .split(',')
        .map(|language| {
            let language = language.trim();
            language
                .strip_prefix('"')
                .and_then(|language| language.strip_suffix('"'))
                .unwrap_or(language)
                .to_owned()
        })
        .collect())
}

fn decode_on_error(value: &serde_json::Value) -> Result<Option<ErrorStrategy>, String> {
    let Some(value) = value.as_str() else {
        return Err("config.on_error must be a string".to_owned());
    };
    let value = value
        .strip_prefix("on_error:")
        .or_else(|| value.strip_prefix("on_error "))
        .unwrap_or(value)
        .trim();
    let strategy = match value {
        "abort" => ErrorStrategy::Abort,
        "continue" => ErrorStrategy::Continue,
        "skip" => ErrorStrategy::Skip,
        _ => {
            return Err(format!(
                "config.on_error contains unknown strategy `{value}`"
            ));
        }
    };
    Ok(Some(strategy))
}

pub(crate) fn is_canonical_language_code(value: &str) -> bool {
    value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyTool {
    Ffmpeg,
    Ffprobe,
    Mkvtoolnix,
}

impl PolicyTool {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
            Self::Mkvtoolnix => "mkvtoolnix",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "ffmpeg" => Some(Self::Ffmpeg),
            "ffprobe" => Some(Self::Ffprobe),
            "mkvtoolnix" => Some(Self::Mkvtoolnix),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredToolsError {
    NotArray,
    NotString { index: usize },
    UnknownTool { index: usize, name: String },
}

impl std::fmt::Display for RequiredToolsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotArray => formatter.write_str("metadata.requires_tools must be an array"),
            Self::NotString { index } => {
                write!(
                    formatter,
                    "metadata.requires_tools[{index}] must be a string"
                )
            }
            Self::UnknownTool { index, name } => write!(
                formatter,
                "metadata.requires_tools[{index}] contains unknown tool `{name}`"
            ),
        }
    }
}

impl std::error::Error for RequiredToolsError {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompiledPolicy {
    pub policy_name: String,
    pub slug: String,
    pub source_hash: String,
    pub schema_version: u32,
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub config: CompiledConfig,
    pub phases: Vec<CompiledPhase>,
    pub phase_order: Vec<String>,
    pub warnings: Vec<PolicyDiagnostic>,
    pub provenance: PolicyProvenance,
}

impl CompiledPolicy {
    /// Return the published tools declared by `metadata.requires_tools`.
    ///
    /// The metadata map is the durable representation for every compiled
    /// policy version. This view accepts canonical strings from stored JSON and
    /// removes duplicate tools while preserving their first-seen order.
    ///
    /// # Errors
    /// Returns [`RequiredToolsError`] when stored metadata is malformed or
    /// names a tool outside the published vocabulary.
    pub fn required_tools(&self) -> Result<Vec<PolicyTool>, RequiredToolsError> {
        let Some(value) = self.metadata.get("requires_tools") else {
            return Ok(Vec::new());
        };
        let values = value.as_array().ok_or(RequiredToolsError::NotArray)?;
        let mut tools = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let name = value
                .as_str()
                .ok_or(RequiredToolsError::NotString { index })?;
            let tool =
                PolicyTool::from_name(name).ok_or_else(|| RequiredToolsError::UnknownTool {
                    index,
                    name: name.to_owned(),
                })?;
            if !tools.contains(&tool) {
                tools.push(tool);
            }
        }
        Ok(tools)
    }

    /// Materialize configured policy defaults into phases that omit overrides.
    ///
    /// Calling this method more than once is safe. Explicit phase strategies
    /// are never replaced.
    pub fn apply_execution_defaults(&mut self) {
        let Some(strategy) = self.config.on_error else {
            return;
        };
        for phase in &mut self.phases {
            if phase.on_error.is_none() {
                phase.on_error = Some(strategy);
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn minimal_for_test(policy_name: &str, source_hash: &str) -> Self {
        Self {
            policy_name: policy_name.to_owned(),
            slug: slug(policy_name),
            source_hash: source_hash.to_owned(),
            schema_version: 2,
            metadata: BTreeMap::new(),
            config: CompiledConfig::default(),
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
        /// `order tracks … where <filter>` pins the single filter-selected track
        /// to the head of the track order, ahead of the group order. Additive
        /// since ADR 0023 (#277); absent ⇒ group-only ordering.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_filter: Option<TrackFilter>,
    },
    SetDefaults {
        target: TrackTarget,
        strategy: DefaultStrategy,
        /// `defaults … where <filter>` makes the single filter-selected track
        /// the default for its group. Additive since ADR 0023 (#277); when
        /// present, `strategy` is not consulted. Absent ⇒ strategy-only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TrackFilter>,
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
    /// `synthesize audio from <filter> { codec … channels … }` — add a
    /// downmixed companion track derived from the filter-selected source
    /// stream(s) (ADR 0026, #276). Unlike `TranscodeAudio` this *adds* a stream
    /// rather than replacing it; `target_channels` is the companion's channel
    /// count (a downmix, so fewer than the source).
    SynthesizeAudio {
        target_codec: String,
        container: String,
        target_channels: u64,
        filter: Option<TrackFilter>,
    },
    /// `verify artifact` — verify the produced artifact against its expected
    /// facts. The spec production takes no arguments, so the variant is
    /// fieldless; the target artifact is identified by the plan node's target
    /// and snapshot, not by operation parameters.
    VerifyArtifact,
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
