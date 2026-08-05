use std::future::Future;
use std::sync::{Arc, Condvar, Mutex};

use voom_core::{
    LibraryId, NodeId, NodeStatus, ProviderLocator, StorageProviderKind, StorageRootState,
    VoomError,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryRoot, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

use crate::cases::cp;

fn new_library(slug: &str) -> NewLibrary {
    NewLibrary {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        media_kind: LibraryMediaKind::Movie,
        description: None,
        enabled: true,
    }
}

fn new_root(library_id: LibraryId, owner_node_id: NodeId, path: &str) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(path.to_owned()).unwrap(),
        display_locator: path.to_owned(),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root_id: None,
        default_staging_root_id: None,
        default_backup_root_id: None,
        enabled: true,
    }
}

async fn node(cp: &crate::ControlPlane, name: &str, status: NodeStatus) -> NodeId {
    let id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (?, 'local', ?, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 60, 'hash', 'hint', '{}')",
    )
    .bind(name)
    .bind(status.as_str())
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    NodeId(u64::try_from(id).unwrap())
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

    fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_one();
    }

    async fn wait_until_entered(&self) {
        let entered =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.entered.notified()).await;
        assert!(entered.is_ok(), "transaction did not reach rollback hook");
    }
}

async fn assert_lifecycle_error_awaits_rollback<Operation, OperationFuture>(
    cp: &crate::ControlPlane,
    operation_name: &str,
    operation: Operation,
) -> VoomError
where
    Operation: FnOnce(crate::ControlPlane) -> OperationFuture,
    OperationFuture: Future<Output = Result<LibraryRoot, VoomError>> + Send + 'static,
{
    let mut held_connections = Vec::new();
    for _ in 0..cp.pool_for_test().options().get_max_connections() {
        held_connections.push(cp.pool_for_test().acquire().await.unwrap());
    }
    let mut rollback_connection = held_connections.pop().unwrap();
    let rollback = RollbackBarrier::new();
    rollback_connection
        .lock_handle()
        .await
        .unwrap()
        .set_rollback_hook(rollback.callback());
    rollback_connection.return_to_pool().await;
    sqlx::query(
        "CREATE TRIGGER reject_root_lifecycle_event BEFORE INSERT ON events \
         BEGIN SELECT RAISE(FAIL, 'forced root lifecycle event failure'); END",
    )
    .execute(&mut *held_connections[0])
    .await
    .unwrap();

    let task = tokio::spawn(operation(cp.clone()));
    rollback.wait_until_entered().await;
    tokio::task::yield_now().await;
    let returned_before_rollback = task.is_finished();
    rollback.release();
    let error = task.await.unwrap().unwrap_err();
    drop(held_connections);

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("events append"));
    assert!(
        !returned_before_rollback,
        "{operation_name} returned before its failed transaction released SQLite writer ownership"
    );
    error
}

#[tokio::test]
async fn library_and_root_crud_round_trip() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let owner = node(&cp, "node-a", NodeStatus::Registered).await;
    assert!(
        cp.list_libraries()
            .await
            .unwrap()
            .iter()
            .any(|listed| listed.id == lib.id)
    );

    let root = cp
        .create_library_root(new_root(lib.id, owner, "/media/films"))
        .await
        .unwrap();
    let fetched = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(fetched.provider_locator.as_str(), "/media/films");
    assert_eq!(cp.list_library_roots(Some(lib.id)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn owner_scoped_provider_locator_identity_is_enforced() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    let owner_a = node(&cp, "node-a", NodeStatus::Registered).await;
    let owner_b = node(&cp, "node-b", NodeStatus::Registered).await;
    cp.create_library_root(new_root(lib.id, owner_a, "/media/films"))
        .await
        .unwrap();

    let duplicate = cp
        .create_library_root(new_root(lib.id, owner_a, "/media/films"))
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), "CONFLICT");

    cp.create_library_root(new_root(lib.id, owner_b, "/media/films"))
        .await
        .unwrap();
}

#[tokio::test]
async fn assign_owner_awaits_event_failure_rollback() {
    let (cp, _tmp) = cp().await;
    let library = cp.create_library(new_library("assign")).await.unwrap();
    let initial_owner = node(&cp, "initial-owner", NodeStatus::Registered).await;
    let assigned_owner = node(&cp, "assigned-owner", NodeStatus::Registered).await;
    let root = cp
        .create_library_root(new_root(library.id, initial_owner, "/assign"))
        .await
        .unwrap();
    sqlx::query("UPDATE library_roots SET owner_node_id = NULL, state = 'unassigned' WHERE id = ?")
        .bind(i64::try_from(root.id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    assert_lifecycle_error_awaits_rollback(&cp, "assign owner", move |cp| async move {
        cp.assign_library_root_owner(root.id, assigned_owner).await
    })
    .await;

    let unchanged = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(unchanged.owner_node_id, None);
    assert_eq!(unchanged.state, StorageRootState::Unassigned);
}

#[tokio::test]
async fn activate_awaits_event_failure_rollback() {
    let (cp, _tmp) = cp().await;
    let library = cp.create_library(new_library("activate")).await.unwrap();
    let owner = node(&cp, "active-owner", NodeStatus::Active).await;
    let root = cp
        .create_library_root(new_root(library.id, owner, "/activate"))
        .await
        .unwrap();

    assert_lifecycle_error_awaits_rollback(&cp, "activate", move |cp| async move {
        cp.activate_library_root(root.id, "volume-identity".to_owned())
            .await
    })
    .await;

    let unchanged = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, StorageRootState::Configured);
    assert_eq!(unchanged.activation_identity, None);
    assert_eq!(unchanged.root_epoch, 0);
}

#[tokio::test]
async fn mark_unavailable_awaits_event_failure_rollback() {
    let (cp, _tmp) = cp().await;
    let library = cp.create_library(new_library("unavailable")).await.unwrap();
    let owner = node(&cp, "unavailable-owner", NodeStatus::Active).await;
    let root = cp
        .create_library_root(new_root(library.id, owner, "/unavailable"))
        .await
        .unwrap();
    let root = cp
        .activate_library_root(root.id, "volume-identity".to_owned())
        .await
        .unwrap();

    assert_lifecycle_error_awaits_rollback(&cp, "mark unavailable", move |cp| async move {
        cp.mark_library_root_unavailable(root.id, "owner offline".to_owned())
            .await
    })
    .await;

    let unchanged = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, StorageRootState::Active);
    assert_eq!(
        unchanged.activation_identity.as_deref(),
        Some("volume-identity")
    );
}

#[tokio::test]
async fn retire_awaits_event_failure_rollback() {
    let (cp, _tmp) = cp().await;
    let library = cp.create_library(new_library("retire")).await.unwrap();
    let owner = node(&cp, "retire-owner", NodeStatus::Registered).await;
    let root = cp
        .create_library_root(new_root(library.id, owner, "/retire"))
        .await
        .unwrap();

    assert_lifecycle_error_awaits_rollback(&cp, "retire", move |cp| async move {
        cp.retire_library_root(root.id).await
    })
    .await;

    let unchanged = cp.get_library_root(root.id).await.unwrap().unwrap();
    assert_eq!(unchanged.state, StorageRootState::Configured);
    assert!(unchanged.enabled);
}

#[tokio::test]
async fn disable_then_enable_library() {
    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();
    assert!(!cp.set_library_enabled(lib.id, false).await.unwrap().enabled);
    assert!(cp.set_library_enabled(lib.id, true).await.unwrap().enabled);
}

#[tokio::test]
async fn set_default_scoring_profile_validates_existence_and_retire() {
    use voom_store::repo::policy::quality_scoring_profiles::NewQualityScoringProfile;

    let (cp, _tmp) = cp().await;
    let lib = cp.create_library(new_library("films")).await.unwrap();

    // Unknown profile is refused.
    let err = cp
        .set_library_default_scoring_profile(lib.id, Some("ghost"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");

    cp.create_scoring_profile(NewQualityScoringProfile {
        name: "balanced-home".to_owned(),
        version: 1,
        definition: serde_json::json!({ "weights": {} }),
    })
    .await
    .unwrap();

    let linked = cp
        .set_library_default_scoring_profile(lib.id, Some("balanced-home"))
        .await
        .unwrap();
    assert_eq!(
        linked.default_scoring_profile_name.as_deref(),
        Some("balanced-home")
    );

    // A retired profile cannot become a library default.
    cp.retire_scoring_profile("balanced-home").await.unwrap();
    let err = cp
        .set_library_default_scoring_profile(lib.id, Some("balanced-home"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");

    // Clearing is always allowed.
    let cleared = cp
        .set_library_default_scoring_profile(lib.id, None)
        .await
        .unwrap();
    assert_eq!(cleared.default_scoring_profile_name, None);
}
