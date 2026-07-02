//! Read-side inspection wrappers over `SqliteBackupRepo` for the CLI
//! `voom backup list|show` command.

use voom_core::{BackupId, VoomError};
use voom_store::repo::backups::{Backup, BackupStatus};

use crate::ControlPlane;

impl ControlPlane {
    /// List backup records, optionally filtered by status, newest-committed
    /// last (`created_at ASC, id ASC`), bounded by `limit`.
    ///
    /// # Errors
    /// Propagates `SqliteBackupRepo` query errors.
    pub async fn list_backups(
        &self,
        status: Option<BackupStatus>,
        limit: u32,
    ) -> Result<Vec<Backup>, VoomError> {
        match status {
            Some(status) => self.backups.list_by_status(status, limit).await,
            None => self.backups.list(limit).await,
        }
    }

    /// Fetch one backup record by id.
    ///
    /// # Errors
    /// Propagates `SqliteBackupRepo::get` errors.
    pub async fn get_backup(&self, id: BackupId) -> Result<Option<Backup>, VoomError> {
        self.backups.get(id).await
    }
}
