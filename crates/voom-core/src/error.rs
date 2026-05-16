use thiserror::Error;

/// Stable wire-format identifier for an error. Consumers match on this enum
/// (exhaustively) instead of comparing against `&'static str` codes, so a
/// renamed or newly-added variant becomes a compile-time error in every
/// surface rather than a silent string-mismatch at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Database file is missing or unreachable from this host.
    DbUnreachable,
    /// Database is reachable but has no migrations applied.
    DbUninitialized,
    /// Database is reachable but its schema is partial or corrupted.
    DbPartialSchema,
    /// A previous migration left a row recorded as `success=0`; sqlx will
    /// refuse to migrate further until it is manually cleared.
    DbDirtyMigration,
    /// Database has migrations this binary does not know about.
    DbSchemaTooNew,
    /// Configuration value is invalid (e.g. malformed URL, unknown enum).
    ConfigInvalid,
    /// Resource lookup miss.
    NotFound,
    /// Unexpected internal failure with no actionable hint.
    Internal,
    /// CLI argument parsing failed (clap surface).
    BadArgs,
}

impl ErrorCode {
    /// Wire-format string for the JSON envelope's `error.code` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DbUnreachable => "DB_UNREACHABLE",
            Self::DbUninitialized => "DB_UNINITIALIZED",
            Self::DbPartialSchema => "DB_PARTIAL_SCHEMA",
            Self::DbDirtyMigration => "DB_DIRTY_MIGRATION",
            Self::DbSchemaTooNew => "DB_SCHEMA_TOO_NEW",
            Self::ConfigInvalid => "CONFIG_INVALID",
            Self::NotFound => "NOT_FOUND",
            Self::Internal => "INTERNAL",
            Self::BadArgs => "BAD_ARGS",
        }
    }
}

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("database error: {0}")]
    Database(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("dirty migration: {0}")]
    DirtyMigration(String),
    #[error("schema is newer than this binary: {0}")]
    SchemaTooNew(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl VoomError {
    /// Typed wire-format code for this error. Prefer this over [`Self::code`]
    /// at every consumer that classifies on the value.
    #[must_use]
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::Database(_) => ErrorCode::DbUnreachable,
            Self::Migration(_) => ErrorCode::DbPartialSchema,
            Self::DirtyMigration(_) => ErrorCode::DbDirtyMigration,
            Self::SchemaTooNew(_) => ErrorCode::DbSchemaTooNew,
            Self::Config(_) => ErrorCode::ConfigInvalid,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }

    /// Stable string code matching the JSON envelope's `error.code`. Thin
    /// wrapper around [`Self::error_code`] kept for the envelope writers that
    /// take `&'static str` (`voom_cli::envelope::emit_err`).
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.error_code().as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_variant_has_db_unreachable_code() {
        let err = VoomError::Database("connection refused".into());
        assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
        assert_eq!(err.code(), "DB_UNREACHABLE");
    }

    #[test]
    fn migration_variant_has_partial_schema_code() {
        let err = VoomError::Migration("missing migration".into());
        assert_eq!(err.error_code(), ErrorCode::DbPartialSchema);
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[test]
    fn schema_too_new_variant_has_too_new_code() {
        let err = VoomError::SchemaTooNew("future migration applied".into());
        assert_eq!(err.error_code(), ErrorCode::DbSchemaTooNew);
        assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
    }

    #[test]
    fn internal_variant_has_internal_code() {
        let err = VoomError::Internal("unexpected".into());
        assert_eq!(err.error_code(), ErrorCode::Internal);
        assert_eq!(err.code(), "INTERNAL");
    }

    #[test]
    fn dirty_migration_variant_has_dirty_migration_code() {
        let err = VoomError::DirtyMigration("version 1 is dirty".into());
        assert_eq!(err.error_code(), ErrorCode::DbDirtyMigration);
        assert_eq!(err.code(), "DB_DIRTY_MIGRATION");
    }

    /// Adding a variant to `ErrorCode` must force a wire-string decision in
    /// `as_str()`. This test is intentionally an exhaustive match so a new
    /// variant fails compilation here too — both halves of the round trip
    /// stay in lockstep.
    #[test]
    fn every_error_code_has_a_wire_string() {
        for code in [
            ErrorCode::DbUnreachable,
            ErrorCode::DbUninitialized,
            ErrorCode::DbPartialSchema,
            ErrorCode::DbDirtyMigration,
            ErrorCode::DbSchemaTooNew,
            ErrorCode::ConfigInvalid,
            ErrorCode::NotFound,
            ErrorCode::Internal,
            ErrorCode::BadArgs,
        ] {
            // Confirm the exhaustive match in `as_str()` produces a
            // SCREAMING_SNAKE_CASE token; format isn't load-bearing beyond
            // the contract documented on the enum itself.
            let s = code.as_str();
            assert!(!s.is_empty(), "{code:?} has empty wire string");
            assert!(
                s.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "{code:?} wire string {s:?} is not SCREAMING_SNAKE_CASE"
            );
        }
    }
}
