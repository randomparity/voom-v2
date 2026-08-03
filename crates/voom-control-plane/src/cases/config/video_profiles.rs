use voom_core::VoomError;
use voom_store::repo::policy::video_profiles::{NewVideoProfile, VideoProfile};

use crate::ControlPlane;

impl ControlPlane {
    /// List the active (non-retired) video encode profiles, ordered by name.
    ///
    /// This surface powers `voom profile list`.
    ///
    /// # Errors
    /// Propagates video-profile repository read errors.
    pub async fn list_video_profiles(&self) -> Result<Vec<VideoProfile>, VoomError> {
        self.video_profiles.list().await
    }

    /// Look up one video encode profile by registry name (retired or not).
    ///
    /// Returns `None` for an unknown name; callers map that to `NOT_FOUND`.
    ///
    /// # Errors
    /// Propagates video-profile repository read errors.
    pub async fn get_video_profile(&self, name: &str) -> Result<Option<VideoProfile>, VoomError> {
        self.video_profiles.get_by_name(name).await
    }

    /// Create a durable video encode profile, validated against its encoder's
    /// capability descriptor.
    ///
    /// # Errors
    /// [`VoomError::Config`] for an invalid field, [`VoomError::Conflict`] for a
    /// duplicate name, or a database error.
    pub async fn create_video_profile(
        &self,
        input: NewVideoProfile,
    ) -> Result<VideoProfile, VoomError> {
        self.video_profiles.create(input).await
    }

    /// Full-replace update of the video profile keyed by `input.name`.
    ///
    /// # Errors
    /// [`VoomError::Config`] for an invalid field, or a database error. Returns
    /// `Ok(None)` when no profile has that name.
    pub async fn update_video_profile(
        &self,
        input: NewVideoProfile,
    ) -> Result<Option<VideoProfile>, VoomError> {
        self.video_profiles.update(input).await
    }

    /// Soft-retire a video profile by name (idempotent). Returns `Ok(None)`
    /// when no profile has that name.
    ///
    /// # Errors
    /// Propagates video-profile repository errors.
    pub async fn retire_video_profile(
        &self,
        name: &str,
    ) -> Result<Option<VideoProfile>, VoomError> {
        self.video_profiles.retire(name, self.clock().now()).await
    }
}
