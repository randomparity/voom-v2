//! `AssertionKind` — vocabulary for `identity_evidence.assertion_type`.
//! Spec §8.5.
//!
//! Its serde wire format and explicit parser both use the `snake_case`
//! string returned by `as_str()`. Tests cover every variant so the durable
//! event representation cannot drift from the identity-evidence column.

use serde::{Deserialize, Serialize};
use voom_core::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionKind {
    BelongsToWork,
    BelongsToVariant,
    SameAsAsset,
    DuplicateOfAsset,
    PreferredVariant,
    UserLabel,
    ExternalIdMatch,
    PathRuleMatch,
    HashMatch,
    RuntimeSimilarityMatch,
    FrameFingerprintMatch,
    AudioFingerprintMatch,
}

impl AssertionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BelongsToWork => "belongs_to_work",
            Self::BelongsToVariant => "belongs_to_variant",
            Self::SameAsAsset => "same_as_asset",
            Self::DuplicateOfAsset => "duplicate_of_asset",
            Self::PreferredVariant => "preferred_variant",
            Self::UserLabel => "user_label",
            Self::ExternalIdMatch => "external_id_match",
            Self::PathRuleMatch => "path_rule_match",
            Self::HashMatch => "hash_match",
            Self::RuntimeSimilarityMatch => "runtime_similarity_match",
            Self::FrameFingerprintMatch => "frame_fingerprint_match",
            Self::AudioFingerprintMatch => "audio_fingerprint_match",
        }
    }

    /// Parse the on-disk wire-format string into an `AssertionKind`.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the string is not one of the
    /// known `snake_case` wire-format values.
    #[expect(
        clippy::should_implement_trait,
        reason = "the dedicated database parser returns contextual VoomError values; \
                  std::str::FromStr would hide that boundary-specific error contract"
    )]
    pub fn from_str(s: &str) -> Result<Self, VoomError> {
        Ok(match s {
            "belongs_to_work" => Self::BelongsToWork,
            "belongs_to_variant" => Self::BelongsToVariant,
            "same_as_asset" => Self::SameAsAsset,
            "duplicate_of_asset" => Self::DuplicateOfAsset,
            "preferred_variant" => Self::PreferredVariant,
            "user_label" => Self::UserLabel,
            "external_id_match" => Self::ExternalIdMatch,
            "path_rule_match" => Self::PathRuleMatch,
            "hash_match" => Self::HashMatch,
            "runtime_similarity_match" => Self::RuntimeSimilarityMatch,
            "frame_fingerprint_match" => Self::FrameFingerprintMatch,
            "audio_fingerprint_match" => Self::AudioFingerprintMatch,
            other => {
                return Err(VoomError::database(format!(
                    "identity_evidence.assertion_type {other:?} not in AssertionKind vocab"
                )));
            }
        })
    }
}

impl TryFrom<&str> for AssertionKind {
    type Error = VoomError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::from_str(s)
    }
}

#[cfg(test)]
#[path = "assertion_test.rs"]
mod tests;
