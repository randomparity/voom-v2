use std::collections::BTreeMap;

use crate::PolicyDiagnostic;

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledConfig {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_languages"
    )]
    pub languages: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_on_error"
    )]
    pub on_error: Option<ErrorStrategy>,
}

fn deserialize_languages<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    decode_languages(&value).map_err(serde::de::Error::custom)
}

fn deserialize_on_error<'de, D>(deserializer: D) -> Result<Option<ErrorStrategy>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    decode_on_error(&value).map_err(serde::de::Error::custom)
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
    if !is_known_legacy_language_prefix(prefix) {
        return Err(format!(
            "legacy config.languages has unknown statement prefix `{}`",
            prefix.trim()
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

fn is_known_legacy_language_prefix(prefix: &str) -> bool {
    let prefix = prefix.trim();
    let prefix = prefix.strip_suffix(':').map_or(prefix, str::trim_end);
    let mut words = prefix.split_ascii_whitespace();
    if words.next() != Some("languages") {
        return false;
    }
    let target = words.next();
    words.next().is_none() && matches!(target, None | Some("audio" | "subtitle"))
}

fn decode_on_error(value: &serde_json::Value) -> Result<Option<ErrorStrategy>, String> {
    let Some(value) = value.as_str() else {
        return Err("config.on_error must be a string".to_owned());
    };
    let value = legacy_on_error_value(value)?;
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

fn legacy_on_error_value(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if matches!(value, "abort" | "continue" | "skip") {
        return Ok(value);
    }
    let Some(rest) = value.strip_prefix("on_error") else {
        return Ok(value);
    };
    let Some(first) = rest.chars().next() else {
        return Err("config.on_error is missing a strategy".to_owned());
    };
    let rest = if first == ':' {
        &rest[first.len_utf8()..]
    } else if first.is_ascii_whitespace() {
        let rest = rest.trim_start();
        rest.strip_prefix(':').map_or(rest, str::trim_start)
    } else {
        return Ok(value);
    };
    Ok(rest.trim())
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct CompiledPhase {
    pub name: String,
    pub depends_on: Vec<String>,
    pub run_if: Option<CompiledRunIf>,
    pub skip_if: Option<CompiledCondition>,
    pub on_error: Option<ErrorStrategy>,
    pub operations: Vec<CompiledOperation>,
}

// payload-contract: exempt — Deserialize delegates to strict CompiledRunIfWire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "CompiledRunIfWire", into = "CompiledRunIfWire")]
pub struct CompiledRunIf {
    pub trigger: RunIfTrigger,
    pub phase: String,
}

impl CompiledRunIf {
    pub(crate) fn parse(name: &str) -> Result<Self, String> {
        let Some((trigger, phase)) = name.split_once(' ') else {
            return Err("compiled run_if requires a trigger and phase".to_owned());
        };
        let trigger = match trigger {
            "completed" => RunIfTrigger::Completed,
            "modified" => RunIfTrigger::Modified,
            _ => {
                return Err(format!(
                    "compiled run_if trigger `{trigger}` must be completed or modified"
                ));
            }
        };
        if phase.is_empty() || phase.chars().any(char::is_whitespace) {
            return Err("compiled run_if requires exactly one phase name".to_owned());
        }
        Ok(Self {
            trigger,
            phase: phase.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunIfTrigger {
    Completed,
    Modified,
}

impl RunIfTrigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Modified => "modified",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledRunIfWire {
    #[serde(rename = "type")]
    kind: CompiledRunIfWireKind,
    name: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompiledRunIfWireKind {
    Predicate,
}

impl TryFrom<CompiledRunIfWire> for CompiledRunIf {
    type Error = String;

    fn try_from(value: CompiledRunIfWire) -> Result<Self, Self::Error> {
        let CompiledRunIfWire {
            kind: CompiledRunIfWireKind::Predicate,
            name,
        } = value;
        Self::parse(&name)
    }
}

impl From<CompiledRunIf> for CompiledRunIfWire {
    fn from(value: CompiledRunIf) -> Self {
        Self {
            kind: CompiledRunIfWireKind::Predicate,
            name: format!("{} {}", value.trigger.as_str(), value.phase),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "TranscodeVideo.resolved_profile is a pinned cross-phase contract field (Phase 6 fills it in-memory); boxing would diverge from the Sprint 15 plan's typed signature"
)]
pub enum CompiledOperation {
    SetContainer(CompiledSetContainerOperation),
    KeepTracks(CompiledKeepTracksOperation),
    RemoveTracks(CompiledRemoveTracksOperation),
    ReorderTracks(CompiledReorderTracksOperation),
    SetDefaults(CompiledSetDefaultsOperation),
    ClearTrackActions(CompiledClearTrackActionsOperation),
    ClearTags(CompiledClearTagsOperation),
    SetTag(CompiledSetTagOperation),
    DeleteTag(CompiledDeleteTagOperation),
    TranscodeVideo(CompiledTranscodeVideoOperation),
    TranscodeAudio(CompiledTranscodeAudioOperation),
    ExtractAudio(CompiledExtractAudioOperation),
    SynthesizeAudio(CompiledSynthesizeAudioOperation),
    VerifyArtifact(CompiledVerifyArtifactOperation),
    Conditional(CompiledConditionalOperation),
    Rules(CompiledRulesOperation),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledSetContainerOperation {
    pub container: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledKeepTracksOperation {
    pub target: TrackTarget,
    pub filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledRemoveTracksOperation {
    pub target: TrackTarget,
    pub filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledReorderTracksOperation {
    pub targets: Vec<TrackTarget>,
    /// `order tracks … where <filter>` pins the single filter-selected track
    /// to the head of the track order, ahead of the group order. Additive since
    /// ADR 0023 (#277); absent means group-only ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledSetDefaultsOperation {
    pub target: TrackTarget,
    pub strategy: DefaultStrategy,
    /// `defaults … where <filter>` makes the single filter-selected track the
    /// default for its group. Additive since ADR 0023 (#277); when present,
    /// `strategy` is not consulted. Absent means strategy-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledClearTrackActionsOperation {
    pub target: TrackTarget,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledClearTagsOperation {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledSetTagOperation {
    pub key: String,
    pub value: CompiledValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledDeleteTagOperation {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledTranscodeVideoOperation {
    pub target_codec: String,
    pub container: String,
    pub profile: crate::VideoProfileRef,
    /// Populated in-memory by the control plane's resolution step before
    /// planning. It is never written to `compiled_json`, so stored rows and
    /// `source_hash` are unaffected and legacy bare-string profiles remain
    /// readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_profile: Option<voom_core::TranscodeVideoProfile>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledTranscodeAudioOperation {
    pub target_codec: String,
    pub container: String,
    pub filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledExtractAudioOperation {
    pub target_codec: String,
    pub container: String,
    pub filter: Option<TrackFilter>,
}

/// `synthesize audio from <filter> { codec … channels … }` adds a downmixed
/// companion track derived from the filter-selected source streams.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledSynthesizeAudioOperation {
    pub target_codec: String,
    pub container: String,
    pub target_channels: u64,
    pub filter: Option<TrackFilter>,
}

/// `verify artifact` takes no arguments; the plan node identifies the target.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledVerifyArtifactOperation {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledConditionalOperation {
    pub condition: CompiledCondition,
    pub operations: Vec<CompiledOperation>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledRulesOperation {
    pub mode: RuleMatchMode,
    pub rules: Vec<CompiledRule>,
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
    LanguageIn(LanguageInTrackFilter),
    CodecIn(CodecInTrackFilter),
    Channels(ChannelsTrackFilter),
    Commentary(CommentaryTrackFilter),
    Forced(ForcedTrackFilter),
    Default(DefaultTrackFilter),
    Font(FontTrackFilter),
    TitleContains(TitleContainsTrackFilter),
    TitleMatches(TitleMatchesTrackFilter),
    Not(NotTrackFilter),
    And(AndTrackFilter),
    Or(OrTrackFilter),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageInTrackFilter {
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodecInTrackFilter {
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelsTrackFilter {
    pub op: ComparisonOp,
    pub value: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentaryTrackFilter {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForcedTrackFilter {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultTrackFilter {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontTrackFilter {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleContainsTrackFilter {
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleMatchesTrackFilter {
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotTrackFilter {
    pub inner: Box<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndTrackFilter {
    pub filters: Vec<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrTrackFilter {
    pub filters: Vec<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledCondition {
    Exists(CompiledExistsCondition),
    Count(CompiledCountCondition),
    FieldComparison(CompiledFieldComparisonCondition),
    FieldExists(CompiledFieldExistsCondition),
    Predicate(CompiledPredicateCondition),
    Not(CompiledNotCondition),
    And(CompiledAndCondition),
    Or(CompiledOrCondition),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledExistsCondition {
    pub target: TrackTarget,
    pub filter: Option<TrackFilter>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledCountCondition {
    pub target: TrackTarget,
    pub op: ComparisonOp,
    pub value: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledFieldComparisonCondition {
    pub path: Vec<String>,
    pub op: ComparisonOp,
    pub value: CompiledValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledFieldExistsCondition {
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledPredicateCondition {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledNotCondition {
    pub inner: Box<CompiledCondition>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledAndCondition {
    pub conditions: Vec<CompiledCondition>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledOrCondition {
    pub conditions: Vec<CompiledCondition>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledRule {
    pub name: String,
    pub condition: Option<CompiledCondition>,
    pub operations: Vec<CompiledOperation>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompiledValue {
    String(CompiledStringValue),
    Number(CompiledNumberValue),
    Boolean(CompiledBooleanValue),
    FieldPath(CompiledFieldPathValue),
    List(CompiledListValue),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledStringValue {
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledNumberValue {
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledBooleanValue {
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledFieldPathValue {
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledListValue {
    pub values: Vec<CompiledValue>,
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
