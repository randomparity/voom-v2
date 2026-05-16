use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoomError {
    #[error("database error: {0}")]
    Database(String),
    #[error("migration error: {0}")]
    Migration(String),
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
    /// Stable string code matching the JSON envelope's `error.code`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "DB_UNREACHABLE",
            Self::Migration(_) => "DB_PARTIAL_SCHEMA",
            Self::SchemaTooNew(_) => "DB_SCHEMA_TOO_NEW",
            Self::Config(_) => "CONFIG_INVALID",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Internal(_) => "INTERNAL",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_variant_has_db_unreachable_code() {
        let err = VoomError::Database("connection refused".into());
        assert_eq!(err.code(), "DB_UNREACHABLE");
    }

    #[test]
    fn migration_variant_has_partial_schema_code() {
        let err = VoomError::Migration("missing migration".into());
        assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
    }

    #[test]
    fn schema_too_new_variant_has_too_new_code() {
        let err = VoomError::SchemaTooNew("future migration applied".into());
        assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
    }

    #[test]
    fn internal_variant_has_internal_code() {
        let err = VoomError::Internal("unexpected".into());
        assert_eq!(err.code(), "INTERNAL");
    }
}
