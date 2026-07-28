//! Bundle-layer use cases. Mutations compose `SqliteBundleRepo` `_in_tx`
//! writes with matching identity and `asset_bundle.*` events.

use std::path::Path;

use time::OffsetDateTime;
use voom_core::{BundleId, FileAssetId, FileVersionId, MediaVariantId, MediaWorkId, VoomError};
use voom_events::payload::{
    AssetBundleCreatedPayload, AssetBundleMemberAddedPayload, AssetBundleMemberRemovedPayload,
    MediaVariantCreatedPayload, MediaWorkCreatedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::bundles::{
    AssetBundle, BundleMember, BundleMemberRole, NewAssetBundle, NewBundleMember,
};
use voom_store::repo::identity::{IdentityRepo, MediaWorkKind, NewMediaVariant, NewMediaWork};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrimaryBundleResolution {
    pub(crate) bundle_id: BundleId,
    pub(crate) created: bool,
}

impl ControlPlane {
    pub(crate) async fn find_primary_bundle_for_file_version(
        &self,
        file_version_id: FileVersionId,
    ) -> Result<Option<BundleId>, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let (_, bundle_id) = self
            .primary_bundle_for_file_version_in_tx(&mut tx, file_version_id)
            .await?;
        commit_tx(tx).await?;
        Ok(bundle_id)
    }

    pub(crate) async fn resolve_or_create_primary_bundle_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        file_version_id: FileVersionId,
        source_path: &Path,
        observed_at: OffsetDateTime,
    ) -> Result<PrimaryBundleResolution, VoomError> {
        let (file_asset_id, bundle_id) = self
            .primary_bundle_for_file_version_in_tx(tx, file_version_id)
            .await?;
        if let Some(bundle_id) = bundle_id {
            return Ok(PrimaryBundleResolution {
                bundle_id,
                created: false,
            });
        }
        let bundle_id = create_primary_bundle_identity_in_tx(
            self,
            tx,
            file_asset_id,
            display_name_from_path(source_path),
            observed_at,
        )
        .await?;
        Ok(PrimaryBundleResolution {
            bundle_id,
            created: true,
        })
    }

    async fn primary_bundle_for_file_version_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        file_version_id: FileVersionId,
    ) -> Result<(FileAssetId, Option<BundleId>), VoomError> {
        let version = self
            .identity
            .get_file_version_in_tx(tx, file_version_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("file_version {file_version_id}")))?;
        self.identity
            .require_active_file_versions_in_tx(tx, &[(version.file_asset_id, file_version_id)])
            .await?;
        let member = self
            .bundles
            .get_member_by_file_asset_in_tx(tx, version.file_asset_id)
            .await?;
        let Some(member) = member else {
            return Ok((version.file_asset_id, None));
        };
        if member.role != BundleMemberRole::PrimaryVideo {
            return Err(VoomError::Conflict(format!(
                "primary asset {} is already a {:?} bundle member",
                version.file_asset_id, member.role
            )));
        }
        Ok((version.file_asset_id, Some(member.bundle_id)))
    }

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

    /// List bundles newest first (`id DESC`) with member counts,
    /// keyset-paginated by `after_id` and bounded by `limit` (ADR 0031).
    ///
    /// # Errors
    /// Propagates `SqliteBundleRepo::list_all` errors.
    pub async fn list_bundles(
        &self,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<(AssetBundle, u64)>, VoomError> {
        self.bundles.list_all(after_id, limit).await
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

async fn create_primary_bundle_identity_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_asset_id: FileAssetId,
    display_name: String,
    observed_at: OffsetDateTime,
) -> Result<BundleId, VoomError> {
    let media_work_id =
        create_provisional_media_work_in_tx(control_plane, tx, &display_name, observed_at).await?;
    let media_variant_id =
        create_provisional_media_variant_in_tx(control_plane, tx, media_work_id, observed_at)
            .await?;
    let bundle = control_plane
        .bundles
        .create_in_tx(
            tx,
            NewAssetBundle {
                media_variant_id,
                display_name,
                created_at: observed_at,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::AssetBundle,
        Some(bundle.id.0),
        observed_at,
        Event::AssetBundleCreated(AssetBundleCreatedPayload {
            bundle_id: bundle.id.0,
            media_variant_id: bundle.media_variant_id.0,
            display_name: bundle.display_name,
        }),
    )
    .await?;
    add_primary_member_in_tx(control_plane, tx, bundle.id, file_asset_id, observed_at).await?;
    Ok(bundle.id)
}

async fn create_provisional_media_work_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    display_name: &str,
    observed_at: OffsetDateTime,
) -> Result<MediaWorkId, VoomError> {
    let work = control_plane
        .identity
        .create_media_work_in_tx(
            tx,
            NewMediaWork {
                kind: MediaWorkKind::Unknown,
                display_title: display_name.to_owned(),
                provisional: true,
                created_at: observed_at,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::MediaWork,
        Some(work.id.0),
        observed_at,
        Event::MediaWorkCreated(MediaWorkCreatedPayload {
            media_work_id: work.id.0,
            kind: work.kind.as_str().to_owned(),
            display_title: work.display_title,
            provisional: work.provisional,
        }),
    )
    .await?;
    Ok(work.id)
}

async fn create_provisional_media_variant_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    media_work_id: MediaWorkId,
    observed_at: OffsetDateTime,
) -> Result<MediaVariantId, VoomError> {
    let variant = control_plane
        .identity
        .create_media_variant_in_tx(
            tx,
            NewMediaVariant {
                media_work_id,
                label: "scan".to_owned(),
                provisional: true,
                created_at: observed_at,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::MediaVariant,
        Some(variant.id.0),
        observed_at,
        Event::MediaVariantCreated(MediaVariantCreatedPayload {
            media_variant_id: variant.id.0,
            media_work_id: variant.media_work_id.0,
            label: variant.label,
            provisional: variant.provisional,
        }),
    )
    .await?;
    Ok(variant.id)
}

async fn add_primary_member_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    bundle_id: BundleId,
    file_asset_id: FileAssetId,
    observed_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let role = BundleMemberRole::PrimaryVideo;
    control_plane
        .bundles
        .add_member_in_tx(
            tx,
            NewBundleMember {
                bundle_id,
                file_asset_id,
                role,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::AssetBundle,
        Some(bundle_id.0),
        observed_at,
        Event::AssetBundleMemberAdded(AssetBundleMemberAddedPayload {
            bundle_id: bundle_id.0,
            file_asset_id: file_asset_id.0,
            role: role.as_str().to_owned(),
        }),
    )
    .await
}

fn display_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map_or_else(|| path.display().to_string(), str::to_owned)
}

#[cfg(test)]
#[path = "bundles_test.rs"]
mod tests;
