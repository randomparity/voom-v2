#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Guard against the most likely future regression: someone adds a new
//! `migrations/000N_*.sql` but forgets to register it in `migrator.rs`'s
//! hand-rolled `vec![Migration::new(...)]`. The sqlx macro used to scan the
//! directory automatically; we replaced that with a manual list to drop the
//! `macros` feature, so this test re-asserts the inventory invariant.

use std::fs;
use std::path::PathBuf;

use voom_store::MIGRATOR;

/// Parse a migrations filename like `0001_init.sql` into its version number.
fn parse_version(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".sql")?;
    let (version_str, _description) = stem.split_once('_')?;
    version_str.parse().ok()
}

#[test]
fn every_migrations_file_is_registered_in_migrator() {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let migrations_dir = workspace_root.join("migrations");

    let mut file_versions: Vec<i64> = fs::read_dir(&migrations_dir)
        .unwrap_or_else(|e| panic!("read_dir({}) failed: {e}", migrations_dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            parse_version(&name)
        })
        .collect();
    file_versions.sort_unstable();

    let mut registered_versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    registered_versions.sort_unstable();

    assert_eq!(
        file_versions, registered_versions,
        "migrations/ directory and MIGRATOR are out of sync — every \
         migrations/000N_*.sql must be registered in voom-store/src/migrator.rs"
    );
    assert!(
        !file_versions.is_empty(),
        "no migrations found — sanity check that the test is reading the right path"
    );
}

#[test]
fn migrator_versions_are_strictly_increasing() {
    let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    let mut sorted = versions.clone();
    sorted.sort_unstable();
    assert_eq!(
        versions, sorted,
        "MIGRATOR must be ordered by ascending version: {versions:?}"
    );
    let dedup_len = {
        let mut d = sorted.clone();
        d.dedup();
        d.len()
    };
    assert_eq!(
        versions.len(),
        dedup_len,
        "MIGRATOR must have unique versions: {versions:?}"
    );
}
