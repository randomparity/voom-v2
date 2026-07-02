//! Control-plane CRUD use cases for the durable scheduling and safety policy
//! records (T12, #281). Thin delegations to the store repos, stamping the
//! injected clock on create/update. Read semantics and the fail-closed gate that
//! consumes safety policies live in `safety_gate.rs`.

use voom_core::VoomError;
use voom_store::repo::safety_policies::{NewSafetyPolicy, SafetyPolicy};
use voom_store::repo::scheduling_policies::{NewSchedulingPolicy, SchedulingPolicy};

use crate::ControlPlane;

impl ControlPlane {
    /// Create a scheduling policy.
    ///
    /// # Errors
    /// [`VoomError::Conflict`] on a duplicate slug, [`VoomError::Config`] on an
    /// invalid `copy_window`, or a database error.
    pub async fn create_scheduling_policy(
        &self,
        input: NewSchedulingPolicy,
    ) -> Result<SchedulingPolicy, VoomError> {
        self.scheduling_policies
            .create(input, self.clock().now())
            .await
    }

    /// Read a scheduling policy by slug.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_scheduling_policy(
        &self,
        slug: &str,
    ) -> Result<Option<SchedulingPolicy>, VoomError> {
        self.scheduling_policies.get_by_slug(slug).await
    }

    /// List scheduling policies ordered by slug.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn list_scheduling_policies(&self) -> Result<Vec<SchedulingPolicy>, VoomError> {
        self.scheduling_policies.list().await
    }

    /// Full-replace update of the scheduling policy keyed by `input.slug`.
    ///
    /// # Errors
    /// [`VoomError::Config`] on an invalid `copy_window`, or a database error.
    /// Returns `Ok(None)` when no policy has that slug.
    pub async fn update_scheduling_policy(
        &self,
        input: NewSchedulingPolicy,
    ) -> Result<Option<SchedulingPolicy>, VoomError> {
        self.scheduling_policies
            .update(input, self.clock().now())
            .await
    }

    /// Delete a scheduling policy by slug. `Ok(true)` when a row was removed.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn delete_scheduling_policy(&self, slug: &str) -> Result<bool, VoomError> {
        self.scheduling_policies.delete(slug).await
    }

    /// Create a safety policy.
    ///
    /// # Errors
    /// [`VoomError::Conflict`] on a duplicate slug, or a database error.
    pub async fn create_safety_policy(
        &self,
        input: NewSafetyPolicy,
    ) -> Result<SafetyPolicy, VoomError> {
        self.safety_policies.create(input, self.clock().now()).await
    }

    /// Read a safety policy by slug.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_safety_policy(&self, slug: &str) -> Result<Option<SafetyPolicy>, VoomError> {
        self.safety_policies.get_by_slug(slug).await
    }

    /// List safety policies ordered by slug.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn list_safety_policies(&self) -> Result<Vec<SafetyPolicy>, VoomError> {
        self.safety_policies.list().await
    }

    /// Full-replace update of the safety policy keyed by `input.slug`.
    ///
    /// # Errors
    /// Propagates database errors. Returns `Ok(None)` when no policy has that
    /// slug.
    pub async fn update_safety_policy(
        &self,
        input: NewSafetyPolicy,
    ) -> Result<Option<SafetyPolicy>, VoomError> {
        self.safety_policies.update(input, self.clock().now()).await
    }

    /// Delete a safety policy by slug. `Ok(true)` when a row was removed.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn delete_safety_policy(&self, slug: &str) -> Result<bool, VoomError> {
        self.safety_policies.delete(slug).await
    }
}
