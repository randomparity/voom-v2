//! Read-side inspection wrappers over `SqliteBackupRepo` for the CLI
//! `voom backup list|show` command.

use voom_core::{BackupId, VoomError};
use voom_store::repo::backups::{Backup, BackupStatus};

use crate::ControlPlane;

impl ControlPlane {
    /// List backup records, optionally filtered by status, newest first
    /// (`id DESC`), keyset-paginated by `after_id` and bounded by `limit`
    /// (ADR 0031).
    ///
    /// # Errors
    /// Propagates `SqliteBackupRepo` query errors.
    pub async fn list_backups(
        &self,
        status: Option<BackupStatus>,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Backup>, VoomError> {
        self.backups.list(status, after_id, limit).await
    }

    /// Fetch one backup record by id.
    ///
    /// # Errors
    /// Propagates `SqliteBackupRepo::get` errors.
    pub async fn get_backup(&self, id: BackupId) -> Result<Option<Backup>, VoomError> {
        self.backups.get(id).await
    }
}
