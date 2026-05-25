//! `BundleRepo` — `asset_bundles` + `asset_bundle_members`. Owns the
//! membership UNIQUE-on-file_asset_id invariant (an asset is a member
//! of at most one bundle at a time per spec §8.3). CASCADE on
//! `asset_bundles` deletion drops member rows transparently.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{BundleId, FileAssetId, MediaVariantId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u64_from_i64};

/// `asset_bundle_members.role` vocabulary. Mirrors the SQL CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BundleMemberRole {
    PrimaryVideo,
    CommentaryAudio,
    ExternalSubtitle,
    Poster,
    Nfo,
    Trailer,
    Transcript,
    Thumbnail,
    Report,
}

impl BundleMemberRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryVideo => "primary_video",
            Self::CommentaryAudio => "commentary_audio",
            Self::ExternalSubtitle => "external_subtitle",
            Self::Poster => "poster",
            Self::Nfo => "nfo",
            Self::Trailer => "trailer",
            Self::Transcript => "transcript",
            Self::Thumbnail => "thumbnail",
            Self::Report => "report",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "primary_video" => Ok(Self::PrimaryVideo),
            "commentary_audio" => Ok(Self::CommentaryAudio),
            "external_subtitle" => Ok(Self::ExternalSubtitle),
            "poster" => Ok(Self::Poster),
            "nfo" => Ok(Self::Nfo),
            "trailer" => Ok(Self::Trailer),
            "transcript" => Ok(Self::Transcript),
            "thumbnail" => Ok(Self::Thumbnail),
            "report" => Ok(Self::Report),
            other => Err(VoomError::Database(format!(
                "asset_bundle_members.role {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewAssetBundle {
    pub media_variant_id: MediaVariantId,
    pub display_name: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct AssetBundle {
    pub id: BundleId,
    pub media_variant_id: MediaVariantId,
    pub display_name: String,
    pub created_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct NewBundleMember {
    pub bundle_id: BundleId,
    pub file_asset_id: FileAssetId,
    pub role: BundleMemberRole,
}

#[derive(Debug, Clone)]
pub struct BundleMember {
    pub id: u64,
    pub bundle_id: BundleId,
    pub file_asset_id: FileAssetId,
    pub role: BundleMemberRole,
}

#[async_trait]
pub trait BundleRepo: Repository {
    async fn create_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewAssetBundle,
    ) -> Result<AssetBundle, VoomError>;
    async fn create(&self, input: NewAssetBundle) -> Result<AssetBundle, VoomError>;
    async fn get(&self, id: BundleId) -> Result<Option<AssetBundle>, VoomError>;
    async fn list_by_variant(
        &self,
        media_variant_id: MediaVariantId,
    ) -> Result<Vec<AssetBundle>, VoomError>;
    async fn update_display_name_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: BundleId,
        display_name: String,
        expected_epoch: u64,
    ) -> Result<AssetBundle, VoomError>;

    async fn add_member_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewBundleMember,
    ) -> Result<BundleMember, VoomError>;
    async fn get_member_by_file_asset_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        file_asset_id: FileAssetId,
    ) -> Result<Option<BundleMember>, VoomError>;
    async fn add_member(&self, input: NewBundleMember) -> Result<BundleMember, VoomError>;
    async fn remove_member_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        bundle_id: BundleId,
        file_asset_id: FileAssetId,
    ) -> Result<BundleMember, VoomError>;
    async fn list_members(&self, bundle_id: BundleId) -> Result<Vec<BundleMember>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteBundleRepo {
    pool: SqlitePool,
}

impl SqliteBundleRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteBundleRepo {}

#[async_trait]
impl BundleRepo for SqliteBundleRepo {
    async fn create_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewAssetBundle,
    ) -> Result<AssetBundle, VoomError> {
        let ts = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO asset_bundles (media_variant_id, display_name, created_at) \
             VALUES (?, ?, ?)",
        )
        .bind(i64_from_u64(input.media_variant_id.0))
        .bind(&input.display_name)
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundles insert: {e}")))?;
        let id = BundleId(u64_from_i64(res.last_insert_rowid()));
        get_bundle_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::Internal(format!("asset_bundles post-insert get: {id}")))
    }

    async fn create(&self, input: NewAssetBundle) -> Result<AssetBundle, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get(&self, id: BundleId) -> Result<Option<AssetBundle>, VoomError> {
        let row = sqlx::query(SELECT_BUNDLE_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("asset_bundles get: {e}")))?;
        row.as_ref().map(row_to_bundle).transpose()
    }

    async fn list_by_variant(
        &self,
        media_variant_id: MediaVariantId,
    ) -> Result<Vec<AssetBundle>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, media_variant_id, display_name, created_at, epoch \
             FROM asset_bundles WHERE media_variant_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(media_variant_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundles list: {e}")))?;
        rows.iter().map(row_to_bundle).collect()
    }

    async fn update_display_name_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: BundleId,
        display_name: String,
        expected_epoch: u64,
    ) -> Result<AssetBundle, VoomError> {
        let res = sqlx::query(
            "UPDATE asset_bundles SET display_name = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ?",
        )
        .bind(&display_name)
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundles update: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "asset_bundles update_display_name: id={id} expected_epoch={expected_epoch} mismatch"
            )));
        }
        get_bundle_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::Internal(format!("asset_bundles post-update get: {id}")))
    }

    async fn add_member_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewBundleMember,
    ) -> Result<BundleMember, VoomError> {
        let res = sqlx::query(
            "INSERT INTO asset_bundle_members (bundle_id, file_asset_id, role) \
             VALUES (?, ?, ?)",
        )
        .bind(i64_from_u64(input.bundle_id.0))
        .bind(i64_from_u64(input.file_asset_id.0))
        .bind(input.role.as_str())
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            // The schema's UNIQUE(file_asset_id) surfaces as a SQLite
            // constraint failure. Map it to a typed Conflict so callers
            // can distinguish duplicate-membership from other DB faults.
            let msg = format!("asset_bundle_members insert: {e}");
            if msg.contains("UNIQUE constraint failed") {
                VoomError::Conflict(format!(
                    "asset {} already a bundle member",
                    input.file_asset_id
                ))
            } else {
                VoomError::Database(msg)
            }
        })?;
        let id = u64_from_i64(res.last_insert_rowid());
        Ok(BundleMember {
            id,
            bundle_id: input.bundle_id,
            file_asset_id: input.file_asset_id,
            role: input.role,
        })
    }

    async fn add_member(&self, input: NewBundleMember) -> Result<BundleMember, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.add_member_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get_member_by_file_asset_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        file_asset_id: FileAssetId,
    ) -> Result<Option<BundleMember>, VoomError> {
        let row = sqlx::query(
            "SELECT id, bundle_id, file_asset_id, role FROM asset_bundle_members \
             WHERE file_asset_id = ?",
        )
        .bind(i64_from_u64(file_asset_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundle_members get_by_asset: {e}")))?;
        row.as_ref().map(row_to_bundle_member).transpose()
    }

    async fn remove_member_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        bundle_id: BundleId,
        file_asset_id: FileAssetId,
    ) -> Result<BundleMember, VoomError> {
        let row: Option<sqlx::sqlite::SqliteRow> = sqlx::query(
            "DELETE FROM asset_bundle_members \
             WHERE bundle_id = ? AND file_asset_id = ? \
             RETURNING id, bundle_id, file_asset_id, role",
        )
        .bind(i64_from_u64(bundle_id.0))
        .bind(i64_from_u64(file_asset_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundle_members delete: {e}")))?;
        let row = row.ok_or_else(|| {
            VoomError::NotFound(format!(
                "asset_bundle_members not found: bundle={bundle_id} asset={file_asset_id}"
            ))
        })?;
        row_to_bundle_member(&row)
    }

    async fn list_members(&self, bundle_id: BundleId) -> Result<Vec<BundleMember>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, bundle_id, file_asset_id, role FROM asset_bundle_members \
             WHERE bundle_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(bundle_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundle_members list: {e}")))?;
        rows.iter().map(row_to_bundle_member).collect()
    }
}

const SELECT_BUNDLE_COLS: &str = "SELECT id, media_variant_id, display_name, created_at, epoch \
     FROM asset_bundles WHERE id = ?";

async fn get_bundle_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: BundleId,
) -> Result<Option<AssetBundle>, VoomError> {
    let row = sqlx::query(SELECT_BUNDLE_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_bundles get_in_tx: {e}")))?;
    row.as_ref().map(row_to_bundle).transpose()
}

fn row_to_bundle(row: &sqlx::sqlite::SqliteRow) -> Result<AssetBundle, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("asset_bundles", &e))?;
    let media_variant_id: i64 = row
        .try_get("media_variant_id")
        .map_err(|e| map_row_err("asset_bundles", &e))?;
    let display_name: String = row
        .try_get("display_name")
        .map_err(|e| map_row_err("asset_bundles", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("asset_bundles", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("asset_bundles", &e))?;
    Ok(AssetBundle {
        id: BundleId(u64_from_i64(id)),
        media_variant_id: MediaVariantId(u64_from_i64(media_variant_id)),
        display_name,
        created_at: parse_iso8601(&created_at)?,
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_bundle_member(row: &sqlx::sqlite::SqliteRow) -> Result<BundleMember, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("asset_bundle_members", &e))?;
    let bundle_id: i64 = row
        .try_get("bundle_id")
        .map_err(|e| map_row_err("asset_bundle_members", &e))?;
    let file_asset_id: i64 = row
        .try_get("file_asset_id")
        .map_err(|e| map_row_err("asset_bundle_members", &e))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| map_row_err("asset_bundle_members", &e))?;
    Ok(BundleMember {
        id: u64_from_i64(id),
        bundle_id: BundleId(u64_from_i64(bundle_id)),
        file_asset_id: FileAssetId(u64_from_i64(file_asset_id)),
        role: BundleMemberRole::parse(&role)?,
    })
}

#[cfg(test)]
#[path = "bundles_test.rs"]
mod tests;
