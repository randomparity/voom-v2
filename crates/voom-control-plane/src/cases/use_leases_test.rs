use time::{Duration, OffsetDateTime};
use voom_core::VoomError;
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{FileLocationKind, NewFileLocation, NewFileVersion, ProducedBy};
use voom_store::repo::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind, UseLeaseReleaseReason,
    UseLeaseRepo,
};

use crate::cases::cp;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

fn ttl_input(scope: LeaseScope) -> NewUseLease {
    NewUseLease {
        kind: UseLeaseKind::Playback,
        scope,
        issuer_kind: IssuerKind::User,
        issuer_ref: "user:42".to_owned(),
        blocking_mode: BlockingMode::Advisory,
        ttl: Some(Duration::seconds(300)),
        acquired_at: T0,
    }
}

fn manual_input(scope: LeaseScope) -> NewUseLease {
    NewUseLease {
        kind: UseLeaseKind::ManualLock,
        scope,
        issuer_kind: IssuerKind::User,
        issuer_ref: "user:42".to_owned(),
        blocking_mode: BlockingMode::Blocking,
        ttl: None,
        acquired_at: T0,
    }
}

#[tokio::test]
async fn acquire_use_lease_emits_acquired_event() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(ttl_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::UseLeaseAcquired).await, 1);

    // Verify payload fields round-trip through the event store.
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::UseLeaseAcquired),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::UseLeaseAcquired(payload) = &page.items[0].envelope.payload else {
        panic!("expected UseLeaseAcquired payload");
    };
    assert_eq!(payload.lease_id, lease.id.0);
    assert_eq!(payload.scope_type, "asset");
    assert_eq!(payload.scope_id, asset.id.0);
    assert!(payload.ttl_bound);
}

#[tokio::test]
async fn heartbeat_use_lease_no_event_emitted() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(ttl_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    let before = count(&cp, EventKind::UseLeaseAcquired).await;
    cp.heartbeat_use_lease(lease.id, T0 + Duration::seconds(60))
        .await
        .unwrap();
    // No new event kinds emitted; acquired count stays the same.
    assert_eq!(count(&cp, EventKind::UseLeaseAcquired).await, before);
}

#[tokio::test]
async fn release_use_lease_emits_released_event() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(ttl_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    cp.release_use_lease(
        lease.id,
        UseLeaseReleaseReason::Released,
        T0 + Duration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::UseLeaseReleased).await, 1);

    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::UseLeaseReleased),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::UseLeaseReleased(payload) = &page.items[0].envelope.payload else {
        panic!("expected UseLeaseReleased payload");
    };
    assert_eq!(payload.lease_id, lease.id.0);
    assert_eq!(payload.release_reason, "released");
}

#[tokio::test]
async fn force_release_use_lease_emits_force_released_with_actor() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(ttl_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    cp.force_release_use_lease(
        lease.id,
        "admin".to_owned(),
        "maintenance".to_owned(),
        T0 + Duration::seconds(10),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::UseLeaseForceReleased).await, 1);

    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::UseLeaseForceReleased),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::UseLeaseForceReleased(payload) = &page.items[0].envelope.payload else {
        panic!("expected UseLeaseForceReleased payload");
    };
    assert_eq!(payload.lease_id, lease.id.0);
    assert_eq!(payload.actor, "admin");
    assert_eq!(payload.reason, "maintenance");
}

#[tokio::test]
async fn force_release_use_lease_rejects_empty_actor() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(ttl_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    let before = count(&cp, EventKind::UseLeaseForceReleased).await;
    let err = cp
        .force_release_use_lease(
            lease.id,
            String::new(),
            "maintenance".to_owned(),
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    // Validation runs before the tx — no event row should have been written.
    assert_eq!(count(&cp, EventKind::UseLeaseForceReleased).await, before);
    // And the lease is still live.
    let still = cp.use_leases().get(lease.id).await.unwrap().unwrap();
    assert!(still.is_live());
}

#[tokio::test]
async fn force_release_use_lease_rejects_whitespace_reason() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(ttl_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    let before = count(&cp, EventKind::UseLeaseForceReleased).await;
    let err = cp
        .force_release_use_lease(
            lease.id,
            "admin".to_owned(),
            "   \t\n".to_owned(),
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    assert_eq!(count(&cp, EventKind::UseLeaseForceReleased).await, before);
    let still = cp.use_leases().get(lease.id).await.unwrap().unwrap();
    assert!(still.is_live());
}

#[tokio::test]
async fn recover_use_lease_stale_issuer_rejects_empty_actor() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(manual_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    let before = count(&cp, EventKind::UseLeaseRecoveredStaleIssuer).await;
    let err = cp
        .recover_use_lease_stale_issuer(
            lease.id,
            String::new(),
            "issuer gone".to_owned(),
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    assert_eq!(
        count(&cp, EventKind::UseLeaseRecoveredStaleIssuer).await,
        before
    );
    let still = cp.use_leases().get(lease.id).await.unwrap().unwrap();
    assert!(still.is_live());
}

#[tokio::test]
async fn recover_use_lease_stale_issuer_rejects_whitespace_reason() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    let lease = cp
        .acquire_use_lease(manual_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    let before = count(&cp, EventKind::UseLeaseRecoveredStaleIssuer).await;
    let err = cp
        .recover_use_lease_stale_issuer(
            lease.id,
            "ops-bot".to_owned(),
            "  ".to_owned(),
            T0 + Duration::seconds(10),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    assert_eq!(
        count(&cp, EventKind::UseLeaseRecoveredStaleIssuer).await,
        before
    );
    let still = cp.use_leases().get(lease.id).await.unwrap().unwrap();
    assert!(still.is_live());
}

#[tokio::test]
async fn expire_due_use_leases_emits_one_event_per_lease() {
    let (cp, _tmp) = cp().await;
    let a1 = cp.create_file_asset(T0).await.unwrap();
    let a2 = cp.create_file_asset(T0).await.unwrap();
    // Acquire two leases with a short TTL (30s).
    let l1 = cp
        .acquire_use_lease(NewUseLease {
            ttl: Some(Duration::seconds(30)),
            scope: LeaseScope::Asset(a1.id),
            ..ttl_input(LeaseScope::Asset(a1.id))
        })
        .await
        .unwrap();
    let l2 = cp
        .acquire_use_lease(NewUseLease {
            ttl: Some(Duration::seconds(30)),
            scope: LeaseScope::Asset(a2.id),
            ..ttl_input(LeaseScope::Asset(a2.id))
        })
        .await
        .unwrap();
    // Expire at T0 + 60s (both leases should expire).
    let report = cp
        .expire_due_use_leases(T0 + Duration::seconds(60))
        .await
        .unwrap();
    assert_eq!(report.expired.len(), 2);
    assert!(report.expired.contains(&l1.id));
    assert!(report.expired.contains(&l2.id));
    assert_eq!(count(&cp, EventKind::UseLeaseExpired).await, 2);
}

#[tokio::test]
async fn recover_use_lease_stale_issuer_emits_recovered_event() {
    let (cp, _tmp) = cp().await;
    let asset = cp.create_file_asset(T0).await.unwrap();
    // ManualLock has no TTL — it is the only valid target for recover.
    let lease = cp
        .acquire_use_lease(manual_input(LeaseScope::Asset(asset.id)))
        .await
        .unwrap();
    cp.recover_use_lease_stale_issuer(
        lease.id,
        "ops-bot".to_owned(),
        "issuer process disappeared".to_owned(),
        T0 + Duration::seconds(120),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::UseLeaseRecoveredStaleIssuer).await, 1);

    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::UseLeaseRecoveredStaleIssuer),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::UseLeaseRecoveredStaleIssuer(payload) = &page.items[0].envelope.payload
    else {
        panic!("expected UseLeaseRecoveredStaleIssuer payload");
    };
    assert_eq!(payload.lease_id, lease.id.0);
    assert_eq!(payload.actor, "ops-bot");
}

#[tokio::test]
async fn reanchor_use_leases_on_move_emits_one_event_per_lease() {
    let (cp, _tmp) = cp().await;
    // Build minimal identity chain: asset → version → two locations.
    let asset = cp.create_file_asset(T0).await.unwrap();
    let version = cp
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "sha256:aabbcc".to_owned(),
            size_bytes: 1024,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let loc_retired = cp
        .create_file_location(NewFileLocation {
            file_version_id: version.id,
            kind: FileLocationKind::LocalPath,
            value: "/mnt/old/file.mkv".to_owned(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();
    let loc_new = cp
        .create_file_location(NewFileLocation {
            file_version_id: version.id,
            kind: FileLocationKind::LocalPath,
            value: "/mnt/new/file.mkv".to_owned(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();

    // Acquire two location-scoped use leases on the retired location.
    let l1 = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Copy,
            scope: LeaseScope::Location(loc_retired.id),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "worker:1".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(Duration::seconds(300)),
            acquired_at: T0,
        })
        .await
        .unwrap();
    let l2 = cp
        .acquire_use_lease(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Location(loc_retired.id),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "worker:2".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(Duration::seconds(300)),
            acquired_at: T0,
        })
        .await
        .unwrap();

    let report = cp
        .reanchor_use_leases_on_move(loc_retired.id, loc_new.id, T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(report.reanchored.len(), 2);
    assert!(report.reanchored.contains(&l1.id));
    assert!(report.reanchored.contains(&l2.id));
    assert_eq!(count(&cp, EventKind::UseLeaseReanchoredByMove).await, 2);

    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::UseLeaseReanchoredByMove),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    // Both events should reference the correct location ids.
    for item in &page.items {
        let voom_events::Event::UseLeaseReanchoredByMove(p) = &item.envelope.payload else {
            panic!("expected UseLeaseReanchoredByMove payload");
        };
        assert_eq!(p.retired_location_id, loc_retired.id.0);
        assert_eq!(p.new_location_id, loc_new.id.0);
    }
}
