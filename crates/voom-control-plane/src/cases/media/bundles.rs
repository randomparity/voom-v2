//! Bundle-layer use cases. `create_bundle`, `add_bundle_member`,
//! `remove_bundle_member` each compose a `SqliteBundleRepo` `_in_tx` write
//! with the matching `asset_bundle.*` event.

use time::OffsetDateTime;
use voom_core::{BundleId, FileAssetId, MediaVariantId, VoomError};
use voom_events::payload::{
    AssetBundleCreatedPayload, AssetBundleMemberAddedPayload, AssetBundleMemberRemovedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::bundles::{
    AssetBundle, BundleMember, BundleMemberRole, NewAssetBundle, NewBundleMember,
};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// Create an `AssetBundle`. Emits `asset_bundle.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_bundle(&self, input: NewAssetBundle) -> Result<AssetBundle, VoomError> {
        let created_at = input.created_at;
        let mut tx = begin_tx(&self.pool).await?;
        let bundle = self.bundles.create_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetBundle,
            Some(bundle.id.0),
            created_at,
            Event::AssetBundleCreated(AssetBundleCreatedPayload {
                bundle_id: bundle.id.0,
                media_variant_id: bundle.media_variant_id.0,
                display_name: bundle.display_name.clone(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(bundle)
    }

    /// Add a member to an `AssetBundle`. Repo enforces the
    /// `(file_asset_id) UNIQUE` invariant: an asset may belong to at
    /// most one bundle. Emits `asset_bundle.member_added`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors; UNIQUE violation maps
    /// to `VoomError::Conflict`.
    pub async fn add_bundle_member(
        &self,
        bundle_id: BundleId,
        file_asset_id: FileAssetId,
        role: BundleMemberRole,
        observed_at: OffsetDateTime,
    ) -> Result<BundleMember, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let member = self
            .bundles
            .add_member_in_tx(
                &mut tx,
                NewBundleMember {
                    bundle_id,
                    file_asset_id,
                    role,
                },
            )
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetBundle,
            Some(bundle_id.0),
            observed_at,
            Event::AssetBundleMemberAdded(AssetBundleMemberAddedPayload {
                bundle_id: bundle_id.0,
                file_asset_id: file_asset_id.0,
                role: role.as_str().to_owned(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(member)
    }

    /// Remove a `(bundle, asset)` membership row. Emits
    /// `asset_bundle.member_removed`. Returns `NotFound` if the pair
    /// wasn't a member.
    ///
    /// The event's `role` is derived from the persisted row so the audit
    /// log cannot disagree with the committed state.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn remove_bundle_member(
        &self,
        bundle_id: BundleId,
        file_asset_id: FileAssetId,
        observed_at: OffsetDateTime,
    ) -> Result<BundleMember, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let removed = self
            .bundles
            .remove_member_in_tx(&mut tx, bundle_id, file_asset_id)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::AssetBundle,
            Some(bundle_id.0),
            observed_at,
            Event::AssetBundleMemberRemoved(AssetBundleMemberRemovedPayload {
                bundle_id: bundle_id.0,
                file_asset_id: file_asset_id.0,
                role: removed.role.as_str().to_owned(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(removed)
    }

    // Thin read-only accessor wrappers for the case-handler surface
    // mirror the repo's read methods one-to-one; they exist so callers
    // can be on a single import path. No event emission.

    /// Get a bundle by id.
    ///
    /// # Errors
    /// Propagates `SqliteBundleRepo::get` errors.
    pub async fn get_bundle(&self, id: BundleId) -> Result<Option<AssetBundle>, VoomError> {
        self.bundles.get(id).await
    }

    /// List all bundles for a media variant.
    ///
    /// # Errors
    /// Propagates `SqliteBundleRepo::list_by_variant` errors.
    pub async fn list_bundles_by_variant(
        &self,
        media_variant_id: MediaVariantId,
    ) -> Result<Vec<AssetBundle>, VoomError> {
        self.bundles.list_by_variant(media_variant_id).await
    }

    /// List members of a bundle.
    ///
    /// # Errors
    /// Propagates `SqliteBundleRepo::list_members` errors.
    pub async fn list_bundle_members(
        &self,
        bundle_id: BundleId,
    ) -> Result<Vec<BundleMember>, VoomError> {
        self.bundles.list_members(bundle_id).await
    }
}

#[cfg(test)]
#[path = "bundles_test.rs"]
mod tests;
