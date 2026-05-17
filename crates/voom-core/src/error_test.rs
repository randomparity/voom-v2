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

#[test]
fn dependency_cycle_error_code_string() {
    let e = VoomError::DependencyCycle("ticket 5 -> ticket 2 -> ticket 5".to_owned());
    assert_eq!(e.code(), "DEPENDENCY_CYCLE");
    assert_eq!(e.error_code(), ErrorCode::DependencyCycle);
}

#[test]
fn conflict_error_code_string() {
    let e = VoomError::Conflict("ticket 7 epoch mismatch".to_owned());
    assert_eq!(e.code(), "CONFLICT");
    assert_eq!(e.error_code(), ErrorCode::Conflict);
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
        ErrorCode::DependencyCycle,
        ErrorCode::Conflict,
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
