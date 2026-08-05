//! Durable node-owned storage roots and their library scan configuration.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{
    LibraryId, NodeId, NodeStatus, ProviderLocator, StorageProviderKind, StorageRootId,
    StorageRootState, VoomError,
};

use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};
use super::libraries::is_unique_violation;
use super::{SqliteLibraryRepo, begin, commit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryScanMode {
    ExplicitOnly,
    ManualRecursive,
    WatchEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    Reject,
    Follow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenFilePolicy {
    Ignore,
    Include,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLibraryRoot {
    pub library_id: LibraryId,
    pub owner_node_id: NodeId,
    pub provider_kind: StorageProviderKind,
    pub provider_locator: ProviderLocator,
    pub display_locator: String,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extension_allowlist: Vec<String>,
    pub scan_mode: LibraryScanMode,
    pub symlink_policy: SymlinkPolicy,
    pub hidden_file_policy: HiddenFilePolicy,
    pub max_depth: Option<u32>,
    pub stability_seconds: u32,
    pub debounce_seconds: u32,
    pub default_output_root_id: Option<StorageRootId>,
    pub default_staging_root_id: Option<StorageRootId>,
    pub default_backup_root_id: Option<StorageRootId>,
    pub enabled: bool,
}

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
    pub default_output_root_id: Option<Option<StorageRootId>>,
    pub default_staging_root_id: Option<Option<StorageRootId>>,
    pub default_backup_root_id: Option<Option<StorageRootId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRoot {
    pub id: StorageRootId,
    pub library_id: LibraryId,
    pub owner_node_id: Option<NodeId>,
    pub provider_kind: StorageProviderKind,
    pub provider_locator: ProviderLocator,
    pub display_locator: String,
    pub state: StorageRootState,
    pub root_epoch: u64,
    pub activation_identity: Option<String>,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extension_allowlist: Vec<String>,
    pub scan_mode: LibraryScanMode,
    pub symlink_policy: SymlinkPolicy,
    pub hidden_file_policy: HiddenFilePolicy,
    pub max_depth: Option<u32>,
    pub stability_seconds: u32,
    pub debounce_seconds: u32,
    pub default_output_root_id: Option<StorageRootId>,
    pub default_staging_root_id: Option<StorageRootId>,
    pub default_backup_root_id: Option<StorageRootId>,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootAvailabilityReason {
    Available,
    LibraryDisabled,
    RootDisabled,
    RootUnassigned,
    RootNotActive,
    OwnerRegistered,
    OwnerStale,
    OwnerRetired,
}

impl RootAvailabilityReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::LibraryDisabled => "library_disabled",
            Self::RootDisabled => "root_disabled",
            Self::RootUnassigned => "root_unassigned",
            Self::RootNotActive => "root_not_active",
            Self::OwnerRegistered => "owner_registered",
            Self::OwnerStale => "owner_stale",
            Self::OwnerRetired => "owner_retired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLibraryRoot {
    pub root: LibraryRoot,
    pub available: bool,
    pub reason: RootAvailabilityReason,
}

const ROOT_COLS: &str = "r.id, r.library_id, r.owner_node_id, r.provider_kind, \
    r.provider_locator, r.display_locator, r.state, r.root_epoch, r.activation_identity, \
    r.include_globs, r.exclude_globs, r.extension_allowlist, r.scan_mode, r.symlink_policy, \
    r.hidden_file_policy, r.max_depth, r.stability_seconds, r.debounce_seconds, \
    r.default_output_root_id, r.default_staging_root_id, r.default_backup_root_id, r.enabled, \
    r.created_at, r.updated_at";

impl SqliteLibraryRepo {
    pub async fn create_library_root(
        &self,
        input: NewLibraryRoot,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let mut tx = begin(&self.pool).await?;
        let root = self.create_library_root_in_tx(&mut tx, input, now).await?;
        commit(tx).await?;
        Ok(root)
    }

    pub async fn create_library_root_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewLibraryRoot,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        require_owner_not_retired(tx, input.owner_node_id).await?;
        require_defaults_in_library(tx, input.library_id, &input).await?;
        let timestamp = iso8601(now)?;
        let include = serialize_json(&input.include_globs, "include_globs")?;
        let exclude = serialize_json(&input.exclude_globs, "exclude_globs")?;
        let allowlist = serialize_json(&input.extension_allowlist, "extension_allowlist")?;
        let res = insert_root(tx, &input, &timestamp, &include, &exclude, &allowlist).await?;
        let id = StorageRootId(u64_from_i64(res, "library_roots.id")?);
        get_root_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("library_roots post-insert row vanished: {id}"))
        })
    }

    pub async fn get_library_root(
        &self,
        id: StorageRootId,
    ) -> Result<Option<LibraryRoot>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {ROOT_COLS} FROM library_roots r WHERE r.id = ?"
        ))
        .bind(root_i64(id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("library_roots get", error))?;
        row.as_ref().map(row_to_root).transpose()
    }

    pub async fn get_library_root_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: StorageRootId,
    ) -> Result<Option<LibraryRoot>, VoomError> {
        get_root_in_tx(tx, id).await
    }

    pub async fn list_library_roots(
        &self,
        library_id: Option<LibraryId>,
    ) -> Result<Vec<LibraryRoot>, VoomError> {
        let rows = if let Some(library_id) = library_id {
            sqlx::query(&format!(
                "SELECT {ROOT_COLS} FROM library_roots r WHERE r.library_id = ? \
                 ORDER BY r.created_at, r.id"
            ))
            .bind(i64_from_u64(library_id.0, "libraries.id")?)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(&format!(
                "SELECT {ROOT_COLS} FROM library_roots r ORDER BY r.created_at, r.id"
            ))
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| VoomError::database_context("library_roots list", error))?;
        rows.iter().map(row_to_root).collect()
    }

    pub async fn effective_library_root(
        &self,
        id: StorageRootId,
    ) -> Result<Option<EffectiveLibraryRoot>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {ROOT_COLS}, l.enabled AS library_enabled, n.status AS owner_status, \
                    CASE WHEN r.owner_node_id IS NULL THEN 0 ELSE 1 END AS owner_expected \
             FROM library_roots r \
             JOIN libraries l ON l.id = r.library_id \
             LEFT JOIN nodes n ON n.id = r.owner_node_id \
             WHERE r.id = ?"
        ))
        .bind(root_i64(id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("library_roots availability", error))?;
        row.as_ref().map(row_to_effective).transpose()
    }

    pub async fn update_library_root(
        &self,
        id: StorageRootId,
        update: LibraryRootUpdate,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let current = self
            .get_library_root(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library root {id} not found")))?;
        let updated_defaults = [
            update.default_output_root_id.flatten(),
            update.default_staging_root_id.flatten(),
            update.default_backup_root_id.flatten(),
        ];
        let timestamp = iso8601(now)?;
        let mut tx = begin(&self.pool).await?;
        require_default_ids_in_library(&mut tx, current.library_id, &updated_defaults).await?;
        update_root_settings(&mut tx, id, &update, &timestamp).await?;
        commit(tx).await?;
        self.get_library_root(id).await?.ok_or_else(|| {
            VoomError::Internal(format!("library_roots post-update row vanished: {id}"))
        })
    }

    pub async fn set_library_root_enabled(
        &self,
        id: StorageRootId,
        enabled: bool,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let current = self
            .get_library_root(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library root {id} not found")))?;
        if current.state == StorageRootState::Retired {
            return Err(root_state_conflict(id, current.state, "set enabled"));
        }
        let timestamp = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE library_roots SET enabled = ?, updated_at = ? \
             WHERE id = ? AND state != 'retired'",
        )
        .bind(i64::from(enabled))
        .bind(timestamp)
        .bind(root_i64(id)?)
        .execute(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("library_roots set enabled", error))?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "library root {id} changed concurrently during set enabled"
            )));
        }
        self.get_library_root(id).await?.ok_or_else(|| {
            VoomError::Internal(format!("library_roots post-enable row vanished: {id}"))
        })
    }

    pub async fn assign_library_root_owner_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: StorageRootId,
        owner_node_id: NodeId,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let current = required_root_in_tx(tx, id).await?;
        if current.activation_identity.is_some()
            || !matches!(
                current.state,
                StorageRootState::Unassigned | StorageRootState::Configured
            )
        {
            return Err(root_state_conflict(id, current.state, "assign owner"));
        }
        require_owner_not_retired(tx, owner_node_id).await?;
        let result = sqlx::query(
            "UPDATE library_roots SET owner_node_id = ?, state = 'configured', updated_at = ? \
             WHERE id = ? AND state IN ('unassigned', 'configured') \
               AND activation_identity IS NULL",
        )
        .bind(node_i64(owner_node_id)?)
        .bind(iso8601(now)?)
        .bind(root_i64(id)?)
        .execute(&mut **tx)
        .await
        .map_err(|error| map_root_write_error(id, "assign owner", error))?;
        require_one_row(result.rows_affected(), id, "assign owner")?;
        required_root_in_tx(tx, id).await
    }

    pub async fn activate_library_root_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: StorageRootId,
        activation_identity: String,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        validate_activation_identity(&activation_identity)?;
        let current = required_root_in_tx(tx, id).await?;
        if !matches!(
            current.state,
            StorageRootState::Configured | StorageRootState::Unavailable | StorageRootState::Active
        ) {
            return Err(root_state_conflict(id, current.state, "activate"));
        }
        let owner = current.owner_node_id.ok_or_else(|| {
            VoomError::database(format!("library_roots.owner_node_id missing for root {id}"))
        })?;
        require_owner_active(tx, owner).await?;
        let advance_epoch = current.activation_identity.as_deref() != Some(&activation_identity);
        let result = sqlx::query(
            "UPDATE library_roots SET state = 'active', activation_identity = ?, \
                 root_epoch = root_epoch + ?, updated_at = ? WHERE id = ? AND state = ?",
        )
        .bind(&activation_identity)
        .bind(i64::from(advance_epoch))
        .bind(iso8601(now)?)
        .bind(root_i64(id)?)
        .bind(current.state.as_str())
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("library_roots activate", error))?;
        require_one_row(result.rows_affected(), id, "activate")?;
        required_root_in_tx(tx, id).await
    }

    pub async fn mark_library_root_unavailable_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: StorageRootId,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        transition_state(tx, id, "active", "unavailable", now, "mark unavailable").await
    }

    pub async fn retire_library_root_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: StorageRootId,
        now: OffsetDateTime,
    ) -> Result<LibraryRoot, VoomError> {
        let current = required_root_in_tx(tx, id).await?;
        if current.state == StorageRootState::Retired {
            return Err(root_state_conflict(id, current.state, "retire"));
        }
        let result = sqlx::query(
            "UPDATE library_roots SET state = 'retired', enabled = 0, updated_at = ? \
             WHERE id = ? AND state != 'retired'",
        )
        .bind(iso8601(now)?)
        .bind(root_i64(id)?)
        .execute(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("library_roots retire", error))?;
        require_one_row(result.rows_affected(), id, "retire")?;
        required_root_in_tx(tx, id).await
    }
}

fn validate_activation_identity(value: &str) -> Result<(), VoomError> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(VoomError::Config(
            "storage root activation identity must contain 1..=4096 bytes without NUL".to_owned(),
        ));
    }
    Ok(())
}

async fn insert_root(
    tx: &mut Transaction<'_, Sqlite>,
    input: &NewLibraryRoot,
    timestamp: &str,
    include: &str,
    exclude: &str,
    allowlist: &str,
) -> Result<i64, VoomError> {
    let result = sqlx::query(
        "INSERT INTO library_roots \
         (library_id, owner_node_id, provider_kind, provider_locator, display_locator, state, \
          include_globs, exclude_globs, extension_allowlist, scan_mode, symlink_policy, \
          hidden_file_policy, max_depth, stability_seconds, debounce_seconds, \
          default_output_root_id, default_staging_root_id, default_backup_root_id, enabled, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 'configured', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(i64_from_u64(input.library_id.0, "libraries.id")?)
    .bind(node_i64(input.owner_node_id)?)
    .bind(input.provider_kind.as_str())
    .bind(input.provider_locator.as_str())
    .bind(&input.display_locator)
    .bind(include)
    .bind(exclude)
    .bind(allowlist)
    .bind(input.scan_mode.as_str())
    .bind(input.symlink_policy.as_str())
    .bind(input.hidden_file_policy.as_str())
    .bind(input.max_depth.map(i64::from))
    .bind(i64::from(input.stability_seconds))
    .bind(i64::from(input.debounce_seconds))
    .bind(optional_root_i64(input.default_output_root_id)?)
    .bind(optional_root_i64(input.default_staging_root_id)?)
    .bind(optional_root_i64(input.default_backup_root_id)?)
    .bind(i64::from(input.enabled))
    .bind(timestamp)
    .bind(timestamp)
    .execute(&mut **tx)
    .await
    .map_err(|error| map_root_insert_error(input, error))?;
    Ok(result.last_insert_rowid())
}

async fn transition_state(
    tx: &mut Transaction<'_, Sqlite>,
    id: StorageRootId,
    from: &str,
    to: &str,
    now: OffsetDateTime,
    operation: &str,
) -> Result<LibraryRoot, VoomError> {
    let current = required_root_in_tx(tx, id).await?;
    if current.state.as_str() != from {
        return Err(root_state_conflict(id, current.state, operation));
    }
    let result = sqlx::query(
        "UPDATE library_roots SET state = ?, updated_at = ? WHERE id = ? AND state = ?",
    )
    .bind(to)
    .bind(iso8601(now)?)
    .bind(root_i64(id)?)
    .bind(from)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context(format!("library_roots {operation}"), error))?;
    require_one_row(result.rows_affected(), id, operation)?;
    required_root_in_tx(tx, id).await
}

async fn get_root_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: StorageRootId,
) -> Result<Option<LibraryRoot>, VoomError> {
    let row = sqlx::query(&format!(
        "SELECT {ROOT_COLS} FROM library_roots r WHERE r.id = ?"
    ))
    .bind(root_i64(id)?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("library_roots get in tx", error))?;
    row.as_ref().map(row_to_root).transpose()
}

async fn required_root_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: StorageRootId,
) -> Result<LibraryRoot, VoomError> {
    get_root_in_tx(tx, id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("library root {id} not found")))
}

async fn require_owner_not_retired(
    tx: &mut Transaction<'_, Sqlite>,
    id: NodeId,
) -> Result<NodeStatus, VoomError> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM nodes WHERE id = ?")
        .bind(node_i64(id)?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("storage root owner lookup", error))?;
    let status = status
        .ok_or_else(|| VoomError::NotFound(format!("storage root owner node {id} not found")))?;
    let status = NodeStatus::parse_database("nodes.status", &status)?;
    if status == NodeStatus::Retired {
        return Err(VoomError::Conflict(format!(
            "storage root owner node {id} is retired"
        )));
    }
    Ok(status)
}

async fn require_owner_active(
    tx: &mut Transaction<'_, Sqlite>,
    id: NodeId,
) -> Result<(), VoomError> {
    let status = require_owner_not_retired(tx, id).await?;
    if status != NodeStatus::Active {
        return Err(VoomError::Conflict(format!(
            "storage root owner node {id} is {}, not active",
            status.as_str()
        )));
    }
    Ok(())
}

async fn require_defaults_in_library(
    tx: &mut Transaction<'_, Sqlite>,
    library_id: LibraryId,
    input: &NewLibraryRoot,
) -> Result<(), VoomError> {
    require_default_ids_in_library(
        tx,
        library_id,
        &[
            input.default_output_root_id,
            input.default_staging_root_id,
            input.default_backup_root_id,
        ],
    )
    .await
}

async fn require_default_ids_in_library(
    tx: &mut Transaction<'_, Sqlite>,
    library_id: LibraryId,
    defaults: &[Option<StorageRootId>; 3],
) -> Result<(), VoomError> {
    for root_id in defaults.iter().flatten() {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM library_roots WHERE id = ? AND library_id = ? AND state != 'retired'",
        )
        .bind(root_i64(*root_id)?)
        .bind(i64_from_u64(library_id.0, "libraries.id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("storage root default lookup", error))?;
        if found.is_none() {
            return Err(VoomError::Conflict(format!(
                "default storage root {root_id} is missing, retired, or belongs to another library"
            )));
        }
    }
    Ok(())
}

async fn update_root_settings(
    tx: &mut Transaction<'_, Sqlite>,
    id: StorageRootId,
    update: &LibraryRootUpdate,
    timestamp: &str,
) -> Result<(), VoomError> {
    let include_globs = update
        .include_globs
        .as_ref()
        .map(|value| serialize_json(value, "include_globs"))
        .transpose()?;
    let exclude_globs = update
        .exclude_globs
        .as_ref()
        .map(|value| serialize_json(value, "exclude_globs"))
        .transpose()?;
    let extension_allowlist = update
        .extension_allowlist
        .as_ref()
        .map(|value| serialize_json(value, "extension_allowlist"))
        .transpose()?;
    let result = sqlx::query(
        "UPDATE library_roots SET include_globs = COALESCE(?, include_globs), \
         exclude_globs = COALESCE(?, exclude_globs), \
         extension_allowlist = COALESCE(?, extension_allowlist), \
         scan_mode = COALESCE(?, scan_mode), symlink_policy = COALESCE(?, symlink_policy), \
         hidden_file_policy = COALESCE(?, hidden_file_policy), \
         max_depth = COALESCE(?, max_depth), \
         stability_seconds = COALESCE(?, stability_seconds), \
         debounce_seconds = COALESCE(?, debounce_seconds), \
         default_output_root_id = CASE WHEN ? THEN ? ELSE default_output_root_id END, \
         default_staging_root_id = CASE WHEN ? THEN ? ELSE default_staging_root_id END, \
         default_backup_root_id = CASE WHEN ? THEN ? ELSE default_backup_root_id END, \
         updated_at = ? WHERE id = ? AND state != 'retired'",
    )
    .bind(include_globs)
    .bind(exclude_globs)
    .bind(extension_allowlist)
    .bind(update.scan_mode.map(LibraryScanMode::as_str))
    .bind(update.symlink_policy.map(SymlinkPolicy::as_str))
    .bind(update.hidden_file_policy.map(HiddenFilePolicy::as_str))
    .bind(update.max_depth.map(i64::from))
    .bind(update.stability_seconds.map(i64::from))
    .bind(update.debounce_seconds.map(i64::from))
    .bind(i64::from(update.default_output_root_id.is_some()))
    .bind(optional_root_i64(update.default_output_root_id.flatten())?)
    .bind(i64::from(update.default_staging_root_id.is_some()))
    .bind(optional_root_i64(update.default_staging_root_id.flatten())?)
    .bind(i64::from(update.default_backup_root_id.is_some()))
    .bind(optional_root_i64(update.default_backup_root_id.flatten())?)
    .bind(timestamp)
    .bind(root_i64(id)?)
    .execute(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("library_roots update", error))?;
    require_one_row(result.rows_affected(), id, "update")
}

fn row_to_effective(row: &SqliteRow) -> Result<EffectiveLibraryRoot, VoomError> {
    let root = row_to_root(row)?;
    let library_enabled: i64 = row
        .try_get("library_enabled")
        .map_err(|error| map_row_err("library_roots", error))?;
    let owner_expected: i64 = row
        .try_get("owner_expected")
        .map_err(|error| map_row_err("library_roots", error))?;
    let owner_status: Option<String> = row
        .try_get("owner_status")
        .map_err(|error| map_row_err("library_roots", error))?;
    if owner_expected != 0 && owner_status.is_none() {
        return Err(VoomError::database(format!(
            "library_roots {} references missing owner node",
            root.id
        )));
    }
    let owner_status = owner_status
        .as_deref()
        .map(|value| NodeStatus::parse_database("nodes.status", value))
        .transpose()?;
    let reason = availability_reason(&root, library_enabled != 0, owner_status)?;
    Ok(EffectiveLibraryRoot {
        root,
        available: reason == RootAvailabilityReason::Available,
        reason,
    })
}

fn availability_reason(
    root: &LibraryRoot,
    library_enabled: bool,
    owner_status: Option<NodeStatus>,
) -> Result<RootAvailabilityReason, VoomError> {
    if !library_enabled {
        return Ok(RootAvailabilityReason::LibraryDisabled);
    }
    if !root.enabled {
        return Ok(RootAvailabilityReason::RootDisabled);
    }
    if root.state == StorageRootState::Unassigned {
        return Ok(RootAvailabilityReason::RootUnassigned);
    }
    if root.state != StorageRootState::Active {
        return Ok(RootAvailabilityReason::RootNotActive);
    }
    match owner_status.ok_or_else(|| {
        VoomError::database(format!("active storage root {} has no owner", root.id))
    })? {
        NodeStatus::Active => Ok(RootAvailabilityReason::Available),
        NodeStatus::Registered => Ok(RootAvailabilityReason::OwnerRegistered),
        NodeStatus::Stale => Ok(RootAvailabilityReason::OwnerStale),
        NodeStatus::Retired => Ok(RootAvailabilityReason::OwnerRetired),
    }
}

fn row_to_root(row: &SqliteRow) -> Result<LibraryRoot, VoomError> {
    let t = "library_roots";
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|e| map_row_err(t, e))?;
    let provider_locator: String = row
        .try_get("provider_locator")
        .map_err(|e| map_row_err(t, e))?;
    let state: String = row.try_get("state").map_err(|e| map_row_err(t, e))?;
    let owner_node_id: Option<i64> = row
        .try_get("owner_node_id")
        .map_err(|e| map_row_err(t, e))?;
    let max_depth: Option<i64> = row.try_get("max_depth").map_err(|e| map_row_err(t, e))?;
    let root = LibraryRoot {
        id: StorageRootId(checked_u64(row, "id")?),
        library_id: LibraryId(checked_u64(row, "library_id")?),
        owner_node_id: owner_node_id
            .map(|value| u64_from_i64(value, "library_roots.owner_node_id").map(NodeId))
            .transpose()?,
        provider_kind: StorageProviderKind::parse_database(
            "library_roots.provider_kind",
            &provider_kind,
        )?,
        provider_locator: ProviderLocator::parse_database(
            "library_roots.provider_locator",
            provider_locator,
        )?,
        display_locator: string_column(row, "display_locator")?,
        state: StorageRootState::parse_database("library_roots.state", &state)?,
        root_epoch: checked_u64(row, "root_epoch")?,
        activation_identity: row
            .try_get("activation_identity")
            .map_err(|e| map_row_err(t, e))?,
        include_globs: json_list(row, "include_globs")?,
        exclude_globs: json_list(row, "exclude_globs")?,
        extension_allowlist: json_list(row, "extension_allowlist")?,
        scan_mode: LibraryScanMode::parse(&string_column(row, "scan_mode")?)?,
        symlink_policy: SymlinkPolicy::parse(&string_column(row, "symlink_policy")?)?,
        hidden_file_policy: HiddenFilePolicy::parse(&string_column(row, "hidden_file_policy")?)?,
        max_depth: max_depth.map(u32_from_i64).transpose()?,
        stability_seconds: checked_u32(row, "stability_seconds")?,
        debounce_seconds: checked_u32(row, "debounce_seconds")?,
        default_output_root_id: optional_root_column(row, "default_output_root_id")?,
        default_staging_root_id: optional_root_column(row, "default_staging_root_id")?,
        default_backup_root_id: optional_root_column(row, "default_backup_root_id")?,
        enabled: checked_bool(row, "enabled")?,
        created_at: parse_iso8601(&string_column(row, "created_at")?)?,
        updated_at: parse_iso8601(&string_column(row, "updated_at")?)?,
    };
    validate_persisted_root(&root)?;
    Ok(root)
}

fn validate_persisted_root(root: &LibraryRoot) -> Result<(), VoomError> {
    let lifecycle_is_valid = match root.state {
        StorageRootState::Unassigned => {
            root.owner_node_id.is_none() && root.activation_identity.is_none()
        }
        StorageRootState::Configured => {
            root.owner_node_id.is_some() && root.activation_identity.is_none()
        }
        StorageRootState::Active | StorageRootState::Unavailable => {
            root.owner_node_id.is_some() && root.activation_identity.is_some()
        }
        StorageRootState::Retired => {
            root.owner_node_id.is_some() || root.activation_identity.is_none()
        }
    };
    if !lifecycle_is_valid {
        return Err(VoomError::database(format!(
            "library_roots lifecycle columns invalid for root {} in state {}",
            root.id,
            root.state.as_str()
        )));
    }
    if root
        .activation_identity
        .as_deref()
        .is_some_and(|value| value.is_empty() || value.len() > 4096 || value.contains('\0'))
    {
        return Err(VoomError::database(format!(
            "library_roots.activation_identity invalid for root {}",
            root.id
        )));
    }
    Ok(())
}

fn checked_u64(row: &SqliteRow, column: &'static str) -> Result<u64, VoomError> {
    let value: i64 = row
        .try_get(column)
        .map_err(|error| map_row_err("library_roots", error))?;
    u64_from_i64(value, concat!(module_path!(), ": persisted integer"))
}

fn checked_u32(row: &SqliteRow, column: &'static str) -> Result<u32, VoomError> {
    let value: i64 = row
        .try_get(column)
        .map_err(|error| map_row_err("library_roots", error))?;
    u32_from_i64(value)
}

fn checked_bool(row: &SqliteRow, column: &'static str) -> Result<bool, VoomError> {
    let value: i64 = row
        .try_get(column)
        .map_err(|error| map_row_err("library_roots", error))?;
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(VoomError::database(format!(
            "library_roots.{column} invalid boolean {other}"
        ))),
    }
}

fn string_column(row: &SqliteRow, column: &'static str) -> Result<String, VoomError> {
    row.try_get(column)
        .map_err(|error| map_row_err("library_roots", error))
}

fn optional_root_column(
    row: &SqliteRow,
    column: &'static str,
) -> Result<Option<StorageRootId>, VoomError> {
    let value: Option<i64> = row
        .try_get(column)
        .map_err(|error| map_row_err("library_roots", error))?;
    value
        .map(|value| u64_from_i64(value, "library_roots default root").map(StorageRootId))
        .transpose()
}

fn json_list(row: &SqliteRow, column: &'static str) -> Result<Vec<String>, VoomError> {
    let raw = string_column(row, column)?;
    serde_json::from_str(&raw).map_err(|error| {
        VoomError::database_context(format!("library_roots.{column} decode"), error)
    })
}

fn root_i64(id: StorageRootId) -> Result<i64, VoomError> {
    i64_from_u64(id.0, "library_roots.id")
}

fn optional_root_i64(id: Option<StorageRootId>) -> Result<Option<i64>, VoomError> {
    id.map(root_i64).transpose()
}

fn node_i64(id: NodeId) -> Result<i64, VoomError> {
    i64_from_u64(id.0, "nodes.id")
}

fn map_root_insert_error(input: &NewLibraryRoot, error: sqlx::Error) -> VoomError {
    if is_unique_violation(&error) {
        return VoomError::Conflict(format!(
            "storage root provider locator already exists for owner {}",
            input.owner_node_id
        ));
    }
    if is_foreign_key_violation(&error) {
        return VoomError::NotFound(format!(
            "library {} or referenced storage root not found",
            input.library_id
        ));
    }
    VoomError::database_context("library_roots insert", error)
}

fn map_root_write_error(id: StorageRootId, operation: &str, error: sqlx::Error) -> VoomError {
    if is_unique_violation(&error) {
        return VoomError::Conflict(format!(
            "storage root {id} {operation} conflicts with another owner-scoped root"
        ));
    }
    VoomError::database_context(format!("library_roots {operation}"), error)
}

fn is_foreign_key_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(inner) if inner.is_foreign_key_violation())
}

fn require_one_row(rows: u64, id: StorageRootId, operation: &str) -> Result<(), VoomError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(VoomError::Conflict(format!(
            "storage root {id} changed concurrently during {operation}"
        )))
    }
}

fn root_state_conflict(id: StorageRootId, state: StorageRootState, operation: &str) -> VoomError {
    VoomError::Conflict(format!(
        "storage root {id} cannot {operation} from state {}",
        state.as_str()
    ))
}

#[cfg(test)]
#[path = "library_roots_test.rs"]
mod tests;
