//! Artifact access resolution tests.

use sqlx::SqlitePool;
use tempfile::TempDir;
use voom_core::{
    artifact_access_declaration::{
        ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessTarget,
        ExistingArtifactAccess, FileLocationAccess, PlannedArtifactAccess, StorageRootAccess,
    },
    ids::{ArtifactHandleId, FileLocationId, StorageRootId},
};
use voom_store::test_support;

use super::super::artifact_access_resolution::{
    AccessResolution, AccessResolutionError,
    resolve_artifact_access as resolve_artifact_access_in_tx,
};

/// Pool-level convenience wrapper over the connection-scoped resolution entry point.
async fn resolve_artifact_access(
    pool: &SqlitePool,
    declaration: &ArtifactAccessDeclaration,
) -> Result<AccessResolution, AccessResolutionError> {
    let mut conn = pool.acquire().await.map_err(|e| {
        AccessResolutionError::DatabaseError(format!("failed to acquire connection: {e}"))
    })?;
    resolve_artifact_access_in_tx(&mut conn, declaration).await
}

async fn setup_test_pool() -> (SqlitePool, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let url = test_support::sqlite_url_for(&db_path);

    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();

    (pool, temp_dir)
}

#[tokio::test]
async fn test_resolve_storage_root_active() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed storage root
    test_support::seed_test_storage_root(&pool).await.unwrap();

    // Add node incarnation
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::StorageRoot(StorageRootAccess {
            storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let resolution = resolve_artifact_access(&pool, &declaration).await.unwrap();

    assert_eq!(resolution.resolved_roots.len(), 1);
    assert_eq!(
        resolution.resolved_roots[0].storage_root_id,
        test_support::TEST_STORAGE_ROOT_ID
    );
    assert_eq!(resolution.resolved_roots[0].owner_node_id, 9_000_001);
}

#[tokio::test]
async fn test_resolve_storage_root_not_found() {
    let (pool, _temp_dir) = setup_test_pool().await;

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::StorageRoot(StorageRootAccess {
            storage_root_id: StorageRootId(999_999),
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let result = resolve_artifact_access(&pool, &declaration).await;
    match result {
        Err(AccessResolutionError::StorageRootNotFound { storage_root_id }) => {
            assert_eq!(storage_root_id, StorageRootId(999_999));
        }
        other => panic!("Expected StorageRootNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_file_location_valid() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed both root and location
    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    test_support::seed_test_rooted_location(&pool)
        .await
        .unwrap();

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::FileLocation(FileLocationAccess {
            storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
            file_location_id: test_support::TEST_FILE_LOCATION_ID,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let resolution = resolve_artifact_access(&pool, &declaration).await.unwrap();

    assert_eq!(resolution.resolved_locations.len(), 1);
    assert_eq!(
        resolution.resolved_locations[0].file_location_id,
        test_support::TEST_FILE_LOCATION_ID
    );
    assert_eq!(
        resolution.resolved_locations[0].storage_root_id,
        test_support::TEST_STORAGE_ROOT_ID
    );
}

#[tokio::test]
async fn test_resolve_file_location_not_found() {
    let (pool, _temp_dir) = setup_test_pool().await;

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::FileLocation(FileLocationAccess {
            storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
            file_location_id: FileLocationId(999_999),
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let result = resolve_artifact_access(&pool, &declaration).await;
    match result {
        Err(AccessResolutionError::FileLocationNotFound { file_location_id }) => {
            assert_eq!(file_location_id, FileLocationId(999_999));
        }
        other => panic!("Expected FileLocationNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_file_location_root_mismatch() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed storage root and location
    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    test_support::seed_test_rooted_location(&pool)
        .await
        .unwrap();

    // Declare a location with a mismatched storage root
    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::FileLocation(FileLocationAccess {
            storage_root_id: StorageRootId(9_999_999), // Wrong root
            file_location_id: test_support::TEST_FILE_LOCATION_ID,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let result = resolve_artifact_access(&pool, &declaration).await;
    match result {
        Err(AccessResolutionError::LocationRootInvalid {
            file_location_id,
            storage_root_id,
        }) => {
            assert_eq!(file_location_id, test_support::TEST_FILE_LOCATION_ID);
            assert_eq!(storage_root_id, StorageRootId(9_999_999));
        }
        other => panic!("Expected LocationRootInvalid, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_mixed_owner_roots() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed one root
    test_support::seed_test_storage_root(&pool).await.unwrap();

    // Seed node incarnation for the first owner
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Create a second library
    sqlx::query(
        "INSERT INTO libraries (id, slug, display_name, media_kind, enabled, created_at, updated_at)
         VALUES (2, 'test-library-2', 'Test Library 2', 'unknown', 1, datetime('now'), datetime('now'))"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Create a second node
    sqlx::query(
        "INSERT INTO nodes (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, auth_token_hash, auth_token_hint)
         VALUES (2, 'test-node-2', 'synthetic', 'active', datetime('now'), datetime('now'), 300, 'hash', 'hint')"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Manually create a second root with a different owner
    sqlx::query(
        "INSERT INTO library_roots (id, library_id, owner_node_id, provider_kind, provider_locator, display_locator, state, root_epoch, activation_identity, scan_mode, symlink_policy, hidden_file_policy, stability_seconds, debounce_seconds, enabled, created_at, updated_at)
         VALUES (?, 2, 2, 'local_filesystem', '/different', '/different', 'active', 0, 'activation-2', 'manual_recursive', 'reject', 'ignore', 0, 0, 1, datetime('now'), datetime('now'))"
    )
    .bind(9_000_002_i64)
    .execute(&pool)
    .await
    .unwrap();

    // Seed a node incarnation for the second owner
    sqlx::query(
        "INSERT INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-2', 2, 'active', datetime('now'), datetime('now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let declaration = ArtifactAccessDeclaration::new(vec![
        ArtifactAccessEntry {
            target: ArtifactAccessTarget::StorageRoot(StorageRootAccess {
                storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
            }),
            rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
        },
        ArtifactAccessEntry {
            target: ArtifactAccessTarget::StorageRoot(StorageRootAccess {
                storage_root_id: StorageRootId(9_000_002),
            }),
            rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
        },
    ])
    .unwrap();

    let result = resolve_artifact_access(&pool, &declaration).await;
    match result {
        Err(AccessResolutionError::MixedOwner {
            first_owner,
            conflicting_owner,
        }) => {
            assert_eq!(first_owner, 9_000_001);
            assert_eq!(conflicting_owner, 2);
        }
        other => panic!("Expected MixedOwner, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_existing_artifact() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed storage root and location
    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    test_support::seed_test_rooted_location(&pool)
        .await
        .unwrap();

    // Create an artifact handle
    let artifact_handle_id = ArtifactHandleId(9_000_001);
    sqlx::query(
        "INSERT INTO artifact_handles (id, privacy_class, durability_class, allowed_access_modes, mutability, created_at)
         VALUES (?, 'private', 'temporary', '[]', 'mutable', datetime('now'))"
    )
    .bind(i64::try_from(artifact_handle_id.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::ExistingArtifact(ExistingArtifactAccess {
            artifact_handle_id,
            storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
            file_location_id: test_support::TEST_FILE_LOCATION_ID,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let resolution = resolve_artifact_access(&pool, &declaration).await.unwrap();

    assert_eq!(resolution.resolved_artifacts.len(), 1);
    assert_eq!(
        resolution.resolved_artifacts[0].artifact_handle_id,
        artifact_handle_id
    );
    assert_eq!(
        resolution.resolved_artifacts[0].storage_root_id,
        test_support::TEST_STORAGE_ROOT_ID
    );
    assert_eq!(
        resolution.resolved_artifacts[0].file_location_id,
        Some(test_support::TEST_FILE_LOCATION_ID)
    );
}

#[tokio::test]
async fn test_resolve_planned_artifact() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed storage root
    test_support::seed_test_storage_root(&pool).await.unwrap();

    // Add node incarnation
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::PlannedArtifact(PlannedArtifactAccess {
            artifact_handle_id: ArtifactHandleId(9_000_001),
            target_storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Write],
    }])
    .unwrap();

    let resolution = resolve_artifact_access(&pool, &declaration).await.unwrap();

    assert_eq!(resolution.resolved_artifacts.len(), 1);
    assert_eq!(
        resolution.resolved_artifacts[0].artifact_handle_id,
        ArtifactHandleId(9_000_001)
    );
    assert_eq!(
        resolution.resolved_artifacts[0].storage_root_id,
        test_support::TEST_STORAGE_ROOT_ID
    );
    assert_eq!(resolution.resolved_artifacts[0].file_location_id, None);
}

#[tokio::test]
async fn test_resolve_fake_scanner_location_id() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed storage root but NOT the location - this simulates a fake scanner id
    test_support::seed_test_storage_root(&pool).await.unwrap();

    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Use a fake-scanner location id (9_100_001+ as per voom-fake-support)
    let fake_location_id = FileLocationId(9_100_001);

    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::FileLocation(FileLocationAccess {
            storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
            file_location_id: fake_location_id,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap();

    let result = resolve_artifact_access(&pool, &declaration).await;
    match result {
        Err(AccessResolutionError::FileLocationNotFound { file_location_id }) => {
            assert_eq!(file_location_id, fake_location_id);
        }
        other => panic!("Expected FileLocationNotFound for fake scanner id, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_accepts_valid_epoch_zero() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE library_roots SET root_epoch = 0 WHERE id = ?1")
        .bind(i64::try_from(test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let declaration = root_read_declaration();
    let resolution = resolve_artifact_access(&pool, &declaration).await.unwrap();
    assert_eq!(resolution.resolved_roots[0].root_epoch, 0);
}

#[tokio::test]
async fn test_resolve_rejects_corrupt_root_state_with_stable_evidence() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Simulate a corrupted row that bypassed application-level invariants.
    voom_store::test_support::with_check_constraints_disabled(&pool, |conn| {
        Box::pin(async move {
            sqlx::query("UPDATE library_roots SET state = 'corrupted' WHERE id = ?1")
                .bind(i64::try_from(test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
                .execute(&mut *conn)
                .await
        })
    })
    .await
    .unwrap();

    let declaration = root_read_declaration();
    let result = resolve_artifact_access(&pool, &declaration).await;
    match result {
        Err(AccessResolutionError::InvalidRootState {
            storage_root_id,
            state,
        }) => {
            assert_eq!(storage_root_id, test_support::TEST_STORAGE_ROOT_ID);
            assert_eq!(state, "corrupted");
        }
        other => panic!("Expected InvalidRootState, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_resolutions_are_read_only_and_consistent() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let declaration = root_read_declaration();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let pool = pool.clone();
            let declaration = declaration.clone();
            tokio::spawn(async move { resolve_artifact_access(&pool, &declaration).await.unwrap() })
        })
        .collect();
    for handle in handles {
        let resolution = handle.await.unwrap();
        assert_eq!(resolution.owner_node_id, 9_000_001);
        assert_eq!(resolution.owner_incarnation_id, "inc-9000001");
    }
}

/// A read declaration over the shared test storage root.
fn root_read_declaration() -> ArtifactAccessDeclaration {
    ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::StorageRoot(StorageRootAccess {
            storage_root_id: test_support::TEST_STORAGE_ROOT_ID,
        }),
        rights: vec![voom_core::artifact_access_declaration::ArtifactAccessRight::Read],
    }])
    .unwrap()
}
