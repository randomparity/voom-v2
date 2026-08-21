//! Artifact access resolution tests.

use crate::repo::execution::artifact_access_resolution::{
    AccessResolutionError, resolve_active_incarnation, resolve_file_location, resolve_storage_root,
};
use sqlx::SqlitePool;
use voom_core::ids::{FileLocationId, StorageRootId};

use crate::test_support;
use crate::test_support::fresh_initialized_pool_at;
use voom_test_support::TempDatabase;

async fn setup_test_pool() -> (SqlitePool, TempDatabase) {
    let tmp = TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (pool, tmp)
}

#[tokio::test]
async fn test_resolve_storage_root_active() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed an active storage root
    test_support::seed_test_storage_root(&pool).await.unwrap();

    // Add node incarnation
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let root = resolve_storage_root(&pool, test_support::TEST_STORAGE_ROOT_ID)
        .await
        .unwrap();

    assert_eq!(root.storage_root_id, test_support::TEST_STORAGE_ROOT_ID);
    assert_eq!(root.owner_node_id, 9_000_001);
}

#[tokio::test]
async fn test_resolve_storage_root_not_found() {
    let (pool, _temp_dir) = setup_test_pool().await;

    let result = resolve_storage_root(&pool, StorageRootId(999_999)).await;
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

    let location = resolve_file_location(
        &pool,
        test_support::TEST_FILE_LOCATION_ID,
        test_support::TEST_STORAGE_ROOT_ID,
    )
    .await
    .unwrap();

    assert_eq!(
        location.file_location_id,
        test_support::TEST_FILE_LOCATION_ID
    );
    assert_eq!(location.storage_root_id, test_support::TEST_STORAGE_ROOT_ID);
    assert_eq!(location.owner_node_id, 9_000_001);
}

#[tokio::test]
async fn test_resolve_file_location_not_found() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = resolve_file_location(
        &pool,
        FileLocationId(999_999),
        test_support::TEST_STORAGE_ROOT_ID,
    )
    .await;
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

    // Seed root and location
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

    // Declare that the location is in a different root than it actually is
    let result = resolve_file_location(
        &pool,
        test_support::TEST_FILE_LOCATION_ID,
        StorageRootId(888_888),
    )
    .await;
    match result {
        Err(AccessResolutionError::LocationRootInvalid {
            file_location_id,
            storage_root_id,
        }) => {
            assert_eq!(file_location_id, test_support::TEST_FILE_LOCATION_ID);
            assert_eq!(storage_root_id, StorageRootId(888_888));
        }
        other => panic!("Expected LocationRootInvalid, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_active_incarnation() {
    let (pool, _temp_dir) = setup_test_pool().await;

    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let incarnation = resolve_active_incarnation(&pool, 9_000_001).await.unwrap();
    assert_eq!(incarnation, "inc-9000001");
}

#[tokio::test]
async fn test_resolve_fake_scanner_location_id() {
    let (pool, _temp_dir) = setup_test_pool().await;

    // Seed storage root but not the fake location id
    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at)
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Fake scanner uses ids in the 9_100_001+ range
    let fake_location_id = FileLocationId(9_100_001);

    let result =
        resolve_file_location(&pool, fake_location_id, test_support::TEST_STORAGE_ROOT_ID).await;
    match result {
        Err(AccessResolutionError::FileLocationNotFound { file_location_id }) => {
            assert_eq!(file_location_id, fake_location_id);
        }
        other => panic!("Expected FileLocationNotFound for fake-scanner id, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_storage_root_accepts_epoch_zero() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_storage_root(&pool).await.unwrap();
    sqlx::query("UPDATE library_roots SET root_epoch = 0 WHERE id = ?1")
        .bind(i64::try_from(test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let root = resolve_storage_root(&pool, test_support::TEST_STORAGE_ROOT_ID)
        .await
        .unwrap();
    assert_eq!(root.root_epoch, 0);
}

#[tokio::test]
async fn test_resolve_storage_root_corrupt_state_fails_closed() {
    let (pool, _temp_dir) = setup_test_pool().await;
    test_support::seed_test_storage_root(&pool).await.unwrap();
    // Simulate a corrupted row that bypassed application-level invariants.
    test_support::with_check_constraints_disabled(&pool, |conn| {
        Box::pin(async move {
            sqlx::query("UPDATE library_roots SET state = 'not-a-state' WHERE id = ?1")
                .bind(i64::try_from(test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
                .execute(&mut *conn)
                .await
        })
    })
    .await
    .unwrap();

    let result = resolve_storage_root(&pool, test_support::TEST_STORAGE_ROOT_ID).await;
    match result {
        Err(AccessResolutionError::InvalidRootState {
            storage_root_id,
            state,
        }) => {
            assert_eq!(storage_root_id, test_support::TEST_STORAGE_ROOT_ID);
            assert_eq!(state, "not-a-state");
        }
        other => panic!("Expected InvalidRootState, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolve_file_location_unrooted_address_fails_closed() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_rooted_location(&pool)
        .await
        .unwrap();
    // Retire the untrusted row within what the schema permits: the location is
    // no longer live, so resolution must refuse to satisfy the entry.
    sqlx::query("UPDATE file_locations SET retired_at = '1970-01-01T00:00:00Z' WHERE id = ?1")
        .bind(i64::try_from(test_support::TEST_FILE_LOCATION_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let result = resolve_file_location(
        &pool,
        test_support::TEST_FILE_LOCATION_ID,
        test_support::TEST_STORAGE_ROOT_ID,
    )
    .await;
    match result {
        Err(AccessResolutionError::InvalidLocationState {
            file_location_id,
            state,
        }) => {
            assert_eq!(file_location_id, test_support::TEST_FILE_LOCATION_ID);
            assert_eq!(state, "retired");
        }
        other => panic!("Expected InvalidLocationState for corrupt row, got {other:?}"),
    }
}

#[tokio::test]
async fn test_resolution_composes_inside_a_caller_transaction() {
    let (pool, _temp_dir) = setup_test_pool().await;

    test_support::seed_test_storage_root(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();

    // An uncommitted mutation is visible to resolution inside the same transaction...
    sqlx::query("UPDATE library_roots SET root_epoch = 42 WHERE id = ?1")
        .bind(i64::try_from(test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    let root = resolve_storage_root(&mut *tx, test_support::TEST_STORAGE_ROOT_ID)
        .await
        .unwrap();
    assert_eq!(root.root_epoch, 42);
    tx.rollback().await.unwrap();

    // ...and the rolled-back mutation is invisible outside it.
    let root = resolve_storage_root(&pool, test_support::TEST_STORAGE_ROOT_ID)
        .await
        .unwrap();
    assert_eq!(root.root_epoch, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_resolutions_stay_read_only_and_consistent() {
    let (pool, _temp_dir) = setup_test_pool().await;
    test_support::seed_test_storage_root(&pool).await.unwrap();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move {
                resolve_storage_root(&pool, test_support::TEST_STORAGE_ROOT_ID)
                    .await
                    .unwrap()
            })
        })
        .collect();
    for handle in handles {
        let root = handle.await.unwrap();
        assert_eq!(root.owner_node_id, 9_000_001);
        assert_eq!(root.root_epoch, 1);
    }
}
