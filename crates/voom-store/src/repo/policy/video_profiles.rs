use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{TranscodeVideoProfile, VoomError, encoder_descriptor};

use super::Repository;
use super::common::{iso8601, parse_iso8601};

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
    /// Soft-retire marker (migration 0021). `None` for active profiles; a
    /// retired profile is hidden from `list` but still resolves by name.
    pub retired_at: Option<OffsetDateTime>,
}

/// Mutable fields of a durable video profile, supplied on create and
/// full-replace update. `name` is the stable, `UNIQUE` key; `target_codec` is
/// derived from the encoder rather than supplied. Validated against the
/// encoder's capability descriptor before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVideoProfile {
    pub name: String,
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

impl NewVideoProfile {
    /// Validate every field against the encoder's capability descriptor — the
    /// same rules the policy compiler applies to inline `transcode video`
    /// profiles — and return the derived target codec.
    ///
    /// # Errors
    /// [`VoomError::Config`] when the encoder is unknown, a field falls outside
    /// the encoder's vocabulary/range, or the container/dimensions are invalid.
    fn validate(&self) -> Result<&'static str, VoomError> {
        if self.name.trim().is_empty() {
            return Err(VoomError::Config(
                "video profile name must not be empty".to_owned(),
            ));
        }
        let descriptor = encoder_descriptor(&self.encoder)
            .ok_or_else(|| VoomError::Config(format!("unknown encoder `{}`", self.encoder)))?;
        let target_codec = descriptor.target_codec;
        let typed = TranscodeVideoProfile {
            name: self.name.clone(),
            target_codec: target_codec.to_owned(),
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
        };
        voom_core::validate_profile_against_descriptor(&typed).map_err(VoomError::Config)?;
        if !voom_core::is_supported_transcode_video_container(&self.output_container) {
            return Err(VoomError::Config(format!(
                "output_container `{}` must be mkv or mp4",
                self.output_container
            )));
        }
        for (field, value) in [
            ("max_width", self.max_width),
            ("max_height", self.max_height),
        ] {
            if value == Some(0) {
                return Err(VoomError::Config(format!("{field} must be greater than 0")));
            }
        }
        Ok(target_codec)
    }
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
    copy_compatible, retired_at";

impl SqliteVideoProfileRepo {
    /// List active (non-retired) profiles ordered by name.
    pub async fn list(&self) -> Result<Vec<VideoProfile>, VoomError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM video_profiles \
             WHERE retired_at IS NULL ORDER BY name ASC"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("video_profiles list", e))?;
        rows.iter().map(row_to_video_profile).collect()
    }

    /// Resolve a profile by name regardless of retire status, so a policy that
    /// pins a since-retired profile still resolves.
    pub async fn get_by_name(&self, name: &str) -> Result<Option<VideoProfile>, VoomError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM video_profiles WHERE name = ?");
        let row = sqlx::query(&sql)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("video_profiles get_by_name", e))?;
        row.as_ref().map(row_to_video_profile).transpose()
    }

    /// Insert a durable video profile after validating it against its encoder's
    /// capability descriptor. The row `id` is `vp-{name}`, 1:1 with the `UNIQUE`
    /// name.
    ///
    /// # Errors
    /// [`VoomError::Config`] for an invalid field, [`VoomError::Conflict`] for a
    /// duplicate name, or a database error.
    pub async fn create(&self, input: NewVideoProfile) -> Result<VideoProfile, VoomError> {
        let target_codec = input.validate()?;
        let id = format!("vp-{}", input.name);
        let res = sqlx::query(
            "INSERT INTO video_profiles \
             (id, name, target_codec, encoder, crf, preset, tune, codec_profile, codec_level, \
              pixel_format, max_width, max_height, output_container, copy_compatible) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&input.name)
        .bind(target_codec)
        .bind(&input.encoder)
        .bind(i64::from(input.crf))
        .bind(&input.preset)
        .bind(input.tune.as_deref())
        .bind(input.codec_profile.as_deref())
        .bind(input.codec_level.as_deref())
        .bind(input.pixel_format.as_deref())
        .bind(input.max_width.map(i64::from))
        .bind(input.max_height.map(i64::from))
        .bind(&input.output_container)
        .bind(i64::from(input.copy_compatible))
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(VideoProfile {
                id,
                name: input.name,
                target_codec: target_codec.to_owned(),
                encoder: input.encoder,
                crf: input.crf,
                preset: input.preset,
                tune: input.tune,
                codec_profile: input.codec_profile,
                codec_level: input.codec_level,
                pixel_format: input.pixel_format,
                max_width: input.max_width,
                max_height: input.max_height,
                output_container: input.output_container,
                copy_compatible: input.copy_compatible,
                retired_at: None,
            }),
            Err(err) => Err(self.classify_insert_error(&input.name, err).await),
        }
    }

    async fn classify_insert_error(&self, name: &str, err: sqlx::Error) -> VoomError {
        match self.get_by_name(name).await {
            Ok(Some(_)) => VoomError::Conflict(format!("video profile {name:?} already exists")),
            _ => VoomError::database_context("video_profiles create", err),
        }
    }

    /// Full-replace update keyed by `input.name`, re-validating every field.
    /// Preserves `id` and `retired_at`. Returns `None` when no profile has that
    /// name.
    ///
    /// # Errors
    /// [`VoomError::Config`] for an invalid field, or a database error.
    pub async fn update(&self, input: NewVideoProfile) -> Result<Option<VideoProfile>, VoomError> {
        let target_codec = input.validate()?;
        let affected = sqlx::query(
            "UPDATE video_profiles SET \
                 target_codec = ?, encoder = ?, crf = ?, preset = ?, tune = ?, \
                 codec_profile = ?, codec_level = ?, pixel_format = ?, max_width = ?, \
                 max_height = ?, output_container = ?, copy_compatible = ? \
             WHERE name = ?",
        )
        .bind(target_codec)
        .bind(&input.encoder)
        .bind(i64::from(input.crf))
        .bind(&input.preset)
        .bind(input.tune.as_deref())
        .bind(input.codec_profile.as_deref())
        .bind(input.codec_level.as_deref())
        .bind(input.pixel_format.as_deref())
        .bind(input.max_width.map(i64::from))
        .bind(input.max_height.map(i64::from))
        .bind(&input.output_container)
        .bind(i64::from(input.copy_compatible))
        .bind(&input.name)
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("video_profiles update", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        self.get_by_name(&input.name).await
    }

    /// Soft-retire the profile by name, stamping `retired_at`. Idempotent: a
    /// re-retire preserves the first stamp. Returns `None` when no profile has
    /// that name.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn retire(
        &self,
        name: &str,
        now: OffsetDateTime,
    ) -> Result<Option<VideoProfile>, VoomError> {
        let ts = iso8601(now)?;
        sqlx::query(
            "UPDATE video_profiles SET retired_at = ? WHERE name = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("video_profiles retire", e))?;
        self.get_by_name(name).await
    }
}

fn row_to_video_profile(row: &sqlx::sqlite::SqliteRow) -> Result<VideoProfile, VoomError> {
    let map = |field: &'static str| {
        move |e: sqlx::Error| VoomError::database_context(format!("video_profiles.{field}"), e)
    };
    let crf: i64 = row.try_get("crf").map_err(map("crf"))?;
    let copy_compatible: i64 = row
        .try_get("copy_compatible")
        .map_err(map("copy_compatible"))?;
    let max_width: Option<i64> = row.try_get("max_width").map_err(map("max_width"))?;
    let max_height: Option<i64> = row.try_get("max_height").map_err(map("max_height"))?;
    let retired_at: Option<String> = row.try_get("retired_at").map_err(map("retired_at"))?;
    let to_u32 = |value: i64| {
        u32::try_from(value).map_err(|_| VoomError::database("video_profiles dimension overflow"))
    };
    Ok(VideoProfile {
        id: row.try_get("id").map_err(map("id"))?,
        name: row.try_get("name").map_err(map("name"))?,
        target_codec: row.try_get("target_codec").map_err(map("target_codec"))?,
        encoder: row.try_get("encoder").map_err(map("encoder"))?,
        crf: u8::try_from(crf).map_err(|_| VoomError::database("video_profiles.crf overflow"))?,
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
        retired_at: retired_at.as_deref().map(parse_iso8601).transpose()?,
    })
}

#[cfg(test)]
#[path = "video_profiles_test.rs"]
mod tests;
