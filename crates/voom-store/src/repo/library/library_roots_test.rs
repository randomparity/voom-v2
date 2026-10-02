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

#[tokio::test]
async fn library_root_decodes_scan_provenance_as_the_exact_typed_session_id() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "scan-provenance-present", true).await;
    let owner = node(&repo, "scan-provenance-present-owner", NodeStatus::Active).await;
    let root = repo
        .create_library_root(
            new_root(library_id, owner, "/scan-provenance-present"),
            at(1),
        )
        .await
        .unwrap();
    let session_id = sqlx::query(
        "INSERT INTO scan_sessions (storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at) VALUES (?, ?, ?, 'requested', \
         300, '1970-01-01T00:05:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(root.id.0).unwrap())
    .bind(i64::try_from(root.root_epoch).unwrap())
    .bind(i64::try_from(owner.0).unwrap())
    .execute(&repo.pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query("UPDATE library_roots SET last_scan_session_id = ? WHERE id = ?")
        .bind(session_id)
        .bind(i64::try_from(root.id.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();

    let decoded = repo.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(
        decoded.last_scan_session_id,
        Some(ScanSessionId(u64::try_from(session_id).unwrap()))
    );
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
    let mut validation_tx =
        crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
            .await
            .unwrap();
    for invalid in [String::new(), "x".repeat(4097), "nul\0identity".to_owned()] {
        let error = repo
            .activate_library_root_in_tx(&mut validation_tx, root.id, invalid, at(2))
            .await
            .unwrap_err();
        assert!(matches!(error, VoomError::Config(_)));
    }
    drop(validation_tx);

    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
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
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
    let active = repo
        .activate_library_root_in_tx(&mut tx, root.id, "device:1".to_owned(), at(3))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(
        (active.state, active.root_epoch),
        (StorageRootState::Active, 1)
    );

    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
    repo.mark_library_root_unavailable_in_tx(&mut tx, root.id, at(4))
        .await
        .unwrap();
    let unchanged = repo
        .activate_library_root_in_tx(&mut tx, root.id, "device:1".to_owned(), at(5))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(unchanged.root_epoch, 1);

    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
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

    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
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

// A zero busy timeout distinguishes the opener from the later UPDATE without
// depending on when another task gets scheduled (ADR 0083).
#[tokio::test]
async fn update_root_contends_at_transaction_open() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let mut connections = Vec::new();
    for _ in 0..repo.pool.options().get_max_connections() {
        let mut connection = repo.pool.acquire().await.unwrap();
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&mut *connection)
            .await
            .unwrap();
        connections.push(connection);
    }
    for mut connection in connections {
        connection.return_to_pool().await;
    }
    let holder = repo.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let update = LibraryRootUpdate {
        default_output_root_id: Some(Some(root.id)),
        debounce_seconds: Some(45),
        ..LibraryRootUpdate::default()
    };

    let result = repo
        .update_library_root(root.id, update.clone(), at(2))
        .await;
    holder.rollback().await.unwrap();
    let error = result.unwrap_err();
    assert!(
        matches!(&error, VoomError::Database { .. }),
        "writer contention must remain a database error: {error}"
    );
    assert!(
        error
            .to_string()
            .contains("library_roots: update_library_root"),
        "the update must contend at its opener, not upgrade a read snapshot: {error}"
    );
    assert_eq!(
        repo.get_library_root(root.id).await.unwrap(),
        Some(root.clone())
    );

    let updated = repo
        .update_library_root(root.id, update, at(2))
        .await
        .unwrap();
    assert_eq!(updated.default_output_root_id, Some(root.id));
    assert_eq!(updated.debounce_seconds, 45);
    assert_eq!(repo.get_library_root(root.id).await.unwrap(), Some(updated));
}

#[tokio::test]
async fn update_root_waits_out_a_writer() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let root = repo
        .create_library_root(new_root(library_id, owner, "/media"), at(1))
        .await
        .unwrap();
    let holder = repo.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let update = repo.update_library_root(
        root.id,
        LibraryRootUpdate {
            default_output_root_id: Some(Some(root.id)),
            debounce_seconds: Some(45),
            ..LibraryRootUpdate::default()
        },
        at(2),
    );
    tokio::pin!(update);

    // Retain the future across the bounded pending observation. This arm cannot
    // prove it reached SQLite before release; the zero-timeout case proves the
    // serialization point independently of scheduling.
    let pending = tokio::time::timeout(std::time::Duration::from_millis(200), &mut update).await;
    holder.rollback().await.unwrap();
    assert!(
        pending.is_err(),
        "update returned while the writer held its lock: {pending:?}"
    );
    let updated = tokio::time::timeout(std::time::Duration::from_secs(5), update)
        .await
        .expect("update did not finish after the competing writer released its lock")
        .expect("update must wait for the writer instead of failing a lock upgrade");
    assert_eq!(updated.default_output_root_id, Some(root.id));
    assert_eq!(updated.debounce_seconds, 45);
    assert_eq!(repo.get_library_root(root.id).await.unwrap(), Some(updated));
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
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
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
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
    let retired = repo
        .retire_library_root_in_tx(&mut tx, root.id, at(2))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(retired.state, StorageRootState::Retired);
    assert!(!retired.enabled);

    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
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

async fn retire(
    repo: &SqliteLibraryRepo,
    id: StorageRootId,
    now: OffsetDateTime,
) -> Result<LibraryRoot, VoomError> {
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: retire")
        .await
        .unwrap();
    let result = repo.retire_library_root_in_tx(&mut tx, id, now).await;
    match &result {
        Ok(_) => commit(tx).await.unwrap(),
        Err(_) => rollback(tx).await.unwrap(),
    }
    result
}

/// Set all three default columns at once; `None` clears a column.
async fn set_defaults(
    repo: &SqliteLibraryRepo,
    id: StorageRootId,
    [staging, output, backup]: [Option<StorageRootId>; 3],
    now: OffsetDateTime,
) {
    let update = LibraryRootUpdate {
        default_staging_root_id: Some(staging),
        default_output_root_id: Some(output),
        default_backup_root_id: Some(backup),
        ..LibraryRootUpdate::default()
    };
    repo.update_library_root(id, update, now).await.unwrap();
}

// ADR 0097: a staging default contains pre-promotion artifacts, so retiring it while a
// live root still resolves to it would strand them (#626).
#[tokio::test]
async fn retire_refuses_a_root_other_live_roots_name_as_a_default() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let create = |locator: &'static str, seconds| {
        repo.create_library_root(new_root(library_id, owner, locator), at(seconds))
    };
    let target = create("/staging", 1).await.unwrap();
    let staging_user = create("/media", 2).await.unwrap();
    let output_user = create("/other", 3).await.unwrap();
    let retired_user = create("/old", 4).await.unwrap();
    let t = Some(target.id);
    set_defaults(&repo, staging_user.id, [t, None, None], at(5)).await;
    set_defaults(&repo, output_user.id, [None, t, t], at(6)).await;
    set_defaults(&repo, retired_user.id, [None, t, None], at(7)).await;
    retire(&repo, retired_user.id, at(8)).await.unwrap();
    set_defaults(&repo, target.id, [t, None, None], at(9)).await;

    let error = retire(&repo, target.id, at(10)).await.unwrap_err();
    let expected = format!(
        "storage root {} cannot retire while other roots name it as a default: \
         root {} (default_staging_root_id), \
         root {} (default_output_root_id, default_backup_root_id); \
         repoint those defaults or retire the referencing roots first",
        target.id, staging_user.id, output_user.id
    );
    assert!(
        matches!(&error, VoomError::Conflict(message) if *message == expected),
        "{error:?}"
    );
    let unchanged = repo.get_library_root(target.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, StorageRootState::Configured);
    assert!(unchanged.enabled);

    let own = Some(staging_user.id);
    set_defaults(&repo, staging_user.id, [own, None, None], at(11)).await;
    set_defaults(&repo, output_user.id, [None, None, None], at(12)).await;
    let retired = retire(&repo, target.id, at(13)).await.unwrap();
    assert_eq!(retired.state, StorageRootState::Retired);
}

// Validation loss is an observed fact (ADR 0055) and reversible by reactivation, so a
// referenced root must still be able to record it (#626).
#[tokio::test]
async fn mark_unavailable_is_not_refused_for_a_referenced_default() {
    let (repo, _tmp) = repo().await;
    let library_id = library(&repo, "films", true).await;
    let owner = node(&repo, "node-a", NodeStatus::Active).await;
    let target = repo
        .create_library_root(new_root(library_id, owner, "/staging"), at(1))
        .await
        .unwrap();
    let user = repo
        .create_library_root(
            NewLibraryRoot {
                default_staging_root_id: Some(target.id),
                ..new_root(library_id, owner, "/media")
            },
            at(2),
        )
        .await
        .unwrap();
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: mark unavailable")
        .await
        .unwrap();
    repo.activate_library_root_in_tx(&mut tx, target.id, "volume-identity".to_owned(), at(3))
        .await
        .unwrap();
    let unavailable = repo
        .mark_library_root_unavailable_in_tx(&mut tx, target.id, at(4))
        .await
        .unwrap();
    commit(tx).await.unwrap();
    assert_eq!(unavailable.state, StorageRootState::Unavailable);
    let user = repo.get_library_root(user.id).await.unwrap().unwrap();
    assert_eq!(user.default_staging_root_id, Some(target.id));
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
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: activate_library_root")
        .await
        .unwrap();
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
