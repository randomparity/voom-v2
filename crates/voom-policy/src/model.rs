use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyInputSourceKind {
    Fixture,
    Test,
    Imported,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    MediaWork,
    MediaVariant,
    AssetBundle,
    FileAsset,
    FileVersion,
    FileLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TargetRef {
    MediaWork { id: voom_core::MediaWorkId },
    MediaVariant { id: voom_core::MediaVariantId },
    AssetBundle { id: voom_core::BundleId },
    FileAsset { id: voom_core::FileAssetId },
    FileVersion { id: voom_core::FileVersionId },
    FileLocation { id: voom_core::FileLocationId },
    Synthetic { key: String, kind: TargetKind },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyInputSetDraft {
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub source_kind: PolicyInputSourceKind,
    pub created_at: time::OffsetDateTime,
    pub description: Option<String>,
    pub fixture_labels: Vec<String>,
    pub synthetic_targets: Vec<PolicySyntheticTarget>,
    pub media_snapshots: Vec<MediaSnapshotInput>,
    pub identity_evidence: Vec<IdentityEvidenceInput>,
    pub bundle_targets: Vec<BundleTargetInput>,
    pub quality_profiles: Vec<QualityProfileSelection>,
    pub issues: Vec<IssueInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PolicySyntheticTarget {
    pub synthetic_key: String,
    pub target_kind: TargetKind,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MediaSnapshotInput {
    pub ordinal: u32,
    pub target: TargetRef,
    pub container: Option<String>,
    pub stream_summary: serde_json::Value,
    pub video_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hdr: Option<String>,
    pub bitrate: Option<u64>,
    pub duration_millis: Option<u64>,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub health_flags: Vec<String>,
    pub existing_media_snapshot_id: Option<voom_core::MediaSnapshotId>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IdentityEvidenceInput {
    pub ordinal: u32,
    pub target: TargetRef,
    pub assertion_type: String,
    pub provider: String,
    pub provider_version: String,
    pub confidence: f64,
    pub provenance: serde_json::Value,
    pub observed_at: time::OffsetDateTime,
    pub existing_evidence_id: Option<voom_core::EvidenceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleTargetState {
    Required,
    Allowed,
    Forbidden,
    Preferred,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BundleTargetInput {
    pub ordinal: u32,
    pub target: TargetRef,
    pub role: String,
    pub desired_state: BundleTargetState,
    pub language: Option<String>,
    pub label: Option<String>,
    pub disposition: Option<String>,
    pub artifact_expectation: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QualityProfileSelection {
    pub ordinal: u32,
    pub target: TargetRef,
    pub profile_name: String,
    pub profile_version: String,
    pub dimension_weights: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueInputState {
    Open,
    Accepted,
    Suppressed,
    Planned,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IssueInput {
    pub ordinal: u32,
    pub target: TargetRef,
    pub kind: String,
    pub severity: voom_core::IssueSeverity,
    pub priority: voom_core::IssuePriority,
    pub state: IssueInputState,
    pub reason: String,
    pub provenance: serde_json::Value,
    pub existing_issue_id: Option<voom_core::IssueId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyInputSetValidationError {
    EmptySlug,
    EmptyFixtureLabels,
    DuplicateFixtureLabel(String),
    MissingSnapshotOrBundleTarget,
    UndeclaredSyntheticTarget {
        key: String,
        kind: TargetKind,
    },
    SyntheticKeyKindMismatch {
        key: String,
        expected: TargetKind,
        actual: TargetKind,
    },
    InvalidEvidenceConfidence {
        ordinal: u32,
    },
    EmptyProviderName {
        ordinal: u32,
    },
    EmptyQualityProfileName {
        ordinal: u32,
    },
}

pub fn validate_input_set(
    input: &PolicyInputSetDraft,
) -> Result<(), PolicyInputSetValidationError> {
    if input.slug.trim().is_empty() {
        return Err(PolicyInputSetValidationError::EmptySlug);
    }

    validate_fixture_labels(&input.fixture_labels)?;

    if input.media_snapshots.is_empty() && input.bundle_targets.is_empty() {
        return Err(PolicyInputSetValidationError::MissingSnapshotOrBundleTarget);
    }

    let synthetic_targets = validate_synthetic_declarations(&input.synthetic_targets)?;
    validate_child_targets(input, &synthetic_targets)?;
    validate_evidence(&input.identity_evidence)?;
    validate_quality_profiles(&input.quality_profiles)?;

    Ok(())
}

fn validate_fixture_labels(labels: &[String]) -> Result<(), PolicyInputSetValidationError> {
    if labels.is_empty() || labels.iter().any(|label| label.trim().is_empty()) {
        return Err(PolicyInputSetValidationError::EmptyFixtureLabels);
    }

    let mut seen = HashSet::new();
    for label in labels {
        if !seen.insert(label) {
            return Err(PolicyInputSetValidationError::DuplicateFixtureLabel(
                label.clone(),
            ));
        }
    }

    Ok(())
}

fn validate_synthetic_declarations(
    declarations: &[PolicySyntheticTarget],
) -> Result<HashMap<&str, TargetKind>, PolicyInputSetValidationError> {
    let mut targets = HashMap::new();

    for declaration in declarations {
        if let Some(existing) =
            targets.insert(declaration.synthetic_key.as_str(), declaration.target_kind)
            && existing != declaration.target_kind
        {
            return Err(PolicyInputSetValidationError::SyntheticKeyKindMismatch {
                key: declaration.synthetic_key.clone(),
                expected: existing,
                actual: declaration.target_kind,
            });
        }
    }

    Ok(targets)
}

fn validate_child_targets(
    input: &PolicyInputSetDraft,
    synthetic_targets: &HashMap<&str, TargetKind>,
) -> Result<(), PolicyInputSetValidationError> {
    for target in input
        .media_snapshots
        .iter()
        .map(|snapshot| &snapshot.target)
        .chain(
            input
                .identity_evidence
                .iter()
                .map(|evidence| &evidence.target),
        )
        .chain(input.bundle_targets.iter().map(|bundle| &bundle.target))
        .chain(input.quality_profiles.iter().map(|profile| &profile.target))
        .chain(input.issues.iter().map(|issue| &issue.target))
    {
        validate_target_ref(target, synthetic_targets)?;
    }

    Ok(())
}

fn validate_target_ref(
    target: &TargetRef,
    synthetic_targets: &HashMap<&str, TargetKind>,
) -> Result<(), PolicyInputSetValidationError> {
    let TargetRef::Synthetic { key, kind } = target else {
        return Ok(());
    };

    match synthetic_targets.get(key.as_str()) {
        Some(declared_kind) if declared_kind == kind => Ok(()),
        Some(declared_kind) => Err(PolicyInputSetValidationError::SyntheticKeyKindMismatch {
            key: key.clone(),
            expected: *declared_kind,
            actual: *kind,
        }),
        None => Err(PolicyInputSetValidationError::UndeclaredSyntheticTarget {
            key: key.clone(),
            kind: *kind,
        }),
    }
}

fn validate_evidence(
    evidence_inputs: &[IdentityEvidenceInput],
) -> Result<(), PolicyInputSetValidationError> {
    for evidence in evidence_inputs {
        if evidence.provider.trim().is_empty() {
            return Err(PolicyInputSetValidationError::EmptyProviderName {
                ordinal: evidence.ordinal,
            });
        }
        if !(0.0..=1.0).contains(&evidence.confidence) {
            return Err(PolicyInputSetValidationError::InvalidEvidenceConfidence {
                ordinal: evidence.ordinal,
            });
        }
    }

    Ok(())
}

fn validate_quality_profiles(
    quality_profiles: &[QualityProfileSelection],
) -> Result<(), PolicyInputSetValidationError> {
    for profile in quality_profiles {
        if profile.profile_name.trim().is_empty() {
            return Err(PolicyInputSetValidationError::EmptyQualityProfileName {
                ordinal: profile.ordinal,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
