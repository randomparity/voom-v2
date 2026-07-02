//! Library-root rows: one canonical path with discovery/scan settings.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;
use voom_core::{LibraryId, LibraryRootId, VoomError};

use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};
use super::libraries::is_unique_violation;
use super::{SqliteLibraryRepo, begin, commit};

/// Storage backing of a library root. Mirrors `library_roots.root_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryRootKind {
    LocalPath,
    SharedMount,
}

/// Discovery mode a daemon watcher will consume. Mirrors
/// `library_roots.scan_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryScanMode {
    ExplicitOnly,
    ManualRecursive,
    WatchEnabled,
}

/// How discovery treats symlinks under the root. Mirrors
/// `library_roots.symlink_policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    Reject,
    Follow,
}

/// How discovery treats hidden entries. Mirrors
/// `library_roots.hidden_file_policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenFilePolicy {
    Ignore,
    Include,
}

macro_rules! str_enum {
    ($ty:ty, $col:literal, { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $ty {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $s),+
                }
            }

            /// Parse a wire/DB value.
            ///
            /// # Errors
            /// Returns a database error for a value outside the CHECK vocabulary.
            pub fn parse(s: &str) -> Result<Self, VoomError> {
                match s {
                    $($s => Ok(Self::$variant),)+
                    other => Err(VoomError::database(format!(
                        "{} {other:?} not in vocab", $col
                    ))),
                }
            }
        }
    };
}

str_enum!(LibraryRootKind, "library_roots.root_kind", {
    LocalPath => "local_path",
    SharedMount => "shared_mount",
});
str_enum!(LibraryScanMode, "library_roots.scan_mode", {
    ExplicitOnly => "explicit_only",
    ManualRecursive => "manual_recursive",
    WatchEnabled => "watch_enabled",
});
str_enum!(SymlinkPolicy, "library_roots.symlink_policy", {
    Reject => "reject",
    Follow => "follow",
});
str_enum!(HiddenFilePolicy, "library_roots.hidden_file_policy", {
    Ignore => "ignore",
    Include => "include",
});

/// Input for a new library root. Paths are already canonicalized by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLibraryRoot {
    pub library_id: LibraryId,
    pub root_kind: LibraryRootKind,
    pub canonical_path: String,
    pub display_path: String,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extension_allowlist: Vec<String>,
    pub scan_mode: LibraryScanMode,
    pub symlink_policy: SymlinkPolicy,
    pub hidden_file_policy: HiddenFilePolicy,
    pub max_depth: Option<u32>,
    pub stability_seconds: u32,
    pub debounce_seconds: u32,
    pub default_output_root: Option<String>,
    pub default_staging_root: Option<String>,
    pub default_backup_root: Option<String>,
    pub enabled: bool,
}

/// Mutable library-root discovery settings. `None` leaves a field unchanged.
/// `canonical_path`/`display_path`/`library_id`/`root_kind` are immutable: to
/// re-point a root, remove and re-add it (keeping `canonical_path` unique).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryRootUpdate {
    pub include_globs: Option<Vec<String>>,
    pub exclude_globs: Option<Vec<String>>,
    pub extension_allowlist: Option<Vec<String>>,
    pub scan_mode: Option<LibraryScanMode>,
    pub symlink_policy: Option<SymlinkPolicy>,
    pub hidden_file_policy: Option<HiddenFilePolicy>,
    pub max_depth: Option<u32>,
    pub stability_seconds: Option<u32>,
    pub debounce_seconds: Option<u32>,
    pub default_output_root: Option<String>,
    pub default_staging_root: Option<String>,
    pub default_backup_root: Option<String>,
}

impl LibraryRootUpdate {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.include_globs.is_none()
            && self.exclude_globs.is_none()
            && self.extension_allowlist.is_none()
            && self.scan_mode.is_none()
            && self.symlink_policy.is_none()
            && self.hidden_file_policy.is_none()
            && self.max_depth.is_none()
            && self.stability_seconds.is_none()
            && self.debounce_seconds.is_none()
            && self.default_output_root.is_none()
            && self.default_staging_root.is_none()
            && self.default_backup_root.is_none()
    }
}

/// A library-root row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRoot {
    pub id: LibraryRootId,
    pub library_id: LibraryId,
    pub root_kind: LibraryRootKind,
    pub canonical_path: String,
    pub display_path: String,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extension_allowlist: Vec<String>,
    pub scan_mode: LibraryScanMode,
    pub symlink_policy: SymlinkPolicy,
    pub hidden_file_policy: HiddenFilePolicy,
    pub max_depth: Option<u32>,
    pub stability_seconds: u32,
    pub debounce_seconds: u32,
    pub default_output_root: Option<String>,
    pub default_staging_root: Option<String>,
    pub default_backup_root: Option<String>,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

const ROOT_COLS: &str = "id, library_id, root_kind, canonical_path, display_path, \
     include_globs, exclude_globs, extension_allowlist, scan_mode, symlink_policy, \
     hidden_file_policy, max_depth, stability_seconds, debounce_seconds, \
     default_output_root, default_staging_root, default_backup_root, enabled, \
     created_at, updated_at";

impl SqliteLibraryRepo {
    /// Insert a new library root.
    ///
    /// # Errors
    /// Returns `NotFound` when `library_id` does not exist, `Conflict` when
    /// `canonical_path` collides, and propagates database errors.
    pub async fn create_library_root(
        &self,
        input: NewLibraryRoot,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let timestamp = iso8601(now)?;
        let include = serialize_json(&input.include_globs, "include_globs")?;
        let exclude = serialize_json(&input.exclude_globs, "exclude_globs")?;
        let allowlist = serialize_json(&input.extension_allowlist, "extension_allowlist")?;
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query(
            "INSERT INTO library_roots \
             (library_id, root_kind, canonical_path, display_path, include_globs, \
              exclude_globs, extension_allowlist, scan_mode, symlink_policy, \
              hidden_file_policy, max_depth, stability_seconds, debounce_seconds, \
              default_output_root, default_staging_root, default_backup_root, enabled, \
              created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.library_id.0))
        .bind(input.root_kind.as_str())
        .bind(&input.canonical_path)
        .bind(&input.display_path)
        .bind(&include)
        .bind(&exclude)
        .bind(&allowlist)
        .bind(input.scan_mode.as_str())
        .bind(input.symlink_policy.as_str())
        .bind(input.hidden_file_policy.as_str())
        .bind(input.max_depth.map(i64::from))
        .bind(i64::from(input.stability_seconds))
        .bind(i64::from(input.debounce_seconds))
        .bind(&input.default_output_root)
        .bind(&input.default_staging_root)
        .bind(&input.default_backup_root)
        .bind(i64::from(input.enabled))
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| root_insert_error(input.library_id, &input.canonical_path, e))?;
        commit(tx).await?;
        Ok(LibraryRoot {
            id: LibraryRootId(u64_from_i64(res.last_insert_rowid())),
            library_id: input.library_id,
            root_kind: input.root_kind,
            canonical_path: input.canonical_path,
            display_path: input.display_path,
            include_globs: input.include_globs,
            exclude_globs: input.exclude_globs,
            extension_allowlist: input.extension_allowlist,
            scan_mode: input.scan_mode,
            symlink_policy: input.symlink_policy,
            hidden_file_policy: input.hidden_file_policy,
            max_depth: input.max_depth,
            stability_seconds: input.stability_seconds,
            debounce_seconds: input.debounce_seconds,
            default_output_root: input.default_output_root,
            default_staging_root: input.default_staging_root,
            default_backup_root: input.default_backup_root,
            enabled: input.enabled,
            created_at: now,
            updated_at: now,
        })
    }

    /// Get a library root by id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn get_library_root(
        &self,
        id: LibraryRootId,
    ) -> Result<Option<LibraryRoot>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {ROOT_COLS} FROM library_roots WHERE id = ?"
        ))
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("library_roots get", e))?;
        row.as_ref().map(row_to_root).transpose()
    }

    /// List library roots, optionally filtered to one library, in creation
    /// order.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn list_library_roots(
        &self,
        library_id: Option<LibraryId>,
    ) -> Result<Vec<LibraryRoot>, VoomError> {
        let rows = match library_id {
            Some(library_id) => {
                sqlx::query(&format!(
                    "SELECT {ROOT_COLS} FROM library_roots WHERE library_id = ? \
                     ORDER BY created_at ASC, id ASC"
                ))
                .bind(i64_from_u64(library_id.0))
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {ROOT_COLS} FROM library_roots ORDER BY created_at ASC, id ASC"
                ))
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| VoomError::database_context("library_roots list", e))?;
        rows.iter().map(row_to_root).collect()
    }

    /// Apply a partial update to a library root, rewriting `updated_at`.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id and propagates database errors.
    pub async fn update_library_root(
        &self,
        id: LibraryRootId,
        update: LibraryRootUpdate,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let timestamp = iso8601(now)?;
        let include = update
            .include_globs
            .as_ref()
            .map(|v| serialize_json(v, "include_globs"))
            .transpose()?;
        let exclude = update
            .exclude_globs
            .as_ref()
            .map(|v| serialize_json(v, "exclude_globs"))
            .transpose()?;
        let allowlist = update
            .extension_allowlist
            .as_ref()
            .map(|v| serialize_json(v, "extension_allowlist"))
            .transpose()?;
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query(
            "UPDATE library_roots SET \
                 include_globs = COALESCE(?, include_globs), \
                 exclude_globs = COALESCE(?, exclude_globs), \
                 extension_allowlist = COALESCE(?, extension_allowlist), \
                 scan_mode = COALESCE(?, scan_mode), \
                 symlink_policy = COALESCE(?, symlink_policy), \
                 hidden_file_policy = COALESCE(?, hidden_file_policy), \
                 max_depth = COALESCE(?, max_depth), \
                 stability_seconds = COALESCE(?, stability_seconds), \
                 debounce_seconds = COALESCE(?, debounce_seconds), \
                 default_output_root = COALESCE(?, default_output_root), \
                 default_staging_root = COALESCE(?, default_staging_root), \
                 default_backup_root = COALESCE(?, default_backup_root), \
                 updated_at = ? \
             WHERE id = ?",
        )
        .bind(include)
        .bind(exclude)
        .bind(allowlist)
        .bind(update.scan_mode.map(LibraryScanMode::as_str))
        .bind(update.symlink_policy.map(SymlinkPolicy::as_str))
        .bind(update.hidden_file_policy.map(HiddenFilePolicy::as_str))
        .bind(update.max_depth.map(i64::from))
        .bind(update.stability_seconds.map(i64::from))
        .bind(update.debounce_seconds.map(i64::from))
        .bind(update.default_output_root.as_ref())
        .bind(update.default_staging_root.as_ref())
        .bind(update.default_backup_root.as_ref())
        .bind(&timestamp)
        .bind(i64_from_u64(id.0))
        .execute(&mut *tx)
        .await
        .map_err(|e| VoomError::database_context("library_roots update", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::NotFound(format!("library root {id} not found")));
        }
        commit(tx).await?;
        self.get_library_root(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library root {id} not found")))
    }

    /// Flip a root's `enabled` flag, rewriting `updated_at`.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id and propagates database errors.
    pub async fn set_library_root_enabled(
        &self,
        id: LibraryRootId,
        enabled: bool,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let timestamp = iso8601(now)?;
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query("UPDATE library_roots SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(i64::from(enabled))
            .bind(&timestamp)
            .bind(i64_from_u64(id.0))
            .execute(&mut *tx)
            .await
            .map_err(|e| VoomError::database_context("library_roots set_enabled", e))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::NotFound(format!("library root {id} not found")));
        }
        commit(tx).await?;
        self.get_library_root(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library root {id} not found")))
    }

    /// Delete a library root. Returns whether a row was removed.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn delete_library_root(&self, id: LibraryRootId) -> Result<bool, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let res = sqlx::query("DELETE FROM library_roots WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .execute(&mut *tx)
            .await
            .map_err(|e| VoomError::database_context("library_roots delete", e))?;
        commit(tx).await?;
        Ok(res.rows_affected() > 0)
    }
}

fn root_insert_error(library_id: LibraryId, canonical_path: &str, err: sqlx::Error) -> VoomError {
    if is_unique_violation(&err) {
        return VoomError::Conflict(format!(
            "library root canonical_path {canonical_path:?} already exists"
        ));
    }
    if is_foreign_key_violation(&err) {
        return VoomError::NotFound(format!("library {library_id} not found"));
    }
    VoomError::database_context("library_roots insert", err)
}

fn is_foreign_key_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_foreign_key_violation(),
        _ => false,
    }
}

fn json_list(row: &SqliteRow, column: &'static str) -> Result<Vec<String>, VoomError> {
    let raw: String = row
        .try_get(column)
        .map_err(|e| map_row_err("library_roots", &e))?;
    serde_json::from_str(&raw)
        .map_err(|e| VoomError::database(format!("library_roots.{column} decode: {e}")))
}

fn row_to_root(row: &SqliteRow) -> Result<LibraryRoot, VoomError> {
    let t = "library_roots";
    let id: i64 = row.try_get("id").map_err(|e| map_row_err(t, &e))?;
    let library_id: i64 = row.try_get("library_id").map_err(|e| map_row_err(t, &e))?;
    let root_kind: String = row.try_get("root_kind").map_err(|e| map_row_err(t, &e))?;
    let canonical_path: String = row
        .try_get("canonical_path")
        .map_err(|e| map_row_err(t, &e))?;
    let display_path: String = row
        .try_get("display_path")
        .map_err(|e| map_row_err(t, &e))?;
    let scan_mode: String = row.try_get("scan_mode").map_err(|e| map_row_err(t, &e))?;
    let symlink_policy: String = row
        .try_get("symlink_policy")
        .map_err(|e| map_row_err(t, &e))?;
    let hidden_file_policy: String = row
        .try_get("hidden_file_policy")
        .map_err(|e| map_row_err(t, &e))?;
    let max_depth: Option<i64> = row.try_get("max_depth").map_err(|e| map_row_err(t, &e))?;
    let stability_seconds: i64 = row
        .try_get("stability_seconds")
        .map_err(|e| map_row_err(t, &e))?;
    let debounce_seconds: i64 = row
        .try_get("debounce_seconds")
        .map_err(|e| map_row_err(t, &e))?;
    let default_output_root: Option<String> = row
        .try_get("default_output_root")
        .map_err(|e| map_row_err(t, &e))?;
    let default_staging_root: Option<String> = row
        .try_get("default_staging_root")
        .map_err(|e| map_row_err(t, &e))?;
    let default_backup_root: Option<String> = row
        .try_get("default_backup_root")
        .map_err(|e| map_row_err(t, &e))?;
    let enabled: i64 = row.try_get("enabled").map_err(|e| map_row_err(t, &e))?;
    let created_at: String = row.try_get("created_at").map_err(|e| map_row_err(t, &e))?;
    let updated_at: String = row.try_get("updated_at").map_err(|e| map_row_err(t, &e))?;
    Ok(LibraryRoot {
        id: LibraryRootId(u64_from_i64(id)),
        library_id: LibraryId(u64_from_i64(library_id)),
        root_kind: LibraryRootKind::parse(&root_kind)?,
        canonical_path,
        display_path,
        include_globs: json_list(row, "include_globs")?,
        exclude_globs: json_list(row, "exclude_globs")?,
        extension_allowlist: json_list(row, "extension_allowlist")?,
        scan_mode: LibraryScanMode::parse(&scan_mode)?,
        symlink_policy: SymlinkPolicy::parse(&symlink_policy)?,
        hidden_file_policy: HiddenFilePolicy::parse(&hidden_file_policy)?,
        max_depth: max_depth.map(u32_from_i64).transpose()?,
        stability_seconds: u32_from_i64(stability_seconds)?,
        debounce_seconds: u32_from_i64(debounce_seconds)?,
        default_output_root,
        default_staging_root,
        default_backup_root,
        enabled: enabled != 0,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "library_roots_test.rs"]
mod tests;
