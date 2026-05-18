//! `SubjectType` — wire-format enum tagging which entity an event row
//! refers to. Sprint 1 M1 + M2 subset; M3 adds the remaining variants.
//!
//! Like `EventKind`, this enum does NOT derive `Serialize`/`Deserialize`:
//! the wire format is the string returned by `as_str()`, and we keep
//! parsing explicit so it can never silently diverge from the on-disk
//! `events.subject_type` column. The current variants happen to use
//! `snake_case` for both, but the explicit pair is the discipline.

use voom_core::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubjectType {
    System,
    Job,
    Ticket,
    Lease,
    Worker,
    ArtifactHandle,
    ArtifactLocation,
    // M2 — identity layer.
    MediaWork,
    MediaVariant,
    AssetBundle,
    FileAsset,
    FileVersion,
    FileLocation,
    IdentityEvidence,
    MediaSnapshot,
    // M3 — use leases.
    AssetUseLease,
}

impl SubjectType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Job => "job",
            Self::Ticket => "ticket",
            Self::Lease => "lease",
            Self::Worker => "worker",
            Self::ArtifactHandle => "artifact_handle",
            Self::ArtifactLocation => "artifact_location",
            Self::MediaWork => "media_work",
            Self::MediaVariant => "media_variant",
            Self::AssetBundle => "asset_bundle",
            Self::FileAsset => "file_asset",
            Self::FileVersion => "file_version",
            Self::FileLocation => "file_location",
            Self::IdentityEvidence => "identity_evidence",
            Self::MediaSnapshot => "media_snapshot",
            Self::AssetUseLease => "asset_use_lease",
        }
    }

    /// Parse the on-disk wire-format string into a `SubjectType`. Mirrors
    /// `as_str()` exactly.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the string is not one of the
    /// known values.
    #[expect(
        clippy::should_implement_trait,
        reason = "explicit inherent fn keeps the wire format the single source of truth; \
                  std::str::FromStr would mask the dedicated VoomError-bearing API"
    )]
    pub fn from_str(s: &str) -> Result<Self, VoomError> {
        Ok(match s {
            "system" => Self::System,
            "job" => Self::Job,
            "ticket" => Self::Ticket,
            "lease" => Self::Lease,
            "worker" => Self::Worker,
            "artifact_handle" => Self::ArtifactHandle,
            "artifact_location" => Self::ArtifactLocation,
            "media_work" => Self::MediaWork,
            "media_variant" => Self::MediaVariant,
            "asset_bundle" => Self::AssetBundle,
            "file_asset" => Self::FileAsset,
            "file_version" => Self::FileVersion,
            "file_location" => Self::FileLocation,
            "identity_evidence" => Self::IdentityEvidence,
            "media_snapshot" => Self::MediaSnapshot,
            "asset_use_lease" => Self::AssetUseLease,
            other => {
                return Err(VoomError::Database(format!(
                    "events.subject_type {other:?} not in SubjectType vocab"
                )));
            }
        })
    }
}

impl TryFrom<&str> for SubjectType {
    type Error = VoomError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::from_str(s)
    }
}

#[cfg(test)]
#[path = "subject_test.rs"]
mod tests;
