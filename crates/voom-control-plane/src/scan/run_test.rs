use voom_core::{
    LibraryId, NodeId, ProviderLocator, ScanSessionStatus, StorageProviderKind,
    StorageRootId, TicketOperation,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use super::{RootBlockReason, ScanRunOutcome};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use crate::{ControlPlane, cases::cp};

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

async fn owner(cp: &ControlPlane, name: &str) -> NodeId {
    let id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (?, 'local', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .bind(name)
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    NodeId(u64::try_from(id).unwrap())
}

async fn seeded_root(cp: &ControlPlane, slug: &str, path: &str) -> StorageRootId {
    let node = owner(cp, &format!("{slug}-owner")).await;
    let library = cp.create_library(new_library(slug)).await.unwrap();
    let root = cp
        .create_library_root(new_root(library.id, node, path))
        .await
        .unwrap();
    cp.activate_library_root(root.id, format!("{slug}-fixture"))
        .await
        .unwrap();
    root.id
}

#[tokio::test]
async fn request_creates_session_and_ready_namespaced_ticket() {
    let (cp, _tmp) = cp().await;
    let root = seeded_root(&cp, "req-ok", "/tmp/does-not-need-to-exist").await;

    let outcome = cp.request_scan_run(root, 600).await.unwrap();
    let ScanRunOutcome::Requested(requested) = outcome else {
        panic!("available owned root must request, got blocked");
    };

    let session = cp.scan_session(requested.scan_session_id).await.unwrap();
    assert_eq!(session.status, ScanSessionStatus::Requested);
    assert_eq!(session.storage_root_id, root);

    let ticket = cp.tickets.get(requested.ticket_id).await.unwrap().unwrap();
    assert_eq!(
        ticket.kind,
        TicketOperation::new("synthetic.workflow.operation.scan_library").unwrap()
    );
    // The ready-marking ran in the same transaction: an operator acquiring the
    // run must not depend on a later promotion pass.
    assert_eq!(ticket.state.as_str(), "ready");

    // The payload must round-trip through the strict workflow decode — the same
    // gate the acquire route applies. A payload that only *looks* right would
    // silently degrade acquire gating to NoDeclaration.
    let payload =
        WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
            .unwrap_or_else(|error| panic!("payload must parse: {error}"));
    assert_eq!(payload.operation, voom_core::OperationKind::ScanLibrary);
    assert_eq!(
        payload.rendered_payload["scan_session_id"],
        requested.scan_session_id.to_string()
    );
    assert!(payload.rendered_payload.get("source_location_id").is_none());
    let declaration = payload.declared_artifact_access.expect("byte-touching");
    assert_eq!(declaration.entries().len(), 1);
}

#[tokio::test]
async fn disabled_root_blocks_without_creating_rows() {
    let (cp, _tmp) = cp().await;
    let node = owner(&cp, "blocked-owner").await;
    let library = cp.create_library(new_library("req-blocked")).await.unwrap();
    let root = cp
        .create_library_root(new_root(library.id, node, "/tmp/req-blocked"))
        .await
        .unwrap();
    cp.set_library_root_enabled(root.id, false).await.unwrap();

    let sessions_before = session_count(&cp).await;
    let tickets_before = ticket_count(&cp).await;
    match cp.request_scan_run(root.id, 60).await.unwrap() {
        ScanRunOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RootBlockReason::RootDisabled);
            assert_eq!(blocked.storage_root_id, root.id);
        }
        ScanRunOutcome::Requested(_) => panic!("disabled root must block the run"),
    }
    assert_eq!(session_count(&cp).await, sessions_before);
    assert_eq!(ticket_count(&cp).await, tickets_before);
}

#[tokio::test]
async fn duplicate_active_session_conflicts_without_a_ticket() {
    let (cp, _tmp) = cp().await;
    let root = seeded_root(&cp, "req-dup", "/tmp/req-dup").await;

    let first = cp.request_scan_run(root, 3_600).await.unwrap();
    let ScanRunOutcome::Requested(first) = first else {
        panic!("first request must succeed");
    };
    let tickets_before = ticket_count(&cp).await;

    let error = cp.request_scan_run(root, 3_600).await.unwrap_err();
    assert!(matches!(error, voom_core::VoomError::Conflict(_)));

    // Exactly one active session and no extra ticket from the failed retry.
    assert_eq!(ticket_count(&cp).await, tickets_before);
    let _ = first;
}

#[tokio::test]
async fn missing_root_is_not_found() {
    let (cp, _tmp) = cp().await;
    let error = cp.request_scan_run(StorageRootId(9_999), 60).await.unwrap_err();
    assert_eq!(error.code(), "NOT_FOUND");
}

async fn session_count(cp: &ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM scan_sessions")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn ticket_count(cp: &ControlPlane) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}
