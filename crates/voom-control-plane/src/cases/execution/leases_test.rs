use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use time::{Duration as TDuration, OffsetDateTime};
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{
    Clock, FailureClass, JobId, TicketId, TicketOperation, VoomError, WorkerId, WorkerKind,
};
use voom_events::{EventKind, SubjectType};
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page};
use voom_store::repo::execution::accelerator_claims::{
    NewAcceleratorClaim, SqliteAcceleratorClaimRepo,
};
use voom_store::repo::execution::jobs::{JobState, NewJob};
use voom_store::repo::execution::leases::{LeaseAcquireOutcome, LeaseFilter, LeaseState};
use voom_store::repo::execution::tickets::{NewTicket, Ticket, TicketState};
use voom_store::repo::execution::workers::{NewCapability, NewGrant, Worker};
use voom_store::repo::media::bundles::NewAssetBundle;
use voom_store::repo::media::identity::{
    DiscoveredFile, IngestOutcome, MediaWorkKind, NewMediaVariant, NewMediaWork,
};

use crate::cases::workers::RegisterWorkerInput;
use crate::cases::{count, cp, issue_link_targets, terminal_failure_issues};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn ticket(kind: &str, max_attempts: u32) -> NewTicket {
    NewTicket {
        job_id: None,
        kind: TicketOperation::new(kind).unwrap(),
        priority: 0,
        payload: serde_json::json!({}),
        max_attempts,
        created_at: T0,
    }
}

fn ticket_for_job(kind: &str, max_attempts: u32, job_id: JobId) -> NewTicket {
    NewTicket {
        job_id: Some(job_id),
        ..ticket(kind, max_attempts)
    }
}

fn worker(name: &str) -> RegisterWorkerInput {
    RegisterWorkerInput {
        name: name.to_owned(),
        kind: WorkerKind::Synthetic,
    }
}

async fn eligible_worker(
    cp: &crate::ControlPlane,
    name: &str,
    operation: &TicketOperation,
) -> Worker {
    let worker = cp.register_worker(worker(name)).await.unwrap();
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: serde_json::json!({}),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation.clone()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: serde_json::json!({}),
    })
    .await
    .unwrap();
    worker
}

async fn grant_capacity(
    cp: &crate::ControlPlane,
    worker: &Worker,
    operation: &TicketOperation,
    limit: u32,
) {
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation.clone()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: serde_json::json!({operation.as_str(): limit}),
    })
    .await
    .unwrap();
}

async fn nvidia_worker(
    cp: &crate::ControlPlane,
    name: &str,
    operation: &TicketOperation,
    hardware_token: &str,
    max_sessions: u32,
) -> Worker {
    let worker = cp.register_worker(worker(name)).await.unwrap();
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: vec![hardware_token.to_owned()],
        artifact_access: Vec::new(),
        extra: serde_json::json!({
            "accelerator": {
                "backend": "nvidia",
                "hardware_token": hardware_token,
                "device_uuid": hardware_token.trim_start_matches("nvidia:"),
                "device_name": "Test GPU",
                "driver_version": "595.80",
                "encoders": ["hevc_nvenc"],
                "decoders": ["h264_cuvid", "hevc_cuvid"],
                "max_sessions": max_sessions
            }
        }),
    })
    .await
    .unwrap();
    grant_capacity(cp, &worker, operation, max_sessions).await;
    worker
}

/// A VAAPI-bound worker as `local_worker.rs` records one: the token is derived from
/// the PCI address, and the stored descriptor is `backend`-tagged and carries no
/// `hardware_token` field of its own (ADR 0052 §1).
async fn vaapi_worker(
    cp: &crate::ControlPlane,
    name: &str,
    operation: &TicketOperation,
    pci_address: &str,
    max_sessions: u32,
) -> Worker {
    let worker = cp.register_worker(worker(name)).await.unwrap();
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: vec![format!("vaapi:pci-{pci_address}")],
        artifact_access: Vec::new(),
        extra: serde_json::json!({
            "accelerator": {
                "backend": "vaapi",
                "pci_address": pci_address,
                "device_name": "AMD Radeon 8060S Graphics",
                "driver_version": "Mesa Gallium 26.1.5 radeonsi",
                "encoders": ["hevc_vaapi"],
                "decoders": ["h264", "hevc", "av1"],
                "max_sessions": max_sessions
            }
        }),
    })
    .await
    .unwrap();
    grant_capacity(cp, &worker, operation, max_sessions).await;
    worker
}

async fn ready_transcode_tickets(cp: &crate::ControlPlane, count: usize) -> Vec<Ticket> {
    let mut tickets = Vec::with_capacity(count);
    for _ in 0..count {
        let ticket = cp
            .create_ticket(ticket("transcode_video", 2))
            .await
            .unwrap();
        cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
        tickets.push(ticket);
    }
    tickets
}

async fn cp_at_t0() -> (
    crate::ControlPlane,
    Arc<ManualClock>,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock.clone(),
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, clock, tmp)
}

async fn cp_with_zero_busy_timeout() -> (
    crate::ControlPlane,
    Arc<ManualClock>,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let options: SqliteConnectOptions = url
        .parse::<SqliteConnectOptions>()
        .unwrap()
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30))
        .disable_statement_logging();
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock.clone(),
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    let mut first = cp.pool_for_test().acquire().await.unwrap();
    let mut second = cp.pool_for_test().acquire().await.unwrap();
    sqlx::query("PRAGMA busy_timeout = 0")
        .execute(&mut *first)
        .await
        .unwrap();
    sqlx::query("PRAGMA busy_timeout = 0")
        .execute(&mut *second)
        .await
        .unwrap();
    drop((first, second));
    (cp, clock, tmp)
}

async fn held_noop_lease(cp: &crate::ControlPlane) -> voom_store::repo::execution::leases::Lease {
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let ticket = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let worker =
        eligible_worker(cp, &format!("heartbeat-worker-{lease_count}"), &ticket.kind).await;
    cp.acquire_lease(NewLease {
        ticket_id: ticket.id,
        worker_id: worker.id,
        ttl: TDuration::minutes(1),
        now: T0,
    })
    .await
    .unwrap()
}

async fn seed_audio_claim_pair(
    cp: &crate::ControlPlane,
    lease_id: voom_core::LeaseId,
    suffix: &str,
    extract_expires_at: OffsetDateTime,
    synthesis_expires_at: OffsetDateTime,
) {
    voom_store::test_support::seed_test_storage_root(cp.pool_for_test())
        .await
        .unwrap();
    let discovered = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &format!("heartbeat-test/{suffix}.mkv"),
                ),
                content_hash: format!("blake3:{suffix}"),
                size_bytes: 1,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = discovered
    else {
        panic!("heartbeat claim source must be new");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({"container": "mkv", "streams": []}),
            T0,
        )
        .await
        .unwrap();
    let work = cp
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: suffix.to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let variant = cp
        .create_media_variant(NewMediaVariant {
            media_work_id: work.id,
            label: suffix.to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    let bundle = cp
        .create_bundle(NewAssetBundle {
            media_variant_id: variant.id,
            display_name: suffix.to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO audio_extract_operations \
         (operation_key, operation_id, source_file_version_id, source_bundle_id, \
          source_media_snapshot_id, state, claim_lease_id, claim_token, claim_expires_at, \
          created_at) VALUES (?, ?, ?, ?, ?, 'planned', ?, ?, ?, ?)",
    )
    .bind(format!("extract:{suffix}"))
    .bind(format!("extract-{suffix}"))
    .bind(i64::try_from(file_version_id.0).unwrap())
    .bind(i64::try_from(bundle.id.0).unwrap())
    .bind(i64::try_from(snapshot.id.0).unwrap())
    .bind(i64::try_from(lease_id.0).unwrap())
    .bind(format!("extract-claim-{suffix}"))
    .bind(extract_expires_at)
    .bind(T0)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO audio_synthesis_operations \
         (operation_key, planned_operation_id, source_file_version_id, \
          source_media_snapshot_id, target_codec, target_channels, container, target_path, \
          state, claim_lease_id, claim_token, claim_expires_at, created_at) \
         VALUES (?, ?, ?, ?, 'aac', 2, 'mkv', ?, 'planned', ?, ?, ?, ?)",
    )
    .bind(format!("synthesis:{suffix}"))
    .bind(format!("synthesis-{suffix}"))
    .bind(i64::try_from(file_version_id.0).unwrap())
    .bind(i64::try_from(snapshot.id.0).unwrap())
    .bind(format!("/heartbeat-test/{suffix}-synthesized.mkv"))
    .bind(i64::try_from(lease_id.0).unwrap())
    .bind(format!("synthesis-claim-{suffix}"))
    .bind(synthesis_expires_at)
    .bind(T0)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

type LeaseHeartbeatRow = (String, String, String, i64);
type AudioClaimRow = (String, i64, String, String);

async fn lease_heartbeat_row(
    cp: &crate::ControlPlane,
    lease_id: voom_core::LeaseId,
) -> Option<LeaseHeartbeatRow> {
    sqlx::query_as("SELECT state, expires_at, last_heartbeat_at, epoch FROM leases WHERE id = ?")
        .bind(i64::try_from(lease_id.0).unwrap())
        .fetch_optional(cp.pool_for_test())
        .await
        .unwrap()
}

async fn audio_claim_rows(cp: &crate::ControlPlane, table: &str) -> Vec<AudioClaimRow> {
    let query = format!(
        "SELECT operation_key, claim_lease_id, claim_token, claim_expires_at \
         FROM {table} WHERE claim_lease_id IS NOT NULL ORDER BY operation_key"
    );
    sqlx::query_as(&query)
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
}

#[tokio::test]
async fn heartbeat_rejects_missing_and_released_leases_without_mutation() {
    let (cp, _clock, _tmp) = cp_at_t0().await;
    let missing = cp
        .heartbeat_lease(
            voom_core::LeaseId(999),
            TDuration::minutes(1),
            T0 + TDuration::seconds(1),
        )
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("not held"));

    let lease = held_noop_lease(&cp).await;
    cp.release_lease(
        lease.id,
        serde_json::json!({"status": "done"}),
        T0 + TDuration::seconds(1),
    )
    .await
    .unwrap();
    let before = lease_heartbeat_row(&cp, lease.id).await;
    let released = cp
        .heartbeat_lease(lease.id, TDuration::minutes(1), T0 + TDuration::seconds(2))
        .await
        .unwrap_err();

    assert!(released.to_string().contains("not held"));
    assert_eq!(lease_heartbeat_row(&cp, lease.id).await, before);
}

#[tokio::test]
async fn heartbeat_rolls_back_lease_and_claims_when_extraction_claim_expired() {
    assert_expired_audio_claim_rolls_back(true).await;
}

#[tokio::test]
async fn heartbeat_rolls_back_lease_and_claims_when_synthesis_claim_expired() {
    assert_expired_audio_claim_rolls_back(false).await;
}

async fn assert_expired_audio_claim_rolls_back(extraction_expired: bool) {
    let (cp, _clock, _tmp) = cp_at_t0().await;
    let lease = held_noop_lease(&cp).await;
    let expired = T0 + TDuration::seconds(1);
    let live = T0 + TDuration::minutes(1);
    seed_audio_claim_pair(
        &cp,
        lease.id,
        if extraction_expired {
            "expired-extraction"
        } else {
            "expired-synthesis"
        },
        if extraction_expired { expired } else { live },
        if extraction_expired { live } else { expired },
    )
    .await;
    let before_lease = lease_heartbeat_row(&cp, lease.id).await;
    let before_extract = audio_claim_rows(&cp, "audio_extract_operations").await;
    let before_synthesis = audio_claim_rows(&cp, "audio_synthesis_operations").await;

    let error = cp
        .heartbeat_lease(lease.id, TDuration::minutes(2), T0 + TDuration::seconds(2))
        .await
        .unwrap_err();

    let expected = if extraction_expired {
        "expired audio extraction claim"
    } else {
        "expired audio synthesis claim"
    };
    assert!(error.to_string().contains(expected));
    assert_eq!(lease_heartbeat_row(&cp, lease.id).await, before_lease);
    assert_eq!(
        audio_claim_rows(&cp, "audio_extract_operations").await,
        before_extract
    );
    assert_eq!(
        audio_claim_rows(&cp, "audio_synthesis_operations").await,
        before_synthesis
    );
}

#[tokio::test]
async fn heartbeat_advances_only_claims_owned_by_its_lease() {
    let (cp, _clock, _tmp) = cp_at_t0().await;
    let target = held_noop_lease(&cp).await;
    let other = held_noop_lease(&cp).await;
    let original_expiry = T0 + TDuration::minutes(1);
    seed_audio_claim_pair(
        &cp,
        target.id,
        "target-owner",
        original_expiry,
        original_expiry,
    )
    .await;
    seed_audio_claim_pair(
        &cp,
        other.id,
        "other-owner",
        original_expiry,
        original_expiry,
    )
    .await;
    let before_extract = audio_claim_rows(&cp, "audio_extract_operations").await;
    let before_synthesis = audio_claim_rows(&cp, "audio_synthesis_operations").await;

    let heartbeat = cp
        .heartbeat_lease(target.id, TDuration::minutes(2), T0 + TDuration::seconds(1))
        .await
        .unwrap();

    let after_extract = audio_claim_rows(&cp, "audio_extract_operations").await;
    let after_synthesis = audio_claim_rows(&cp, "audio_synthesis_operations").await;
    for (before, after) in before_extract
        .iter()
        .zip(&after_extract)
        .chain(before_synthesis.iter().zip(&after_synthesis))
    {
        if before.1 == i64::try_from(other.id.0).unwrap() {
            assert_eq!(after, before, "another lease's claim must remain unchanged");
        } else {
            assert_eq!(after.0, before.0);
            assert_eq!(after.1, before.1);
            assert_eq!(after.2, before.2);
            assert_ne!(after.3, before.3);
        }
    }
    assert_eq!(heartbeat.last_heartbeat_at, T0 + TDuration::seconds(1));
}

#[tokio::test]
async fn heartbeat_reserves_writer_at_transaction_start() {
    let (cp, _clock, _tmp) = cp_with_zero_busy_timeout().await;
    let lease = held_noop_lease(&cp).await;
    let writer = begin_immediate_tx(cp.pool_for_test()).await.unwrap();

    let error = cp
        .heartbeat_lease(lease.id, TDuration::minutes(1), T0 + TDuration::seconds(1))
        .await
        .unwrap_err();
    writer.rollback().await.unwrap();

    assert!(
        error.to_string().contains("begin immediate"),
        "heartbeat contention must be reported at its immediate transaction opener: {error}"
    );
}

#[tokio::test]
async fn heartbeat_serializes_one_production_attempt_behind_a_writer() {
    let (cp, _clock, _tmp) = cp_at_t0().await;
    let lease = held_noop_lease(&cp).await;
    let cp = Arc::new(cp);
    let writer = begin_immediate_tx(cp.pool_for_test()).await.unwrap();
    let observer = Arc::new(HeartbeatTransactionObserver::default());
    let outer_attempts = Arc::new(AtomicUsize::new(0));
    let task_cp = Arc::clone(&cp);
    let task_observer = Arc::clone(&observer);
    let task_attempts = Arc::clone(&outer_attempts);
    let heartbeat = tokio::spawn(async move {
        crate::workflow::execution::leases::retry_on_database_locked(|| {
            task_attempts.fetch_add(1, Ordering::SeqCst);
            task_cp.heartbeat_lease_observed(
                lease.id,
                TDuration::minutes(1),
                T0 + TDuration::seconds(1),
                Some(task_observer.as_ref()),
            )
        })
        .await
    });

    observer.invoked.notified().await;
    assert_eq!(observer.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(outer_attempts.load(Ordering::SeqCst), 1);
    assert!(!observer.acquired.load(Ordering::SeqCst));
    assert!(!heartbeat.is_finished());

    writer.rollback().await.unwrap();
    let updated = heartbeat.await.unwrap().unwrap();

    assert_eq!(observer.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(outer_attempts.load(Ordering::SeqCst), 1);
    assert!(observer.acquired.load(Ordering::SeqCst));
    assert_eq!(updated.last_heartbeat_at, T0 + TDuration::seconds(1));
}

fn claim_for(
    worker_id: WorkerId,
    pci_address: &str,
    pid: u32,
    capacity: u32,
    claimed_at: OffsetDateTime,
) -> NewAcceleratorClaim {
    NewAcceleratorClaim {
        hardware_token: format!("vaapi:pci-{pci_address}"),
        backend: "vaapi".to_owned(),
        worker_id,
        boot_id: "boot-id".to_owned(),
        supervisor_pid: pid,
        supervisor_start_identity: Some(format!("linux-proc-ticks:{}", u64::from(pid) * 10)),
        process_group_id: pid,
        capacity,
        claimed_at,
    }
}

/// Capacity is a property of the *device*, not of a worker process: two live workers
/// bound to one render node must not each receive the declared limit (ADR 0049 §6).
/// The VAAPI descriptor carries no `hardware_token` of its own, so this also proves
/// the grouping key is the capability's own `hardware` column for both backends.
#[tokio::test]
async fn vaapi_capacity_is_exhausted_across_workers_sharing_one_device() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::new("transcode_video").unwrap();
    let tickets = ready_transcode_tickets(&cp, 3).await;
    let first = vaapi_worker(&cp, "vaapi-1", &operation, "0000:f4:00.0", 2).await;
    let second = vaapi_worker(&cp, "vaapi-2", &operation, "0000:f4:00.0", 2).await;

    for (ticket, holder) in [(&tickets[0], &first), (&tickets[1], &second)] {
        cp.acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: holder.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    }

    let candidates = cp.workers.operation_candidates(&operation).await.unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.active_leases == 2 && candidate.max_parallel == 2),
        "both workers must observe the device's two held leases: {candidates:?}"
    );

    let before = cp.tickets().get(tickets[2].id).await.unwrap().unwrap();
    let outcome = cp
        .try_acquire_lease(NewLease {
            ticket_id: tickets[2].id,
            worker_id: first.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let LeaseAcquireOutcome::CapacityFull(saturation) = outcome else {
        panic!("a saturated device must refuse a third lease");
    };
    assert_eq!(saturation.active_leases, 2);
    assert_eq!(saturation.max_parallel, 2);
    let after = cp.tickets().get(tickets[2].id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Ready);
    assert_eq!(
        after.attempt, before.attempt,
        "device saturation must not consume an attempt"
    );
}

/// Two render nodes are two independent pools. The acceptance host has one node
/// (spec §10), so this is unit-level rather than a real-media two-device run: a
/// device saturated by its own leases must not make a sibling device unavailable.
#[tokio::test]
async fn vaapi_capacity_never_crosses_devices() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::new("transcode_video").unwrap();
    let tickets = ready_transcode_tickets(&cp, 2).await;
    let busy = vaapi_worker(&cp, "vaapi-busy", &operation, "0000:03:00.0", 1).await;
    let idle = vaapi_worker(&cp, "vaapi-idle", &operation, "0000:f4:00.0", 1).await;

    cp.acquire_lease(NewLease {
        ticket_id: tickets[0].id,
        worker_id: busy.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();

    let candidates = cp.workers.operation_candidates(&operation).await.unwrap();
    let busy_view = candidates
        .iter()
        .find(|candidate| candidate.worker_id == busy.id)
        .unwrap();
    let idle_view = candidates
        .iter()
        .find(|candidate| candidate.worker_id == idle.id)
        .unwrap();
    assert_eq!(busy_view.active_leases, 1);
    assert_eq!(
        idle_view.active_leases, 0,
        "the other device holds no leases of its own"
    );

    cp.acquire_lease(NewLease {
        ticket_id: tickets[1].id,
        worker_id: idle.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();
}

/// After a supervisor's death is proven, recovery retires the old worker and the
/// device must come back to the fleet, or a crashed supervisor would strand its GPU
/// permanently. Recovery takes two durable steps, and this pins both: retiring the
/// owner releases the device claim in the same transaction, so a replacement
/// incarnation can take it immediately, while the dead worker's *held lease* still
/// occupies the device until it is expired. Retirement deliberately does not cancel a
/// lease — a lease is released by completion or by TTL expiry, so a worker that only
/// looked dead cannot have work pulled out from under it.
///
/// Control-plane-owned timestamps use a `ManualClock` starting at `T0`, while lifecycle
/// calls take explicit domain times. The pool runs on real time, so this does not pause tokio.
#[tokio::test]
async fn a_retired_vaapi_owner_releases_its_device_to_a_replacement() {
    let (cp, clock, _tmp) = cp_at_t0().await;
    let operation = TicketOperation::new("transcode_video").unwrap();
    let tickets = ready_transcode_tickets(&cp, 2).await;
    let claims = SqliteAcceleratorClaimRepo::new(cp.pool_for_test().clone());
    let dead = vaapi_worker(&cp, "vaapi-dead", &operation, "0000:f4:00.0", 1).await;
    let dead_claim = claim_for(dead.id, "0000:f4:00.0", 4242, 1, dead.registered_at);
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    claims.claim_in_tx(&mut tx, dead_claim).await.unwrap();
    tx.commit().await.unwrap();
    cp.acquire_lease(NewLease {
        ticket_id: tickets[0].id,
        worker_id: dead.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();

    clock.advance(TDuration::seconds(1));
    let retired_at = clock.now();
    let retired = cp
        .retire_worker(dead.id, dead.epoch, retired_at)
        .await
        .unwrap();
    assert!(dead.registered_at < retired_at);
    assert_eq!(retired.retired_at, Some(retired_at));

    assert!(
        claims
            .get("vaapi:pci-0000:f4:00.0")
            .await
            .unwrap()
            .is_none(),
        "retiring the proven-dead owner must release its device claim"
    );
    let replacement = vaapi_worker(&cp, "vaapi-new", &operation, "0000:f4:00.0", 1).await;
    assert!(retired_at <= replacement.registered_at);

    let worker_events = cp
        .events()
        .list(
            EventFilter {
                subject_type: Some(SubjectType::Worker),
                ..EventFilter::default()
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(
        worker_events
            .items
            .windows(2)
            .all(|events| events[0].envelope.occurred_at <= events[1].envelope.occurred_at)
    );

    let replacement_claim_input = claim_for(replacement.id, "0000:f4:00.0", 5353, 1, clock.now());
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    claims
        .claim_in_tx(&mut tx, replacement_claim_input)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let replacement_claim = claims.get("vaapi:pci-0000:f4:00.0").await.unwrap().unwrap();
    assert!(replacement_claim.claimed_at >= replacement.registered_at);
    assert!(replacement_claim.claimed_at >= retired_at);

    let occupied = active_leases_for(&cp, &operation, replacement.id).await;
    assert_eq!(
        occupied, 1,
        "the dead owner's held lease still occupies the device it was bound to"
    );

    let expired_at = T0 + TDuration::seconds(61);
    cp.expire_due(expired_at).await.unwrap();

    assert_eq!(
        active_leases_for(&cp, &operation, replacement.id).await,
        0,
        "once the stale lease expires the device is free for the replacement"
    );
    cp.acquire_lease(NewLease {
        ticket_id: tickets[1].id,
        worker_id: replacement.id,
        ttl: TDuration::seconds(60),
        now: expired_at,
    })
    .await
    .unwrap();
}

async fn active_leases_for(
    cp: &crate::ControlPlane,
    operation: &TicketOperation,
    worker_id: WorkerId,
) -> u32 {
    cp.workers
        .operation_candidates(operation)
        .await
        .unwrap()
        .iter()
        .find(|candidate| candidate.worker_id == worker_id)
        .unwrap()
        .active_leases
}

#[tokio::test]
async fn acquire_lease_emits_lease_acquired_and_ticket_leased() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 1);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, 1);
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketLeased),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::TicketLeased(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketLeased payload");
    };
    assert_eq!(payload.attempt, 1, "first dispatch bumps attempt to 1");
    assert_eq!(payload.lease_id, lease.id);
}

#[tokio::test]
async fn acquire_lease_rejects_cancelled_job_without_durable_side_effects() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "policy_execute".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(ticket_for_job("noop", 2, job.id))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let ready = cp.tickets().get(ticket.id).await.unwrap().unwrap();
    let worker = eligible_worker(&cp, "cancelled-job-worker", &ticket.kind).await;
    cp.cancel_job(job.id, "operator cancel".to_owned(), T0)
        .await
        .unwrap();
    let lease_events = count(&cp, EventKind::LeaseAcquired).await;
    let ticket_events = count(&cp, EventKind::TicketLeased).await;

    let err = cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    assert_ticket_unchanged(&cp, &ready).await;
    assert!(
        cp.leases()
            .list(LeaseFilter::default(), None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, lease_events);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, ticket_events);
}

#[tokio::test]
async fn cancel_job_does_not_preempt_held_lease() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "policy_execute".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let ticket = cp
        .create_ticket(ticket_for_job("noop", 2, job.id))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let worker = eligible_worker(&cp, "held-job-worker", &ticket.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    cp.cancel_job(job.id, "operator cancel".to_owned(), T0)
        .await
        .unwrap();

    let stored_job = cp.get_job(job.id.0).await.unwrap().unwrap();
    let stored_ticket = cp.tickets().get(ticket.id).await.unwrap().unwrap();
    let stored_lease = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(stored_job.state, JobState::Cancelled);
    assert_eq!(stored_ticket.state, TicketState::Leased);
    assert_eq!(stored_lease.state, LeaseState::Held);
}

async fn assert_ticket_unchanged(cp: &crate::ControlPlane, expected: &Ticket) {
    let actual = cp.tickets().get(expected.id).await.unwrap().unwrap();
    assert_eq!(actual.state, expected.state);
    assert_eq!(actual.attempt, expected.attempt);
    assert_eq!(actual.epoch, expected.epoch);
}

#[tokio::test]
async fn acquire_lease_in_tx_rolls_back_with_caller_transaction() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let mut tx = begin_tx(&cp.pool).await.unwrap();

    let lease = cp
        .acquire_lease_in_tx(
            &mut tx,
            NewLease {
                ticket_id: t.id,
                worker_id: w.id,
                ttl: TDuration::seconds(60),
                now: T0,
            },
        )
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(lease.ticket_id, t.id);
    assert!(
        cp.leases().get(lease.id).await.unwrap().is_none(),
        "helper must leave commit/rollback ownership with the caller"
    );
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 0);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, 0);
}

#[tokio::test]
async fn acquire_lease_rechecks_deny_after_candidate_selection_without_side_effects() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;

    let candidates = cp.workers.operation_candidates(&t.kind).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].worker_id, w.id);

    cp.record_grant(NewGrant {
        worker_id: w.id,
        can_execute: Vec::new(),
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: vec![t.kind.clone()],
        max_parallel: serde_json::json!({}),
    })
    .await
    .unwrap();

    assert!(
        cp.workers
            .operation_candidates(&t.kind)
            .await
            .unwrap()
            .is_empty(),
        "a later deny must remove the worker from candidate selection"
    );
    let before = cp.tickets().get(t.id).await.unwrap().unwrap();
    let err = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::Conflict(ref message) if message.contains("denied")),
        "got {err:?}"
    );

    let after = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Ready);
    assert_eq!(after.attempt, before.attempt);
    assert_eq!(after.epoch, before.epoch);
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(lease_count, 0);
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 0);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, 0);
}

#[tokio::test]
async fn acquire_lease_rechecks_stale_worker_capacity_without_side_effects() {
    let (cp, _tmp) = cp().await;
    let first = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    let second = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(first.id, T0).await.unwrap();
    cp.mark_ready_if_unblocked(second.id, T0).await.unwrap();
    let worker = eligible_worker(&cp, "capacity-stale", &first.kind).await;
    grant_capacity(&cp, &worker, &first.kind, 1).await;

    let candidates = cp.workers.operation_candidates(&first.kind).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].active_leases, 0);

    cp.acquire_lease(NewLease {
        ticket_id: first.id,
        worker_id: worker.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();
    let ready = cp.tickets().get(second.id).await.unwrap().unwrap();

    let err = cp
        .acquire_lease(NewLease {
            ticket_id: second.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap_err();

    assert!(
        matches!(err, VoomError::NoEligibleWorker(ref message) if message.contains("capacity")),
        "got {err:?}"
    );
    assert_ticket_unchanged(&cp, &ready).await;
    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM leases WHERE worker_id = ? AND state = 'held'")
            .bind(i64::try_from(worker.id.0).unwrap())
            .fetch_one(&cp.pool)
            .await
            .unwrap();
    assert_eq!(held, 1);
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 1);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, 1);
}

#[tokio::test]
async fn accelerator_capacity_is_aggregated_by_hardware_token() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::new("transcode_video").unwrap();
    let tickets = [
        cp.create_ticket(ticket(operation.as_str(), 2))
            .await
            .unwrap(),
        cp.create_ticket(ticket(operation.as_str(), 2))
            .await
            .unwrap(),
        cp.create_ticket(ticket(operation.as_str(), 2))
            .await
            .unwrap(),
    ];
    for ticket in &tickets {
        cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    }
    let token = "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let first = nvidia_worker(&cp, "gpu-a-1", &operation, token, 2).await;
    let second = nvidia_worker(&cp, "gpu-a-2", &operation, token, 2).await;

    for (ticket, worker) in [(&tickets[0], &first), (&tickets[1], &second)] {
        cp.acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    }

    let candidates = cp.workers.operation_candidates(&operation).await.unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.active_leases == 2 && candidate.max_parallel == 2)
    );
    let before = cp.tickets().get(tickets[2].id).await.unwrap().unwrap();
    let outcome = cp
        .try_acquire_lease(NewLease {
            ticket_id: tickets[2].id,
            worker_id: first.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let saturation = match outcome {
        LeaseAcquireOutcome::CapacityFull(saturation) => Some(saturation),
        LeaseAcquireOutcome::Acquired(_) => None,
    };
    assert!(saturation.is_some());
    let Some(saturation) = saturation else {
        return;
    };
    assert_eq!(saturation.active_leases, 2);
    assert_eq!(saturation.max_parallel, 2);
    let after = cp.tickets().get(tickets[2].id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Ready);
    assert_eq!(after.attempt, before.attempt);
}

#[tokio::test]
async fn try_acquire_lease_reports_capacity_without_durable_side_effects() {
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(NewJob {
            kind: "capacity-test".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let first = cp
        .create_ticket(ticket_for_job("noop", 2, job.id))
        .await
        .unwrap();
    let second = cp
        .create_ticket(ticket_for_job("noop", 2, job.id))
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(first.id, T0).await.unwrap();
    cp.mark_ready_if_unblocked(second.id, T0).await.unwrap();
    let worker = eligible_worker(&cp, "typed-capacity", &first.kind).await;
    grant_capacity(&cp, &worker, &first.kind, 1).await;
    cp.acquire_lease(NewLease {
        ticket_id: first.id,
        worker_id: worker.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();

    let ticket_before = cp.tickets().get(second.id).await.unwrap().unwrap();
    let job_before = cp.jobs.get(job.id).await.unwrap().unwrap();
    let lease_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let event_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&cp.pool)
        .await
        .unwrap();

    let outcome = cp
        .try_acquire_lease(NewLease {
            ticket_id: second.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let LeaseAcquireOutcome::CapacityFull(saturation) = outcome else {
        panic!("expected typed capacity saturation");
    };
    assert_eq!(saturation.worker_id, worker.id);
    assert_eq!(saturation.operation, first.kind);
    assert_eq!(saturation.active_leases, 1);
    assert_eq!(saturation.max_parallel, 1);
    assert_ticket_unchanged(&cp, &ticket_before).await;
    let job_after = cp.jobs.get(job.id).await.unwrap().unwrap();
    assert_eq!(job_after.state, job_before.state);
    assert_eq!(job_after.epoch, job_before.epoch);
    assert_eq!(job_after.updated_at, job_before.updated_at);
    let lease_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    let event_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(lease_count_after, lease_count_before);
    assert_eq!(event_count_after, event_count_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_local_acquire_never_exceeds_worker_operation_capacity() {
    const ATTEMPTS: usize = 8;

    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::new("noop").unwrap();
    let worker = eligible_worker(&cp, "capacity-concurrent", &operation).await;
    grant_capacity(&cp, &worker, &operation, 1).await;
    let mut tickets = Vec::with_capacity(ATTEMPTS);
    for _ in 0..ATTEMPTS {
        let ticket = cp.create_ticket(ticket("noop", 2)).await.unwrap();
        cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
        tickets.push(ticket);
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(ATTEMPTS));
    let mut handles = Vec::with_capacity(ATTEMPTS);
    for ticket in &tickets {
        let cp = cp.clone();
        let barrier = barrier.clone();
        let input = NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        };
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            cp.acquire_lease(input).await
        }));
    }

    let mut acquired = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => acquired += 1,
            Err(VoomError::NoEligibleWorker(message)) => {
                assert!(message.contains("capacity"), "got {message}");
            }
            Err(error) => panic!("unexpected concurrent acquire error: {error:?}"),
        }
    }

    assert_eq!(acquired, 1);
    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM leases WHERE worker_id = ? AND state = 'held'")
            .bind(i64::try_from(worker.id.0).unwrap())
            .fetch_one(&cp.pool)
            .await
            .unwrap();
    assert_eq!(held, 1);
    let states: Vec<(String, i64)> =
        sqlx::query_as("SELECT state, attempt FROM tickets ORDER BY id")
            .fetch_all(&cp.pool)
            .await
            .unwrap();
    assert_eq!(
        states
            .iter()
            .filter(|(state, attempt)| state == "leased" && *attempt == 1)
            .count(),
        1
    );
    assert_eq!(
        states
            .iter()
            .filter(|(state, attempt)| state == "ready" && *attempt == 0)
            .count(),
        ATTEMPTS - 1
    );
    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 1);
    assert_eq!(count(&cp, EventKind::TicketLeased).await, 1);
}

#[tokio::test]
async fn worker_capacity_counts_normalized_operation_not_unrelated_leases() {
    let (cp, _tmp) = cp().await;
    let remux = TicketOperation::new("remux").unwrap();
    let probe = TicketOperation::new("probe_file").unwrap();
    let worker = eligible_worker(&cp, "capacity-by-operation", &remux).await;
    grant_capacity(&cp, &worker, &remux, 2).await;
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: probe.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: serde_json::json!({}),
    })
    .await
    .unwrap();
    grant_capacity(&cp, &worker, &probe, 1).await;
    let workflow_remux = cp
        .create_ticket(ticket("synthetic.workflow.operation.remux", 2))
        .await
        .unwrap();
    let probe_ticket = cp.create_ticket(ticket("probe_file", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(workflow_remux.id, T0)
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(probe_ticket.id, T0)
        .await
        .unwrap();
    cp.acquire_lease(NewLease {
        ticket_id: workflow_remux.id,
        worker_id: worker.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();
    cp.acquire_lease(NewLease {
        ticket_id: probe_ticket.id,
        worker_id: worker.id,
        ttl: TDuration::seconds(60),
        now: T0,
    })
    .await
    .unwrap();

    let candidates = cp.workers.operation_candidates(&remux).await.unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].active_leases, 1);
    assert_eq!(candidates[0].max_parallel, 2);
}

#[tokio::test]
async fn release_lease_emits_lease_released_and_ticket_succeeded() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.release_lease(lease.id, serde_json::json!({}), T0 + TDuration::seconds(5))
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketSucceeded).await, 1);
}

#[tokio::test]
async fn fail_lease_retriable_emits_lease_released_and_ticket_failed_retriable() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 3)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        lease.id,
        "transient".to_owned(),
        FailureClass::WorkerTimeout,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedRetriable).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn fail_lease_terminal_emits_lease_released_and_ticket_failed_terminal() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // max_attempts=1: a single retriable failure exhausts the budget,
    // so the case handler emits TicketFailedTerminal even though the
    // class is retriable. Reuses the same call shape as the retriable
    // happy path.
    cp.fail_lease(
        lease.id,
        "fatal".to_owned(),
        FailureClass::WorkerTimeout,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedRetriable).await, 0);
}

#[tokio::test]
async fn expire_due_emits_paired_events_requeued() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 3)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let _lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(30),
            now: T0,
        })
        .await
        .unwrap();
    let report = cp.expire_due(T0 + TDuration::seconds(60)).await.unwrap();
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.requeued_tickets, vec![t.id]);
    assert!(report.failed_expiries.is_empty());
    assert_eq!(count(&cp, EventKind::LeaseExpired).await, 1);
    assert_eq!(
        count(&cp, EventKind::TicketRequeuedAfterLeaseExpiry).await,
        1
    );
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn expire_due_emits_paired_events_terminal() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let _lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(30),
            now: T0,
        })
        .await
        .unwrap();
    let report = cp.expire_due(T0 + TDuration::seconds(60)).await.unwrap();
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.failed_expiries.len(), 1);
    assert_eq!(report.failed_expiries[0].ticket_id, t.id);
    assert_eq!(count(&cp, EventKind::LeaseExpired).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketFailedTerminal),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let voom_events::Event::TicketFailedTerminal(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketFailedTerminal payload");
    };
    assert!(payload.reason.contains("lease expired"));
    let _: TicketId = t.id;
}

#[tokio::test]
async fn force_release_with_requeue_emits_ticket_requeued_after_force_release_when_attempts_remain()
{
    // max_attempts=2: after acquire, attempts remain (1 < 2).
    // also_requeue=true → ticket back to ready, and the case handler
    // emits TicketRequeuedAfterForceRelease (not TicketReady — the
    // distinct kind lets audit tell operator-driven requeue apart from
    // dependency-driven readiness).
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let ready_before = count(&cp, EventKind::TicketReady).await;
    let outcome = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            true,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap();
    assert!(outcome.ticket_requeued);
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, 1);
    assert_eq!(
        count(&cp, EventKind::TicketRequeuedAfterForceRelease).await,
        1
    );
    assert_eq!(
        count(&cp, EventKind::TicketReady).await,
        ready_before,
        "force-release uses the dedicated event kind, not TicketReady"
    );
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 0);
}

#[tokio::test]
async fn force_release_with_requeue_rejects_when_attempts_exhausted() {
    // §13 stranding regression. max_attempts=1: acquire consumes the
    // only attempt. Operator asks for requeue, but no attempts remain.
    // The repo now returns VoomError::Conflict with NO side effects on
    // the lease, ticket, or event log — the caller must retry with
    // also_requeue=false if they intend a terminal force-release.
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let force_released_before = count(&cp, EventKind::LeaseForceReleased).await;
    let requeued_before = count(&cp, EventKind::TicketRequeuedAfterForceRelease).await;
    let terminal_before = count(&cp, EventKind::TicketFailedTerminal).await;
    let err = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            true,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    // No side effects: lease still held, ticket still leased, no events.
    let lease_after = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(
        lease_after.state,
        voom_store::repo::execution::leases::LeaseState::Held,
        "rejected force_release must leave the lease held"
    );
    let ticket_after = cp.tickets().get(t.id).await.unwrap().unwrap();
    assert_eq!(
        ticket_after.state,
        TicketState::Leased,
        "rejected force_release must leave the ticket leased"
    );
    assert_eq!(
        count(&cp, EventKind::LeaseForceReleased).await,
        force_released_before
    );
    assert_eq!(
        count(&cp, EventKind::TicketRequeuedAfterForceRelease).await,
        requeued_before
    );
    assert_eq!(
        count(&cp, EventKind::TicketFailedTerminal).await,
        terminal_before
    );
    // The same fixture with also_requeue=false succeeds: lease force-released,
    // ticket parked in failed, single LeaseForceReleased + single
    // TicketFailedTerminal event.
    let outcome = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "manual cleanup".to_owned(),
            false,
            T0 + TDuration::seconds(6),
        )
        .await
        .unwrap();
    assert!(!outcome.ticket_requeued);
    assert_eq!(
        count(&cp, EventKind::LeaseForceReleased).await,
        force_released_before + 1
    );
    assert_eq!(
        count(&cp, EventKind::TicketFailedTerminal).await,
        terminal_before + 1
    );
    let _: TicketId = t.id;
}

#[tokio::test]
async fn release_lease_promotes_dependent_and_emits_ticket_ready() {
    // parent -> child. Releasing parent must promote child to ready and
    // emit exactly one ticket.ready for child.id.
    let (cp, _tmp) = cp().await;
    let parent = cp.create_ticket(ticket("parent", 1)).await.unwrap();
    let child = cp.create_ticket(ticket("child", 1)).await.unwrap();
    cp.tickets()
        .add_dependency(child.id, parent.id)
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(parent.id, T0).await.unwrap();
    // child cannot promote yet — parent is not succeeded.
    let none = cp.mark_ready_if_unblocked(child.id, T0).await.unwrap();
    assert!(none.is_empty(), "child must stay pending while parent runs");

    let w = eligible_worker(&cp, "alpha", &parent.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: parent.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let ready_before = count(&cp, EventKind::TicketReady).await;
    cp.release_lease(lease.id, serde_json::json!({}), T0 + TDuration::seconds(5))
        .await
        .unwrap();

    let child_after = cp.tickets().get(child.id).await.unwrap().unwrap();
    assert_eq!(
        child_after.state,
        TicketState::Ready,
        "child must be promoted to ready when parent succeeds"
    );
    assert_eq!(
        count(&cp, EventKind::TicketReady).await,
        ready_before + 1,
        "exactly one ticket.ready emitted for the promoted child"
    );

    // Verify the emitted ticket.ready payload references the child.
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketReady),
                subject_id: Some(child.id.0),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "exactly one ticket.ready for child");
    let voom_events::Event::TicketReady(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketReady payload");
    };
    assert_eq!(payload.ticket_id, child.id);
}

#[tokio::test]
async fn release_lease_does_not_promote_child_with_outstanding_parent() {
    // Diamond: child depends on parent_a AND parent_b. Releasing parent_a
    // alone must not promote child (parent_b still leased), so no
    // ticket.ready is emitted for child.
    let (cp, _tmp) = cp().await;
    let parent_a = cp.create_ticket(ticket("parent_a", 1)).await.unwrap();
    let parent_b = cp.create_ticket(ticket("parent_b", 1)).await.unwrap();
    let child = cp.create_ticket(ticket("child", 1)).await.unwrap();
    cp.tickets()
        .add_dependency(child.id, parent_a.id)
        .await
        .unwrap();
    cp.tickets()
        .add_dependency(child.id, parent_b.id)
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(parent_a.id, T0).await.unwrap();
    cp.mark_ready_if_unblocked(parent_b.id, T0).await.unwrap();

    let w = eligible_worker(&cp, "alpha", &parent_a.kind).await;
    cp.record_capability(NewCapability {
        worker_id: w.id,
        operation: parent_b.kind.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: serde_json::json!({}),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: w.id,
        can_execute: vec![parent_b.kind.clone()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: serde_json::json!({}),
    })
    .await
    .unwrap();
    let lease_a = cp
        .acquire_lease(NewLease {
            ticket_id: parent_a.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // parent_b is also leased so it cannot succeed.
    let _lease_b = cp
        .acquire_lease(NewLease {
            ticket_id: parent_b.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let ready_before = count(&cp, EventKind::TicketReady).await;
    cp.release_lease(
        lease_a.id,
        serde_json::json!({}),
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();

    let child_after = cp.tickets().get(child.id).await.unwrap().unwrap();
    assert_eq!(
        child_after.state,
        TicketState::Pending,
        "child must stay pending while a parent is still outstanding"
    );
    assert_eq!(
        count(&cp, EventKind::TicketReady).await,
        ready_before,
        "no ticket.ready when a dependent remains blocked"
    );
}

#[tokio::test]
async fn force_release_without_requeue_emits_lease_force_released_and_ticket_failed_terminal() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.force_release_lease(
        lease.id,
        "operator".to_owned(),
        "manual cleanup".to_owned(),
        false,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
}

#[tokio::test]
async fn force_release_lease_rejects_empty_actor() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let before = count(&cp, EventKind::LeaseForceReleased).await;
    let err = cp
        .force_release_lease(
            lease.id,
            String::new(),
            "manual cleanup".to_owned(),
            false,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    // Validation runs before the tx — the lease must still be held and
    // no audit event row must have been written.
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, before);
    let still = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(
        still.state,
        voom_store::repo::execution::leases::LeaseState::Held
    );
}

#[tokio::test]
async fn force_release_lease_rejects_whitespace_reason() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let before = count(&cp, EventKind::LeaseForceReleased).await;
    let err = cp
        .force_release_lease(
            lease.id,
            "operator".to_owned(),
            "   \t\n".to_owned(),
            false,
            T0 + TDuration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Config(_)), "got {err:?}");
    assert_eq!(count(&cp, EventKind::LeaseForceReleased).await, before);
    let still = cp.leases().get(lease.id).await.unwrap().unwrap();
    assert_eq!(
        still.state,
        voom_store::repo::execution::leases::LeaseState::Held
    );
}

// --- terminal_failure issue auto-open (Issue Model + Failure taxonomy) -------

/// The single `TicketFailedTerminal` event's `issue_id`. Panics if the store
/// holds a number other than one terminal event.
async fn only_terminal_issue_id(cp: &crate::ControlPlane) -> Option<voom_core::IssueId> {
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::TicketFailedTerminal),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "expected exactly one terminal event");
    let voom_events::Event::TicketFailedTerminal(payload) = &page.items[0].envelope.payload else {
        panic!("expected TicketFailedTerminal payload");
    };
    payload.issue_id
}

fn expected_issue_id(row_id: i64) -> voom_core::IssueId {
    voom_core::IssueId(u64::try_from(row_id).unwrap())
}

/// Retries exhausted on a retriable class: one `terminal_failure` issue at the
/// taxonomy's medium/normal defaults, linked to both the ticket and its last
/// lease, with its id stamped on the payload.
#[tokio::test]
async fn fail_lease_terminal_opens_retriable_exhausted_issue_linked_to_ticket_and_lease() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        lease.id,
        "fatal".to_owned(),
        FailureClass::WorkerTimeout,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();

    let issues = terminal_failure_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.severity, "medium");
    assert_eq!(issue.priority, "normal");
    assert_eq!(issue.priority_source, "system");
    assert_eq!(issue.status, "open");
    assert_eq!(
        only_terminal_issue_id(&cp).await,
        Some(expected_issue_id(issue.id))
    );
    assert_eq!(
        issue_link_targets(&cp, issue.id).await,
        vec![
            ("lease".to_owned(), i64::try_from(lease.id.0).unwrap()),
            ("ticket".to_owned(), i64::try_from(t.id.0).unwrap()),
        ]
    );
}

/// A non-retriable class transitions terminally on the first failure even with
/// attempts remaining, and opens a high/high issue.
#[tokio::test]
async fn fail_lease_non_retriable_opens_high_severity_terminal_failure_issue() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 3)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        lease.id,
        "bad worker output".to_owned(),
        FailureClass::MalformedWorkerResult,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();

    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
    assert_eq!(count(&cp, EventKind::TicketFailedRetriable).await, 0);
    let issues = terminal_failure_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].severity, "high");
    assert_eq!(issues[0].priority, "high");
    assert_eq!(
        only_terminal_issue_id(&cp).await,
        Some(expected_issue_id(issues[0].id))
    );
}

/// An operator-required class is terminal on the first failure and opens a
/// high/high issue.
#[tokio::test]
async fn fail_lease_operator_required_opens_high_severity_terminal_failure_issue() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 3)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.fail_lease(
        lease.id,
        "needs operator approval".to_owned(),
        FailureClass::ApprovalRequired,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();

    assert_eq!(count(&cp, EventKind::TicketFailedTerminal).await, 1);
    let issues = terminal_failure_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].severity, "high");
    assert_eq!(issues[0].priority, "high");
    assert_eq!(
        only_terminal_issue_id(&cp).await,
        Some(expected_issue_id(issues[0].id))
    );
}

/// Lease expiry with no retries remaining is a terminal transition too: it
/// opens a `WorkerCrash` (retriable) issue linked to the ticket and lease.
#[tokio::test]
async fn expire_due_terminal_opens_issue_linked_to_ticket_and_lease() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 1)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(30),
            now: T0,
        })
        .await
        .unwrap();
    cp.expire_due(T0 + TDuration::seconds(60)).await.unwrap();

    let issues = terminal_failure_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(
        issue.severity, "medium",
        "WorkerCrash is retriable => medium"
    );
    assert_eq!(issue.priority, "normal");
    assert_eq!(
        only_terminal_issue_id(&cp).await,
        Some(expected_issue_id(issue.id))
    );
    assert_eq!(
        issue_link_targets(&cp, issue.id).await,
        vec![
            ("lease".to_owned(), i64::try_from(lease.id.0).unwrap()),
            ("ticket".to_owned(), i64::try_from(t.id.0).unwrap()),
        ]
    );
}

/// Operator force-release without requeue is a `UserCancellation` (non-retriable)
/// terminal transition that opens a high/high issue.
#[tokio::test]
async fn force_release_without_requeue_opens_terminal_failure_issue() {
    let (cp, _tmp) = cp().await;
    let t = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = eligible_worker(&cp, "alpha", &t.kind).await;
    let lease = cp
        .acquire_lease(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: TDuration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    cp.force_release_lease(
        lease.id,
        "operator".to_owned(),
        "manual cleanup".to_owned(),
        false,
        T0 + TDuration::seconds(5),
    )
    .await
    .unwrap();

    let issues = terminal_failure_issues(&cp).await;
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.severity, "high");
    assert_eq!(issue.priority, "high");
    assert_eq!(
        only_terminal_issue_id(&cp).await,
        Some(expected_issue_id(issue.id))
    );
    assert_eq!(
        issue_link_targets(&cp, issue.id).await,
        vec![
            ("lease".to_owned(), i64::try_from(lease.id.0).unwrap()),
            ("ticket".to_owned(), i64::try_from(t.id.0).unwrap()),
        ]
    );
}
