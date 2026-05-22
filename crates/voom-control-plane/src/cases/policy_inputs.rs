use voom_core::{PolicyInputSetId, VoomError};
use voom_store::repo::policy_inputs::{PolicyInputRepo, PolicyInputSet, PolicyInputSetSummary};

use crate::ControlPlane;

use super::{begin_tx, commit_tx};

impl ControlPlane {
    /// Create a durable policy input set without emitting events in Sprint 3.
    ///
    /// # Errors
    /// Propagates policy validation and repository errors.
    pub async fn create_policy_input_set(
        &self,
        input: voom_policy::PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self
            .policy_inputs
            .create_input_set_in_tx(&mut tx, input)
            .await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    /// Get a policy input set by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_policy_input_set(
        &self,
        id: PolicyInputSetId,
    ) -> Result<Option<PolicyInputSet>, VoomError> {
        self.policy_inputs.get_input_set(id).await
    }

    /// List policy input set summaries in repository order.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_policy_input_sets(&self) -> Result<Vec<PolicyInputSetSummary>, VoomError> {
        self.policy_inputs.list_input_sets().await
    }
}

#[cfg(test)]
#[path = "policy_inputs_test.rs"]
mod tests;
