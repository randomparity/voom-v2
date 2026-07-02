//! Library rows: a `library` groups roots and default policy intent.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;
use voom_core::{LibraryId, VoomError};

use super::super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u64_from_i64};
use super::{SqliteLibraryRepo, begin, commit};

/// Expected media kind of a library. Mirrors the `libraries.media_kind` CHECK
/// and the existing `media_works.kind` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryMediaKind {
    Movie,
    Episode,
    Personal,
    Unknown,
}

str_enum!(LibraryMediaKind, "libraries.media_kind", {
    Movie => "movie",
    Episode => "episode",
    Personal => "personal",
    Unknown => "unknown",
});

/// Input for a new library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLibrary {
    pub slug: String,
    pub display_name: String,
    pub media_kind: LibraryMediaKind,
    pub description: Option<String>,
    pub enabled: bool,
}

/// Mutable library attributes. `None` leaves a field unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryUpdate {
    pub display_name: Option<String>,
    pub media_kind: Option<LibraryMediaKind>,
    pub description: Option<String>,
}

/// A library row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    pub id: LibraryId,
    pub slug: String,
    pub display_name: String,
    pub media_kind: LibraryMediaKind,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

const LIBRARY_COLS: &str =
    "id, slug, display_name, media_kind, description, enabled, created_at, updated_at";

impl SqliteLibraryRepo {
    /// Insert a new library.
    ///
    /// # Errors
    /// Returns `Conflict` when `slug` collides, and propagates database errors.
    pub async fn create_library(
        &self,
        input: NewLibrary,
        now: OffsetDateTime,
    ) -> Result<Library, VoomError> {
        let timestamp = iso8601(now)?;
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query(
            "INSERT INTO libraries \
             (slug, display_name, media_kind, description, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.slug)
        .bind(&input.display_name)
        .bind(input.media_kind.as_str())
        .bind(&input.description)
        .bind(i64::from(input.enabled))
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| library_insert_error(&input.slug, e))?;
        commit(tx).await?;
        Ok(Library {
            id: LibraryId(u64_from_i64(res.last_insert_rowid())),
            slug: input.slug,
            display_name: input.display_name,
            media_kind: input.media_kind,
            description: input.description,
            enabled: input.enabled,
            created_at: now,
            updated_at: now,
        })
    }

    /// Get a library by id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_library(&self, id: LibraryId) -> Result<Option<Library>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {LIBRARY_COLS} FROM libraries WHERE id = ?"
        ))
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("libraries get", e))?;
        row.as_ref().map(row_to_library).transpose()
    }

    /// Get a library by its unique slug.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_library_by_slug(&self, slug: &str) -> Result<Option<Library>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {LIBRARY_COLS} FROM libraries WHERE slug = ?"
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("libraries get_by_slug", e))?;
        row.as_ref().map(row_to_library).transpose()
    }

    /// List libraries in creation order.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn list_libraries(&self) -> Result<Vec<Library>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {LIBRARY_COLS} FROM libraries ORDER BY created_at ASC, id ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("libraries list", e))?;
        rows.iter().map(row_to_library).collect()
    }

    /// Apply a partial update to a library, rewriting `updated_at`.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id and propagates database errors.
    pub async fn update_library(
        &self,
        id: LibraryId,
        update: LibraryUpdate,
        now: OffsetDateTime,
    ) -> Result<Library, VoomError> {
        let timestamp = iso8601(now)?;
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query(
            "UPDATE libraries SET \
                 display_name = COALESCE(?, display_name), \
                 media_kind = COALESCE(?, media_kind), \
                 description = COALESCE(?, description), \
                 updated_at = ? \
             WHERE id = ?",
        )
        .bind(update.display_name.as_ref())
        .bind(update.media_kind.map(LibraryMediaKind::as_str))
        .bind(update.description.as_ref())
        .bind(&timestamp)
        .bind(i64_from_u64(id.0))
        .execute(&mut *tx)
        .await
        .map_err(|e| VoomError::database_context("libraries update", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::NotFound(format!("library {id} not found")));
        }
        commit(tx).await?;
        self.get_library(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library {id} not found")))
    }

    /// Flip a library's `enabled` flag, rewriting `updated_at`.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id and propagates database errors.
    pub async fn set_library_enabled(
        &self,
        id: LibraryId,
        enabled: bool,
        now: OffsetDateTime,
    ) -> Result<Library, VoomError> {
        let timestamp = iso8601(now)?;
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query("UPDATE libraries SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(i64::from(enabled))
            .bind(&timestamp)
            .bind(i64_from_u64(id.0))
            .execute(&mut *tx)
            .await
            .map_err(|e| VoomError::database_context("libraries set_enabled", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::NotFound(format!("library {id} not found")));
        }
        commit(tx).await?;
        self.get_library(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library {id} not found")))
    }

    /// Delete a library. Its roots cascade (FK `ON DELETE CASCADE`). Returns
    /// whether a row was removed.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn delete_library(&self, id: LibraryId) -> Result<bool, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query("DELETE FROM libraries WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .execute(&mut *tx)
            .await
            .map_err(|e| VoomError::database_context("libraries delete", e))?;
        commit(tx).await?;
        Ok(res.rows_affected() > 0)
    }
}

fn library_insert_error(slug: &str, err: sqlx::Error) -> VoomError {
    if is_unique_violation(&err) {
        return VoomError::Conflict(format!("library slug {slug:?} already exists"));
    }
    VoomError::database_context("libraries insert", err)
}

/// True when a `sqlx::Error` wraps a `SQLite` UNIQUE/PRIMARY-KEY constraint
/// violation.
pub(super) fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn row_to_library(row: &SqliteRow) -> Result<Library, VoomError> {
    let t = "libraries";
    let id: i64 = row.try_get("id").map_err(|e| map_row_err(t, &e))?;
    let slug: String = row.try_get("slug").map_err(|e| map_row_err(t, &e))?;
    let display_name: String = row
        .try_get("display_name")
        .map_err(|e| map_row_err(t, &e))?;
    let media_kind: String = row.try_get("media_kind").map_err(|e| map_row_err(t, &e))?;
    let description: Option<String> = row.try_get("description").map_err(|e| map_row_err(t, &e))?;
    let enabled: i64 = row.try_get("enabled").map_err(|e| map_row_err(t, &e))?;
    let created_at: String = row.try_get("created_at").map_err(|e| map_row_err(t, &e))?;
    let updated_at: String = row.try_get("updated_at").map_err(|e| map_row_err(t, &e))?;
    Ok(Library {
        id: LibraryId(u64_from_i64(id)),
        slug,
        display_name,
        media_kind: LibraryMediaKind::parse(&media_kind)?,
        description,
        enabled: enabled != 0,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "libraries_test.rs"]
mod tests;
