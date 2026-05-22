use voom_core::{PolicyDocumentId, PolicyVersionId, VoomError};
use voom_store::repo::policies::{
    CreatedPolicyVersion, NewPolicyDocumentVersion, PolicyDocument, PolicyDocumentSummary,
    PolicyRepo, PolicyVersion,
};

use crate::ControlPlane;

impl ControlPlane {
    /// Compile a policy source without persisting it.
    ///
    /// # Errors
    /// Propagates parser, validator, and compiler diagnostics.
    #[expect(
        clippy::unused_async,
        reason = "control-plane use-case methods expose an async API even for compile-only work"
    )]
    pub async fn compile_policy_source(
        &self,
        source: &str,
    ) -> Result<voom_policy::CompileOutput, voom_policy::PolicyCompileError> {
        voom_policy::compile_policy(source)
    }

    /// Create a policy document with its initial accepted version.
    ///
    /// # Errors
    /// Propagates policy compilation and repository errors.
    pub async fn create_policy_document(
        &self,
        slug: &str,
        source: &str,
    ) -> Result<CreatedPolicyVersion, VoomError> {
        self.policies
            .create_document_with_version(NewPolicyDocumentVersion {
                slug: slug.to_owned(),
                display_name: None,
                source_text: source.to_owned(),
                created_at: self.clock().now(),
            })
            .await
    }

    /// Add a new accepted version to an existing policy document.
    ///
    /// # Errors
    /// Propagates policy compilation and repository errors.
    pub async fn add_policy_version(
        &self,
        document_id: PolicyDocumentId,
        source: &str,
    ) -> Result<PolicyVersion, VoomError> {
        self.policies
            .add_version(document_id, source.to_owned())
            .await
    }

    /// Get a policy document by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_policy_document(
        &self,
        id: PolicyDocumentId,
    ) -> Result<Option<PolicyDocument>, VoomError> {
        self.policies.get_document(id).await
    }

    /// List policy document summaries in repository order.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_policy_documents(&self) -> Result<Vec<PolicyDocumentSummary>, VoomError> {
        self.policies.list_documents().await
    }

    /// Get a policy version by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_policy_version(
        &self,
        id: PolicyVersionId,
    ) -> Result<Option<PolicyVersion>, VoomError> {
        self.policies.get_version(id).await
    }

    /// List policy versions for a document.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_policy_versions(
        &self,
        document_id: PolicyDocumentId,
    ) -> Result<Vec<PolicyVersion>, VoomError> {
        self.policies.list_versions(document_id).await
    }
}

#[cfg(test)]
#[path = "policies_test.rs"]
mod tests;
