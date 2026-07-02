//! Control-plane CRUD use cases for the durable quality scoring registry
//! (#285/T16). Thin delegations to the store repo, stamping the injected clock
//! on create. No scorer consumes these records yet (design doc -> Quality
//! Scoring Registry); see `docs/adr/0032`.

use voom_core::VoomError;
use voom_store::repo::quality_scoring_profiles::{NewQualityScoringProfile, QualityScoringProfile};

use crate::ControlPlane;

impl ControlPlane {
    /// Create a quality scoring profile.
    ///
    /// # Errors
    /// [`VoomError::Config`] for a non-object `definition`, [`VoomError::Conflict`]
    /// on a duplicate name, or a database error.
    pub async fn create_scoring_profile(
        &self,
        input: NewQualityScoringProfile,
    ) -> Result<QualityScoringProfile, VoomError> {
        self.quality_scoring_profiles
            .create(input, self.clock().now())
            .await
    }

    /// Read a scoring profile by name (retired or not).
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_scoring_profile(
        &self,
        name: &str,
    ) -> Result<Option<QualityScoringProfile>, VoomError> {
        self.quality_scoring_profiles.get_by_name(name).await
    }

    /// List active (non-retired) scoring profiles ordered by name.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn list_scoring_profiles(&self) -> Result<Vec<QualityScoringProfile>, VoomError> {
        self.quality_scoring_profiles.list().await
    }

    /// Full-replace update of the scoring profile keyed by `input.name`.
    ///
    /// # Errors
    /// [`VoomError::Config`] for a non-object `definition`, or a database error.
    /// Returns `Ok(None)` when no profile has that name.
    pub async fn update_scoring_profile(
        &self,
        input: NewQualityScoringProfile,
    ) -> Result<Option<QualityScoringProfile>, VoomError> {
        self.quality_scoring_profiles.update(input).await
    }

    /// Soft-retire a scoring profile by name (idempotent). Returns `Ok(None)`
    /// when no profile has that name.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn retire_scoring_profile(
        &self,
        name: &str,
    ) -> Result<Option<QualityScoringProfile>, VoomError> {
        self.quality_scoring_profiles
            .retire(name, self.clock().now())
            .await
    }
}
