use sqlx::migrate::Migrator;

/// Embedded migration set. The single source of truth for "what schema does
/// this binary expect" — both `init()` and `probe_schema()` read from here.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");
