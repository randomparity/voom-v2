use std::error::Error;
use std::sync::{Arc, Condvar, Mutex};

use time::OffsetDateTime;
use voom_core::{NodeKind, ProviderLocator, ScanSessionId};

use super::super::libraries::{LibraryMediaKind, NewLibrary};
use super::*;
use crate::test_support::with_check_constraints_disabled;

async fn repo() -> (SqliteLibraryRepo, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (SqliteLibraryRepo::new(pool), tmp)
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).unwrap()
}

async fn library(repo: &SqliteLibraryRepo, slug: &str, enabled: bool) -> LibraryId {
    repo.create_library(
        NewLibrary {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled,
        },
        at(0),
    )
    .await
    .unwrap()
    .id
}

async fn node(repo: &SqliteLibraryRepo, name: &str, status: NodeStatus) -> NodeId {
    let id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, retired_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (?, ?, ?, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 CASE WHEN ? = 'retired' THEN '1970-01-01T00:00:00Z' END, \
                 60, 'hash', 'hint', '{}')",
    )
    .bind(name)
    .bind(NodeKind::Local.as_str())
    .bind(status.as_str())
    .bind(status.as_str())
    .execute(&repo.pool)
    .await
    .unwrap()
    .last_insert_rowid();
    NodeId(u64::try_from(id).unwrap())
}

fn new_root(library_id: LibraryId, owner_node_id: NodeId, locator: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(locator.to_owned()).unwrap(),
        display_locator: locator.to_owned(),
        include_globs: vec!["**/*.mkv".to_owned()],
        exclude_globs: vec!["**/sample/**".to_owned()],
        extension_allowlist: vec!["mkv".to_owned(), "mp4".to_owned()],
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: Some(4),
        stability_seconds: 30,
        debounce_seconds: 5,
        default_output_root_id: None,
        default_staging_root_id: None,
        default_backup_root_id: None,
        enabled: true,
    }
}

#[tokio::test]
async fn library_root_decodes_null_scan_provenance_as_a_typed_optional_id() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "scan-provenance", true).await;
    let owner = node(&repo, "scan-provenance-owner", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/scan-provenance"), at(1))
        .await
        .unwrap();

    assert_eq!(root.last_scan_session_id, None::<ScanSessionId>);
}

struct RollbackBarrier {
    entered: tokio::sync::Notify,
    release: (Mutex<bool>, Condvar),
}

impl RollbackBarrier {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: (Mutex::new(false), Condvar::new()),
        })
    }

    fn callback(self: &Arc<Self>) -> impl FnMut() + Send + 'static {
        let barrier = Arc::clone(self);
        move || {
            barrier.entered.notify_one();
            let (lock, wake) = &barrier.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }
    }

    fn rearm(&self) {
        *self.release.0.lock().unwrap() = false;
    }

    fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_one();
    }

    async fn wait_until_entered(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.entered.notified())
            .await
            .expect("transaction did not reach rollback hook");
    }
}

async fn isolate_rollback_connection(
    repo: &SqliteLibraryRepo,
) -> (
    Vec<sqlx::pool::PoolConnection<sqlx::Sqlite>>,
    Arc<RollbackBarrier>,
) {
    let mut held_connections = Vec::new();
    for _ in 0..repo.pool.options().get_max_connections() {
        held_connections.push(repo.pool.acquire().await.unwrap());
    }
    let mut rollback_connection = held_connections.pop().unwrap();
    let barrier = RollbackBarrier::new();
    rollback_connection
        .lock_handle()
        .await
        .unwrap()
        .set_rollback_hook(barrier.callback());
    rollback_connection.return_to_pool().await;
    (held_connections, barrier)
}

#[tokio::test]
async fn create_then_get_round_trips_typed_owner_provider_and_state() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Registered).await;
    let created = repo
        .create_library_root(new_root(library_id, owner, "/media/films"), at(1))
        .await
        .unwrap();
    assert_eq!(
        created,
        repo.get_library_root(created.id).await.unwrap().unwrap()
    );
    assert_eq!(created.owner_node_id, Some(owner));
    assert_eq!(created.state, StorageRootState::Configured);
    assert_eq!(created.root_epoch, 0);
    assert_eq!(created.provider_locator.as_str(), "/media/films");
}

#[tokio::test]
async fn create_rejects_missing_and_retired_owners_before_insert() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let initial_count = repo.list_library_roots(None).await.unwrap().len();
    let missing = repo
        .create_library_root(new_root(library_id, NodeId(999), "/missing"), at(1))
        .await
        .unwrap_err();
    assert!(matches!(missing, VoomError::NotFound(_)));

    let retired = node(&repo, "retired", NodeStatus::Retired).await;
    let error = repo
        .create_library_root(new_root(library_id, retired, "/retired"), at(1))
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Conflict(_)));
    assert_eq!(
        repo.list_library_roots(None).await.unwrap().len(),
        initial_count
    );
}

#[tokio::test]
async fn provider_locator_is_unique_only_within_one_owner() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner_a = node(&repo, "node-a", NodeStatus::Registered).await;
    let owner_b = node(&repo, "node-b", NodeStatus::Registered).await;
    repo.create_library_root(new_root(library_id, owner_a, "/media"), at(1))
        .await
        .unwrap();
    let duplicate = repo
        .create_library_root(new_root(library_id, owner_a, "/media"), at(2))
        .await
        .unwrap_err();
    assert!(matches!(duplicate, VoomError::Conflict(_)));
    repo.create_library_root(new_root(library_id, owner_b, "/media"), at(3))
        .await
        .unwrap();
}

#[tokio::test]
async fn standalone_root_writes_await_rollback_before_returning_errors() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Registered).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let (mut held_connections, rollback) = isolate_rollback_connection(&repo).await;

    let create_repo = repo.clone();
    let create_task = tokio::spawn(async move {
        create_repo
            .create_library_root(new_root(library_id, owner, "/media"), at(2))
            .await
    });
    rollback.wait_until_entered().await;
    tokio::task::yield_now().await;
    let create_returned_before_rollback = create_task.is_finished();
    rollback.release();
    let create_error = create_task.await.unwrap().unwrap_err();
    assert!(matches!(create_error, VoomError::Conflict(_)));
    assert!(
        !create_returned_before_rollback,
        "create returned before its failed transaction released SQLite writer ownership"
    );

    sqlx::query(
        "CREATE TRIGGER reject_root_update BEFORE UPDATE ON library_roots \
         BEGIN SELECT RAISE(FAIL, 'forced root update failure'); END",
    )
    .execute(&mut *held_connections[0])
    .await
    .unwrap();
    rollback.rearm();
    let update_repo = repo.clone();
    let update_task = tokio::spawn(async move {
        update_repo
            .update_library_root(
                root.id,
                LibraryRootUpdate {
                    debounce_seconds: Some(10),
                    ..LibraryRootUpdate::default()
                },
                at(3),
            )
            .await
    });
    rollback.wait_until_entered().await;
    tokio::task::yield_now().await;
    let update_returned_before_rollback = update_task.is_finished();
    rollback.release();
    let update_error = update_task.await.unwrap().unwrap_err();
    assert!(matches!(update_error, VoomError::Database { .. }));
    assert!(
        !update_returned_before_rollback,
        "update returned before its failed transaction released SQLite writer ownership"
    );

    drop(held_connections);
}

#[tokio::test]
async fn activation_requires_active_owner_and_fences_changed_identity() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Registered).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let mut validation_tx = begin(&repo.pool).await.unwrap();
    for invalid in [String::new(), "x".repeat(4097), "nul\0identity".to_owned()] {
        let error = repo
            .activate_library_root_in_tx(&mut validation_tx, root.id, invalid, at(2))
            .await
            .unwrap_err();
        assert!(matches!(error, VoomError::Config(_)));
    }
    drop(validation_tx);

    let mut tx = begin(&repo.pool).await.unwrap();
    let inactive = repo
        .activate_library_root_in_tx(&mut tx, root.id, "device:1".to_owned(), at(2))
        .await
        .unwrap_err();
    assert!(matches!(inactive, VoomError::Conflict(_)));
    drop(tx);

    sqlx::query("UPDATE nodes SET status = 'active' WHERE id = ?")
        .bind(i64::try_from(owner.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();
    let mut tx = begin(&repo.pool).await.unwrap();
    let active = repo
        .activate_library_root_in_tx(&mut tx, root.id, "device:1".to_owned(), at(3))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(
        (active.state, active.root_epoch),
        (StorageRootState::Active, 1)
    );

    let mut tx = begin(&repo.pool).await.unwrap();
    repo.mark_library_root_unavailable_in_tx(&mut tx, root.id, at(4))
        .await
        .unwrap();
    let unchanged = repo
        .activate_library_root_in_tx(&mut tx, root.id, "device:1".to_owned(), at(5))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(unchanged.root_epoch, 1);

    let mut tx = begin(&repo.pool).await.unwrap();
    repo.mark_library_root_unavailable_in_tx(&mut tx, root.id, at(6))
        .await
        .unwrap();
    let changed = repo
        .activate_library_root_in_tx(&mut tx, root.id, "device:2".to_owned(), at(7))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(changed.root_epoch, 2);
}

#[tokio::test]
async fn effective_availability_fails_closed_at_each_gate() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    assert_reason(&repo, root.id, RootAvailabilityReason::RootNotActive).await;

    let mut tx = begin(&repo.pool).await.unwrap();
    repo.activate_library_root_in_tx(&mut tx, root.id, "device:1".to_owned(), at(2))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_reason(&repo, root.id, RootAvailabilityReason::Available).await;

    repo.set_library_root_enabled(root.id, false, at(3))
        .await
        .unwrap();
    assert_reason(&repo, root.id, RootAvailabilityReason::RootDisabled).await;
    repo.set_library_root_enabled(root.id, true, at(4))
        .await
        .unwrap();
    repo.set_library_enabled(library_id, false, at(5))
        .await
        .unwrap();
    assert_reason(&repo, root.id, RootAvailabilityReason::LibraryDisabled).await;
    repo.set_library_enabled(library_id, true, at(6))
        .await
        .unwrap();

    for (status, reason) in [
        (
            NodeStatus::Registered,
            RootAvailabilityReason::OwnerRegistered,
        ),
        (NodeStatus::Stale, RootAvailabilityReason::OwnerStale),
        (NodeStatus::Retired, RootAvailabilityReason::OwnerRetired),
    ] {
        sqlx::query(
            "UPDATE nodes SET status = ?, \
             retired_at = CASE WHEN ? = 'retired' THEN '1970-01-01T00:00:00Z' END WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(status.as_str())
        .bind(i64::try_from(owner.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();
        assert_reason(&repo, root.id, reason).await;
    }
}

#[tokio::test]
async fn partial_root_updates_preserve_unrelated_settings_and_can_clear_defaults() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let output = repo
        .create_library_root(new_root(library_id, owner, "/output"), at(2))
        .await
        .unwrap();

    repo.update_library_root(
        root.id,
        LibraryRootUpdate {
            include_globs: Some(vec!["**/*.mov".to_owned()]),
            default_output_root_id: Some(Some(output.id)),
            ..LibraryRootUpdate::default()
        },
        at(3),
    )
    .await
    .unwrap();
    let updated = repo
        .update_library_root(
            root.id,
            LibraryRootUpdate {
                debounce_seconds: Some(45),
                ..LibraryRootUpdate::default()
            },
            at(4),
        )
        .await
        .unwrap();
    assert_eq!(updated.include_globs, ["**/*.mov"]);
    assert_eq!(updated.debounce_seconds, 45);
    assert_eq!(updated.default_output_root_id, Some(output.id));

    let cleared = repo
        .update_library_root(
            root.id,
            LibraryRootUpdate {
                default_output_root_id: Some(None),
                ..LibraryRootUpdate::default()
            },
            at(5),
        )
        .await
        .unwrap();
    assert_eq!(cleared.include_globs, ["**/*.mov"]);
    assert_eq!(cleared.debounce_seconds, 45);
    assert_eq!(cleared.default_output_root_id, None);
}

#[tokio::test]
async fn set_root_enabled_distinguishes_missing_from_retired() {
    let (repo, _tmp) = repo().await;
    let missing = repo
        .set_library_root_enabled(StorageRootId(42), false, at(1))
        .await
        .unwrap_err();
    assert!(matches!(missing, VoomError::NotFound(_)));

    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(2))
        .await
        .unwrap();
    let mut tx = begin(&repo.pool).await.unwrap();
    repo.retire_library_root_in_tx(&mut tx, root.id, at(3))
        .await
        .unwrap();
    commit(tx).await.unwrap();

    let retired = repo
        .set_library_root_enabled(root.id, true, at(4))
        .await
        .unwrap_err();
    assert!(matches!(retired, VoomError::Conflict(_)));
}

async fn assert_reason(
    repo: &SqliteLibraryRepo,
    id: StorageRootId,
    expected: RootAvailabilityReason,
) {
    let availability = repo.effective_library_root(id).await.unwrap().unwrap();
    assert_eq!(availability.reason, expected);
    assert_eq!(
        availability.available,
        expected == RootAvailabilityReason::Available
    );
}

#[tokio::test]
async fn retire_is_terminal_and_library_delete_is_restricted() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let mut tx = begin(&repo.pool).await.unwrap();
    let retired = repo
        .retire_library_root_in_tx(&mut tx, root.id, at(2))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(retired.state, StorageRootState::Retired);
    assert!(!retired.enabled);

    let mut tx = begin(&repo.pool).await.unwrap();
    let error = repo
        .retire_library_root_in_tx(&mut tx, root.id, at(3))
        .await
        .unwrap_err();
    assert!(matches!(error, VoomError::Conflict(_)));
    drop(tx);
    assert!(matches!(
        repo.delete_library(library_id).await.unwrap_err(),
        VoomError::Conflict(message)
            if message.contains("durable storage roots")
    ));
}

#[tokio::test]
async fn corrupt_persisted_root_data_is_a_database_error() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    with_check_constraints_disabled(&repo.pool, move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE library_roots SET provider_locator = '' WHERE id = ?")
                .bind(i64::try_from(root.id.0).unwrap())
                .execute(&mut *connection)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let error = repo.get_library_root(root.id).await.unwrap_err();
    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert!(error.source().is_none());
}

#[tokio::test]
async fn effective_root_with_missing_library_is_a_database_error() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let mut connection = repo.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM libraries WHERE id = ?")
        .bind(i64::try_from(library_id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    let error = repo.effective_library_root(root.id).await.unwrap_err();
    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert!(error.to_string().contains("missing library"));
}

#[tokio::test]
async fn effective_root_with_corrupt_library_enabled_is_a_database_error() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    with_check_constraints_disabled(&repo.pool, move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE libraries SET enabled = 2 WHERE id = ?")
                .bind(i64::try_from(library_id.0).unwrap())
                .execute(&mut *connection)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let error = repo.effective_library_root(root.id).await.unwrap_err();
    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert!(error.to_string().contains("libraries.enabled"));
}

#[tokio::test]
async fn corrupt_persisted_root_lifecycle_is_a_database_error_before_classification() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let mut tx = begin(&repo.pool).await.unwrap();
    repo.activate_library_root_in_tx(&mut tx, root.id, "device:media".to_owned(), at(2))
        .await
        .unwrap();
    commit(tx).await.unwrap();

    with_check_constraints_disabled(&repo.pool, move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE library_roots SET activation_identity = NULL WHERE id = ?")
                .bind(i64::try_from(root.id.0).unwrap())
                .execute(&mut *connection)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let error = repo.effective_library_root(root.id).await.unwrap_err();
    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert!(error.to_string().contains("lifecycle columns invalid"));
}
