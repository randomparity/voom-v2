use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use voom_core::{TranscodeVideoProfile, VoomError};

use super::Repository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoProfile {
    pub id: String,
    pub name: String,
    pub target_codec: String,
    pub encoder: String,
    pub crf: u8,
    pub preset: String,
    pub tune: Option<String>,
    pub codec_profile: Option<String>,
    pub codec_level: Option<String>,
    pub pixel_format: Option<String>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub output_container: String,
    pub copy_compatible: bool,
}

impl VideoProfile {
    /// Projects the durable row into the shared transcode profile, preserving the
    /// registry `name` as the resolved identity.
    #[must_use]
    pub fn to_worker_profile(&self) -> TranscodeVideoProfile {
        TranscodeVideoProfile {
            name: self.name.clone(),
            target_codec: self.target_codec.clone(),
            encoder: self.encoder.clone(),
            crf: self.crf,
            preset: self.preset.clone(),
            tune: self.tune.clone(),
            codec_profile: self.codec_profile.clone(),
            codec_level: self.codec_level.clone(),
            pixel_format: self.pixel_format.clone(),
            max_width: self.max_width,
            max_height: self.max_height,
            copy_compatible: self.copy_compatible,
        }
    }
}

#[async_trait]
pub trait VideoProfileRepo: Repository {
    async fn list(&self) -> Result<Vec<VideoProfile>, VoomError>;
    async fn get_by_name(&self, name: &str) -> Result<Option<VideoProfile>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteVideoProfileRepo {
    pool: SqlitePool,
}

impl SqliteVideoProfileRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteVideoProfileRepo {}

const SELECT_COLUMNS: &str = "id, name, target_codec, encoder, crf, preset, tune, \
    codec_profile, codec_level, pixel_format, max_width, max_height, output_container, \
    copy_compatible";

#[async_trait]
impl VideoProfileRepo for SqliteVideoProfileRepo {
    async fn list(&self) -> Result<Vec<VideoProfile>, VoomError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM video_profiles ORDER BY name ASC");
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("video_profiles list: {e}")))?;
        rows.iter().map(row_to_video_profile).collect()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<VideoProfile>, VoomError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM video_profiles WHERE name = ?");
        let row = sqlx::query(&sql)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("video_profiles get_by_name: {e}")))?;
        row.as_ref().map(row_to_video_profile).transpose()
    }
}

fn row_to_video_profile(row: &sqlx::sqlite::SqliteRow) -> Result<VideoProfile, VoomError> {
    let map = |field: &'static str| {
        move |e: sqlx::Error| VoomError::Database(format!("video_profiles.{field}: {e}"))
    };
    let crf: i64 = row.try_get("crf").map_err(map("crf"))?;
    let copy_compatible: i64 = row
        .try_get("copy_compatible")
        .map_err(map("copy_compatible"))?;
    let max_width: Option<i64> = row.try_get("max_width").map_err(map("max_width"))?;
    let max_height: Option<i64> = row.try_get("max_height").map_err(map("max_height"))?;
    let to_u32 = |value: i64| {
        u32::try_from(value)
            .map_err(|_| VoomError::Database("video_profiles dimension overflow".to_owned()))
    };
    Ok(VideoProfile {
        id: row.try_get("id").map_err(map("id"))?,
        name: row.try_get("name").map_err(map("name"))?,
        target_codec: row.try_get("target_codec").map_err(map("target_codec"))?,
        encoder: row.try_get("encoder").map_err(map("encoder"))?,
        crf: u8::try_from(crf)
            .map_err(|_| VoomError::Database("video_profiles.crf overflow".to_owned()))?,
        preset: row.try_get("preset").map_err(map("preset"))?,
        tune: row.try_get("tune").map_err(map("tune"))?,
        codec_profile: row.try_get("codec_profile").map_err(map("codec_profile"))?,
        codec_level: row.try_get("codec_level").map_err(map("codec_level"))?,
        pixel_format: row.try_get("pixel_format").map_err(map("pixel_format"))?,
        max_width: max_width.map(to_u32).transpose()?,
        max_height: max_height.map(to_u32).transpose()?,
        output_container: row
            .try_get("output_container")
            .map_err(map("output_container"))?,
        copy_compatible: copy_compatible != 0,
    })
}

#[cfg(test)]
#[path = "video_profiles_test.rs"]
mod tests;
