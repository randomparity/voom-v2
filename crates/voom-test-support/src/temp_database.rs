use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// On-disk `SQLite` test database whose directory owns every `SQLite` sidecar.
///
/// Dropping this fixture removes the entire private directory, including WAL,
/// shared-memory, rollback-journal, and super-journal files `SQLite` may create.
#[derive(Debug)]
pub struct TempDatabase {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl TempDatabase {
    /// Create a database path in a new system temporary directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the temporary directory cannot be created.
    pub fn new() -> io::Result<Self> {
        Self::from_directory(tempfile::tempdir()?)
    }

    /// Create a database path in a new temporary directory below `parent`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the temporary directory cannot be created.
    pub fn new_in(parent: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_directory(tempfile::tempdir_in(parent)?)
    }

    /// Return the path reserved for the `SQLite` database.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn from_directory(directory: tempfile::TempDir) -> io::Result<Self> {
        let path = directory.path().join("voom.db");
        File::create(&path)?;
        Ok(Self {
            _directory: directory,
            path,
        })
    }
}

#[cfg(test)]
#[path = "temp_database_test.rs"]
mod tests;
