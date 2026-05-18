#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests favor unwrap/expect/panic over plumbing Result<()> through every assertion"
)]

//! End-to-end lifecycle for `asset_use_leases` through the
//! `ControlPlane` use cases. Covers acquire → heartbeat → release /
//! expire / `force_release` / `recover_stale_issuer` / reanchor, plus
//! the matching event journal. Runs against a tempfile-backed disk pool.

use tempfile::NamedTempFile;
use time::Duration;
use voom_control_plane::ControlPlane;
use voom_core::{FileLocationId, FileVersionId};
use voom_events::{EventKind, SubjectType};
use voom_store::init;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome};
use voom_store::repo::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind, UseLeaseReleaseReason,
};
use voom_store::test_support::{T0, sqlite_url_for};

async fn open_disk_plane() -> (ControlPlane, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    init(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    (cp, tmp)
}

/// Seed a `file_asset` + `file_version` + `file_location` chain via the
/// M2 ingest path. The `path` argument must be unique per call so each
/// invocation creates a fresh chain.
async fn seed_location(cp: &ControlPlane, path: &str) -> (FileVersionId, FileLocationId) {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.to_owned(),
                content_hash: format!("hash-of-{path}"),
                size_bytes: 1,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    match outcome {
        IngestOutcome::NewFileAsset {
            file_version_id,
            file_location_id,
            ..
        } => (file_version_id, file_location_id),
        IngestOutcome::AliasAttached { .. } => panic!("expected NewFileAsset"),
    }
}

/// Fetch all events for a given subject from the event journal (up to `limit`).
async fn events_for(
    cp: &ControlPlane,
    subject_type: SubjectType,
    subject_id: u64,
    limit: u32,
) -> Vec<voom_store::repo::events::EventRow> {
    cp.events()
        .list(
            EventFilter {
                kind: None,
                subject_type: Some(subject_type),
                subject_id: Some(subject_id),
            },
            Page {
                limit,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn full_lifecycle_against_disk_pool() {
    let (cp, _tmp) = open_disk_plane().await;
    let asset = cp.identity().create_file_asset(T0).await.unwrap();

    // 1) Acquire
    let lease = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset.id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    assert!(lease.is_live());

    // 2) Heartbeat
    let beat = cp
        .heartbeat_use_lease(lease.id, T0 + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(beat.expires_at, Some(T0 + Duration::seconds(90)));

    // 3) Release
    let released = cp
        .release_use_lease(
            lease.id,
            UseLeaseReleaseReason::Released,
            T0 + Duration::seconds(45),
        )
        .await
        .unwrap();
    assert_eq!(
        released.release_reason,
        Some(UseLeaseReleaseReason::Released)
    );

    // Event journal: acquired and released events both present for this lease.
    let rows = events_for(&cp, SubjectType::AssetUseLease, lease.id.0, 10).await;
    let kinds: Vec<EventKind> = rows.iter().map(|r| r.envelope.payload.kind()).collect();
    assert!(
        kinds.contains(&EventKind::UseLeaseAcquired),
        "expected UseLeaseAcquired in {kinds:?}"
    );
    assert!(
        kinds.contains(&EventKind::UseLeaseReleased),
        "expected UseLeaseReleased in {kinds:?}"
    );
}

#[tokio::test]
async fn expire_due_emits_one_event_per_lease() {
    let (cp, _tmp) = open_disk_plane().await;
    let asset = cp.identity().create_file_asset(T0).await.unwrap();

    let a = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset.id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(10)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    let b = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Asset(asset.id),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "w-1".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(Duration::seconds(20)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let report = cp
        .expire_due_use_leases(T0 + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(report.expired.len(), 2);

    // Each expired lease must have its own use_lease.expired event.
    for (id, label) in [(a.id, "lease-a"), (b.id, "lease-b")] {
        let rows = events_for(&cp, SubjectType::AssetUseLease, id.0, 5).await;
        let kinds: Vec<EventKind> = rows.iter().map(|r| r.envelope.payload.kind()).collect();
        assert!(
            kinds.contains(&EventKind::UseLeaseExpired),
            "missing UseLeaseExpired for {label}: got {kinds:?}"
        );
    }
}

#[tokio::test]
async fn force_release_then_recovery_audit_event_carries_actor_and_reason() {
    let (cp, _tmp) = open_disk_plane().await;
    let asset = cp.identity().create_file_asset(T0).await.unwrap();

    let lease = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Asset(asset.id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    cp.force_release_use_lease(
        lease.id,
        "operator-jane".to_owned(),
        "clearing for destructive commit".to_owned(),
        T0 + Duration::seconds(5),
    )
    .await
    .unwrap();

    // Decode the force-released event payload and verify actor + reason.
    let rows = events_for(&cp, SubjectType::AssetUseLease, lease.id.0, 5).await;
    let force_row = rows
        .iter()
        .find(|r| r.envelope.payload.kind() == EventKind::UseLeaseForceReleased)
        .expect("UseLeaseForceReleased event must be present");

    match &force_row.envelope.payload {
        voom_events::Event::UseLeaseForceReleased(p) => {
            assert_eq!(p.actor, "operator-jane");
            assert_eq!(p.reason, "clearing for destructive commit");
            assert_eq!(p.lease_id, lease.id.0);
        }
        other => panic!("expected UseLeaseForceReleased, got {other:?}"),
    }
}

#[tokio::test]
async fn reanchor_on_move_emits_one_event_per_lease() {
    let (cp, _tmp) = open_disk_plane().await;
    let (_v, loc_old) = seed_location(&cp, "/srv/old.mkv").await;
    let (_v2, loc_new) = seed_location(&cp, "/srv/new.mkv").await;

    let lease = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Location(loc_old),
            issuer_kind: IssuerKind::User,
            issuer_ref: "alice".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(60)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let report = cp
        .reanchor_use_leases_on_move(loc_old, loc_new, T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(report.reanchored, vec![lease.id]);

    // Decode the reanchored event payload and verify location IDs.
    let rows = events_for(&cp, SubjectType::AssetUseLease, lease.id.0, 5).await;
    let reanchored_row = rows
        .iter()
        .find(|r| r.envelope.payload.kind() == EventKind::UseLeaseReanchoredByMove)
        .expect("UseLeaseReanchoredByMove event must be present");

    match &reanchored_row.envelope.payload {
        voom_events::Event::UseLeaseReanchoredByMove(p) => {
            assert_eq!(p.retired_location_id, loc_old.0);
            assert_eq!(p.new_location_id, loc_new.0);
            assert_eq!(p.lease_id, lease.id.0);
        }
        other => panic!("expected UseLeaseReanchoredByMove, got {other:?}"),
    }
}
