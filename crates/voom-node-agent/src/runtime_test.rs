use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use rand::{RngCore, TryRngCore};
use secrecy::SecretString;
#[cfg(unix)]
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, oneshot};
use voom_core::{
    ArtifactAccessMode, LeaseId, NodeId, NodeIncarnationStatus, OperationKind, TicketId, WorkerId,
    ids::{ArtifactCommitIntentId, ArtifactCommitRecordId},
};
use voom_worker_protocol::{
    DispatchStream, HandshakeResponse, HttpServer, NdjsonReader, OperationFuture, OperationHandler,
    OperationResponse, ProtocolError, ServerHandle, ServerRunning, VaapiVideoAcceleratorDescriptor,
    VideoAcceleratorDescriptor, WorkerCredentials, WorkerIdentityResponse,
};

use super::*;
use crate::client::{
    AcquireIdle, ArtifactAccessPlan, CommitApplyingOutcome, CommitApplyingRequest,
    CommitAuthorizeOutcome, CommitAuthorizeRequest, CommitCompleteOutcome, CommitCompleteRequest,
    CommitOpenOutcome, CommitOpenRequest, CommitOutcomeRequest, CommitReceiptOutcome,
    CompleteOutcome, DeactivateOutcome, FailOutcome, LeaseHeartbeatOutcome, NodeHeartbeatOutcome,
};
use crate::config::{AgentConfig, TokenSource};

#[test]
fn shutdown_signal_phase_matches_original_exit() {
    assert_eq!(
        signal_phase_for_exit(&RuntimeExit::Graceful),
        ShutdownSignalPhase::ForceEnabled
    );
    assert_eq!(
        signal_phase_for_exit(&RuntimeExit::Fatal(RuntimeFatal::Internal(
            "fatal".to_owned()
        ))),
        ShutdownSignalPhase::AwaitingFirst
    );
    assert_eq!(
        signal_phase_for_exit(&RuntimeExit::RestartExhausted),
        ShutdownSignalPhase::AwaitingFirst
    );
}

#[tokio::test]
async fn fatal_reaping_requires_a_genuine_second_signal() {
    let (release_tx, release_rx) = oneshot::channel();
    let mut coordinators = JoinSet::new();
    coordinators.spawn(async move {
        let _ = release_rx.await;
        CoordinatorExit::Shutdown(LeaseSettlement::Completed)
    });
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Fenced);
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    signal_tx.send(()).unwrap();

    let reaping = wait_for_coordinators(
        &mut coordinators,
        &shutdown_tx,
        &mut signal_rx,
        ShutdownSignalPhase::AwaitingFirst,
    );
    tokio::pin!(reaping);

    assert!(
        tokio::time::timeout(Duration::from_millis(50), async {
            tokio::select! {
                result = &mut reaping => {
                    assert!(result.is_err(), "blocked coordinator reaped early: {result:?}");
                    Ok(())
                }
                changed = shutdown_rx.changed() => changed,
            }
        })
        .await
        .is_err(),
        "a buffered first signal must not force fatal settlement"
    );
    assert_eq!(*shutdown_rx.borrow(), ShutdownKind::Fenced);

    signal_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut reaping => {
                assert!(result.is_err(), "blocked coordinator reaped early: {result:?}");
                Ok(())
            }
            changed = shutdown_rx.changed() => changed,
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(*shutdown_rx.borrow(), ShutdownKind::Forced);
    release_tx.send(()).unwrap();

    let progress = reaping.await.unwrap();
    assert_eq!(progress.signal_phase, ShutdownSignalPhase::ForceEnabled);
    assert_eq!(progress.forced, Some(ShutdownForce::Signal));
}

#[tokio::test]
async fn graceful_reaping_forces_on_its_next_signal() {
    let (release_tx, release_rx) = oneshot::channel();
    let mut coordinators = JoinSet::new();
    coordinators.spawn(async move {
        let _ = release_rx.await;
        CoordinatorExit::Shutdown(LeaseSettlement::Completed)
    });
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::User);
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();

    let reaping = tokio::spawn(async move {
        wait_for_coordinators(
            &mut coordinators,
            &shutdown_tx,
            &mut signal_rx,
            ShutdownSignalPhase::ForceEnabled,
        )
        .await
    });
    signal_tx.send(()).unwrap();

    tokio::time::timeout(Duration::from_secs(5), shutdown_rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*shutdown_rx.borrow(), ShutdownKind::Forced);
    release_tx.send(()).unwrap();

    let progress = reaping.await.unwrap().unwrap();
    assert_eq!(progress.signal_phase, ShutdownSignalPhase::ForceEnabled);
    assert_eq!(progress.forced, Some(ShutdownForce::Signal));
}

#[test]
fn validation_accepts_only_responses_before_half_ttl() {
    let ttl = Duration::from_secs(30);
    assert!(is_validation_fresh(Duration::from_millis(14_999), ttl));
    assert!(!is_validation_fresh(Duration::from_secs(15), ttl));
    assert!(!is_validation_fresh(Duration::from_secs(16), ttl));
}

#[test]
fn granted_lease_ttl_normalizes_nonpositive_grants_to_one_second() {
    assert_eq!(granted_lease_ttl(0), Duration::from_secs(1));
    assert_eq!(granted_lease_ttl(-1), Duration::from_secs(1));
    assert_eq!(granted_lease_ttl(20), Duration::from_secs(20));
}

#[test]
fn acquisition_poll_schedule_is_centered_across_full_duration_range() {
    let bases = [
        Duration::from_secs(20),
        Duration::from_secs(u64::MAX / 1_000_000_000 + 1),
    ];
    for base in bases {
        let mut rng = StdRng::seed_from_u64(459);
        let samples = (0..128)
            .map(|_| acquisition_poll_delay(base, &mut rng))
            .collect::<Vec<_>>();
        let lower = base / 2;
        let upper = base.saturating_add(lower);
        assert!(samples.iter().all(|delay| *delay >= lower));
        assert!(samples.iter().all(|delay| *delay <= upper));
        assert!(samples.iter().any(|delay| *delay < base));
        assert!(samples.iter().any(|delay| *delay > base));
    }
}

#[test]
fn acquisition_poll_task_streams_consume_distinct_parent_output() {
    let mut master = StdRng::seed_from_u64(459);
    let mut first = derive_schedule_rng(&mut master);
    let mut second = derive_schedule_rng(&mut master);

    let first = (0..128).map(|_| first.next_u64()).collect::<Vec<_>>();
    let second = (0..128).map(|_| second.next_u64()).collect::<Vec<_>>();

    assert_ne!(first, second);
}

#[tokio::test]
async fn acquisition_poll_entropy_failure_precedes_activation() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (_signal_tx, signal_rx) = mpsc::unbounded_channel();
    let mut source = FailingRng;

    let error = runtime
        .run_with_shutdowns_from_rng(signal_rx, &mut source)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("seed node-agent schedule RNG"));
    assert_eq!(control.activate_started_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn activation_forwards_the_configured_accelerator_descriptor() {
    let control = Arc::new(FakeControlPlane::default());
    let mut configured_worker = worker();
    configured_worker.accelerator = Some(VideoAcceleratorDescriptor::Vaapi(
        VaapiVideoAcceleratorDescriptor {
            pci_address: "0000:f4:00.0".to_owned(),
            device_name: "Radeon Pro".to_owned(),
            driver_version: "Mesa 26.1".to_owned(),
            encoders: vec!["hevc_vaapi".to_owned()],
            decoders: vec!["hevc".to_owned()],
            max_sessions: 2,
        },
    ));
    let runtime = AgentRuntime::with_client(
        loaded_config_with_worker(configured_worker),
        control.clone(),
    );

    runtime.activate(incarnation()).await.unwrap();

    let bodies = control.activation_bodies.lock().await;
    assert_eq!(bodies[0]["workers"][0]["accelerator"]["backend"], "vaapi");
    assert_eq!(
        bodies[0]["workers"][0]["accelerator"]["pci_address"],
        "0000:f4:00.0"
    );
}

#[tokio::test(start_paused = true)]
async fn acquisition_poll_resamples_each_production_wait() {
    let interval = Duration::from_secs(20);
    let mut expected_rng = StdRng::seed_from_u64(459);
    let first_delay = acquisition_poll_delay(interval, &mut expected_rng);
    let second_delay = acquisition_poll_delay(interval, &mut expected_rng);
    assert_ne!(first_delay, second_delay);

    let mut actual_rng = StdRng::seed_from_u64(459);
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    for expected in [first_delay, second_delay] {
        let started = tokio::time::Instant::now();
        let event = acquisition_poll_event(
            interval,
            &mut actual_rng,
            &mut shutdown_rx,
            std::future::pending::<Result<(), crate::child::ChildError>>(),
        );
        tokio::pin!(event);
        assert_future_pending(event.as_mut());
        let before_deadline = expected
            .checked_sub(Duration::from_nanos(1))
            .unwrap_or(Duration::ZERO);
        tokio::time::advance(before_deadline).await;
        assert_future_pending(event.as_mut());
        tokio::time::advance(Duration::from_nanos(1)).await;
        assert!(matches!(event.await, CoordinatorEvent::PollElapsed));
        assert!(started.elapsed() >= expected);
        assert!(started.elapsed() <= expected + Duration::from_millis(2));
    }
}

#[tokio::test(start_paused = true)]
async fn acquisition_poll_shutdown_interrupts_the_upper_bound() {
    let interval = Duration::from_secs(20);
    let upper = interval + interval / 2;
    let started = tokio::time::Instant::now();
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let waiting = wait_for_acquisition_poll(
        upper,
        &mut shutdown_rx,
        std::future::pending::<Result<(), crate::child::ChildError>>(),
    );
    tokio::pin!(waiting);
    assert_future_pending(waiting.as_mut());

    shutdown_tx.send(ShutdownKind::User).unwrap();

    assert!(matches!(
        waiting.await,
        CoordinatorEvent::Shutdown(ShutdownKind::User)
    ));
    assert!(started.elapsed() < upper);
}

#[tokio::test(start_paused = true)]
async fn acquisition_poll_child_exit_interrupts_the_upper_bound() {
    let interval = Duration::from_secs(20);
    let upper = interval + interval / 2;
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let waiting = wait_for_acquisition_poll(upper, &mut shutdown_rx, std::future::ready(Ok(())));

    assert!(matches!(waiting.await, CoordinatorEvent::ChildExit(Ok(()))));
}

#[derive(Debug)]
struct FailingRng;

fn assert_future_pending<F>(mut future: Pin<&mut F>)
where
    F: Future,
{
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
}

impl TryRngCore for FailingRng {
    type Error = std::io::Error;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Err(std::io::Error::other("entropy unavailable"))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Err(std::io::Error::other("entropy unavailable"))
    }

    fn try_fill_bytes(&mut self, _destination: &mut [u8]) -> Result<(), Self::Error> {
        Err(std::io::Error::other("entropy unavailable"))
    }
}

#[test]
fn node_heartbeat_schedule_stays_centered_inside_the_effective_ttl() {
    let interval = Duration::from_secs(20);
    let ttl = Duration::from_mins(1);
    let mut rng = StdRng::seed_from_u64(459);
    let samples = (0..128)
        .map(|_| node_heartbeat_delay(interval, &mut rng))
        .collect::<Vec<_>>();

    assert!(samples.iter().all(|delay| *delay >= interval / 2));
    assert!(
        samples
            .iter()
            .all(|delay| *delay <= interval + interval / 2)
    );
    assert!(samples.iter().all(|delay| *delay < ttl));
}

#[tokio::test(start_paused = true)]
async fn node_heartbeat_schedule_resamples_the_production_loop() {
    let control = Arc::new(FakeControlPlane::default());
    let client: Arc<dyn ControlPlaneApi> = control.clone();
    let interval = Duration::from_secs(20);
    let ttl = Duration::from_mins(1);
    let mut expected_rng = StdRng::seed_from_u64(459);
    let expected = [
        node_heartbeat_delay(interval, &mut expected_rng),
        node_heartbeat_delay(interval, &mut expected_rng),
    ];
    assert_ne!(expected[0], expected[1]);
    let (stop_tx, stop_rx) = watch::channel(false);
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
    let running = node_heartbeat_loop(
        client,
        NodeId(7),
        incarnation(),
        NodeHeartbeatTiming { interval, ttl },
        fatal_tx,
        stop_rx,
        StdRng::seed_from_u64(459),
    );
    tokio::pin!(running);
    assert_future_pending(running.as_mut());

    for (index, delay) in expected.into_iter().enumerate() {
        let before_deadline = delay
            .checked_sub(Duration::from_millis(2))
            .unwrap_or(Duration::ZERO);
        tokio::time::advance(before_deadline).await;
        assert_future_pending(running.as_mut());
        assert_eq!(control.node_heartbeats.load(Ordering::SeqCst), index);
        tokio::time::advance(Duration::from_millis(4)).await;
        assert_future_pending(running.as_mut());
        assert_eq!(control.node_heartbeats.load(Ordering::SeqCst), index + 1);
    }

    stop_tx.send(true).unwrap();
    running.await;
    assert!(fatal_rx.try_recv().is_err());
}

#[test]
fn lease_heartbeat_schedule_preserves_coherent_and_incoherent_grants() {
    let interval = Duration::from_secs(3);
    let ttl = Duration::from_secs(10);
    let mut rng = StdRng::seed_from_u64(459);
    let samples = (0..128)
        .map(|_| lease_heartbeat_delay(interval, ttl, &mut rng))
        .collect::<Vec<_>>();

    assert!(samples.iter().all(|delay| *delay >= interval / 2));
    assert!(
        samples
            .iter()
            .all(|delay| *delay <= interval + interval / 2)
    );
    assert!(samples.iter().all(|delay| *delay < ttl));

    let boundary_interval = Duration::from_secs(4);
    let boundary_ttl = Duration::from_secs(6);
    let mut boundary_rng = StdRng::seed_from_u64(459);
    let mut untouched_rng = boundary_rng.clone();
    assert_eq!(
        lease_heartbeat_delay(boundary_interval, boundary_ttl, &mut boundary_rng),
        boundary_interval
    );
    assert_eq!(
        centered_jitter(interval, &mut boundary_rng),
        centered_jitter(interval, &mut untouched_rng),
        "an equality-boundary grant must not consume schedule entropy"
    );

    let incoherent = Duration::from_secs(8);
    assert_eq!(
        lease_heartbeat_delay(incoherent, Duration::from_secs(6), &mut rng),
        incoherent
    );
}

#[tokio::test(start_paused = true)]
async fn lease_heartbeat_schedule_resamples_the_production_loop() {
    let control = Arc::new(FakeControlPlane::default());
    let interval = Duration::from_secs(3);
    let ttl = Duration::from_secs(10);
    let mut expected_rng = StdRng::seed_from_u64(459);
    let expected = [
        lease_heartbeat_delay(interval, ttl, &mut expected_rng),
        lease_heartbeat_delay(interval, ttl, &mut expected_rng),
    ];
    assert_ne!(expected[0], expected[1]);
    let (stop_tx, stop_rx) = watch::channel(false);
    let (fence_tx, mut fence_rx) = mpsc::channel(1);
    let running = lease_heartbeat_loop(
        context(control.clone()),
        LeaseId(1),
        3,
        10,
        stop_rx,
        fence_tx,
        StdRng::seed_from_u64(459),
    );
    tokio::pin!(running);
    assert_future_pending(running.as_mut());

    for (index, delay) in expected.into_iter().enumerate() {
        let before_deadline = delay
            .checked_sub(Duration::from_millis(2))
            .unwrap_or(Duration::ZERO);
        tokio::time::advance(before_deadline).await;
        assert_future_pending(running.as_mut());
        assert_eq!(control.lease_heartbeats.load(Ordering::SeqCst), index);
        tokio::time::advance(Duration::from_millis(4)).await;
        assert_future_pending(running.as_mut());
        assert_eq!(control.lease_heartbeats.load(Ordering::SeqCst), index + 1);
    }

    stop_tx.send(true).unwrap();
    running.await;
    assert!(fence_rx.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn node_heartbeat_continues_during_delayed_child_startup() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
    let heartbeat = runtime
        .start_node_heartbeat(incarnation(), 6, fatal_tx, StdRng::seed_from_u64(459))
        .await
        .unwrap();

    let delayed_startup = tokio::spawn(tokio::time::sleep(Duration::from_secs(20)));
    advance_seconds(7).await;

    assert!(!delayed_startup.is_finished());
    // The initial heartbeat plus two worst-case 3-second jittered intervals must land while
    // child startup remains blocked. Exact resampling timing is covered by the schedule test.
    assert!(control.node_heartbeats.load(Ordering::SeqCst) >= 3);
    assert!(fatal_rx.try_recv().is_err());
    delayed_startup.abort();
    heartbeat.stop();
}

#[tokio::test(start_paused = true)]
async fn silent_dispatch_keeps_lease_heartbeat_until_progress_timeout_is_settled() {
    let control = Arc::new(FakeControlPlane::default());
    let worker = Arc::new(FakeWorker::new(WorkerMode::Silent));
    let permits = Arc::new(Semaphore::new(1));
    let held = held_lease(Arc::clone(&permits)).await;
    let (_worker_cancel_tx, worker_cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let task = tokio::spawn(run_lease(
        held,
        worker,
        credentials(),
        context(control.clone()),
        worker_cancel_rx,
        shutdown_rx,
        StdRng::seed_from_u64(459),
    ));

    tokio::time::advance(Duration::from_secs(6)).await;
    task.await.unwrap();

    assert!(control.lease_heartbeats.load(Ordering::SeqCst) >= 4);
    let failures = control.failures.lock().await;
    assert_eq!(failures.as_slice(), &[FailureClass::ProgressTimeout]);
    assert!(permits.try_acquire_owned().is_ok());
}

#[tokio::test(start_paused = true)]
async fn terminal_retry_retains_heartbeat_and_parallelism_permit() {
    let control = Arc::new(FakeControlPlane::default());
    let gate = Arc::new(Notify::new());
    *control.fail_gate.lock().await = Some(Arc::clone(&gate));
    let worker = Arc::new(FakeWorker::new(WorkerMode::Error));
    let permits = Arc::new(Semaphore::new(1));
    let held = held_lease(Arc::clone(&permits)).await;
    let (_worker_cancel_tx, worker_cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let task = tokio::spawn(run_lease(
        held,
        worker,
        credentials(),
        context(control.clone()),
        worker_cancel_rx,
        shutdown_rx,
        StdRng::seed_from_u64(459),
    ));

    wait_for_count(&control.fail_started, &control.fail_started_count, 1).await;
    assert!(Arc::clone(&permits).try_acquire_owned().is_err());
    advance_seconds(3).await;
    assert!(control.lease_heartbeats.load(Ordering::SeqCst) >= 2);
    assert!(Arc::clone(&permits).try_acquire_owned().is_err());

    gate.notify_waiters();
    task.await.unwrap();
    assert!(permits.try_acquire_owned().is_ok());
}

#[tokio::test]
async fn rejected_completion_fails_the_held_lease_as_a_malformed_result() {
    let control = Arc::new(FakeControlPlane::default());
    control.reject_complete.store(true, Ordering::SeqCst);

    settle_lease(
        &context(control.clone()),
        &dispatch(json!({})),
        LeaseOutcome::Complete(json!({"invalid": "result"})),
    )
    .await
    .unwrap();

    assert_eq!(control.complete_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        control.failures.lock().await.as_slice(),
        &[FailureClass::MalformedWorkerResult]
    );
}

#[tokio::test(start_paused = true)]
async fn delayed_validation_rotates_key_and_terminal_replay_never_dispatches() {
    let control = Arc::new(FakeControlPlane::default());
    control.heartbeat_actions.lock().await.extend([
        HeartbeatAction::Delay(Duration::from_secs(4)),
        HeartbeatAction::Success,
        HeartbeatAction::Conflict,
    ]);
    let context = context(control.clone());
    let dispatch = dispatch(json!({}));

    let validation = tokio::spawn({
        let context = context.clone();
        let dispatch = dispatch.clone();
        async move { validate_lease(&context, &dispatch).await }
    });
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        validation.await.unwrap().unwrap(),
        ValidationOutcome::Fresh
    ));
    let keys = control.heartbeat_keys.lock().await;
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    drop(keys);

    *control.acquire_mode.lock().await = AcquireMode::Lease(Box::new(dispatch));
    let permits = Arc::new(Semaphore::new(1));
    let worker = FakeWorker::new(WorkerMode::Silent);
    let terminal = match acquire_one(&context, permits).await.unwrap() {
        Acquired::Terminal => true,
        Acquired::Idle | Acquired::Lease(_) => false,
    };
    assert!(
        terminal,
        "expired acquire replay must not become dispatchable"
    );
    assert_eq!(worker.dispatches.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn max_parallel_reserves_before_acquire_and_never_over_acquires() {
    let control = Arc::new(FakeControlPlane::default());
    let gate = Arc::new(Notify::new());
    *control.acquire_mode.lock().await = AcquireMode::GatedIdle(Arc::clone(&gate));
    let permits = Arc::new(Semaphore::new(2));
    let context = context(control.clone());
    let mut acquisitions = JoinSet::new();
    for _ in 0..3 {
        let context = context.clone();
        let permits = Arc::clone(&permits);
        acquisitions.spawn(async move { acquire_one(&context, permits).await });
    }
    wait_for_count(&control.acquire_notify, &control.acquire_started, 2).await;
    tokio::task::yield_now().await;
    assert_eq!(control.acquire_started.load(Ordering::SeqCst), 2);
    assert_eq!(control.max_active_acquires.load(Ordering::SeqCst), 2);
    *control.acquire_mode.lock().await = AcquireMode::Idle;
    gate.notify_waiters();
    while acquisitions.join_next().await.is_some() {}
    assert_eq!(control.max_active_acquires.load(Ordering::SeqCst), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn child_crash_restarts_only_after_every_held_lease_settles() {
    let fixture = ProcessWorkerFixture::new();
    let control = Arc::new(FakeControlPlane::default());
    let fail_gate = Arc::new(Notify::new());
    *control.fail_gate.lock().await = Some(Arc::clone(&fail_gate));
    *control.acquire_mode.lock().await = AcquireMode::Leases(Arc::new(StdMutex::new(
        [dispatch_with_lease(11), dispatch_with_lease(12)].into(),
    )));
    let runtime = AgentRuntime::with_client(
        loaded_config_with_worker(fixture.worker(2)),
        control.clone(),
    );
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move { runtime.run_with_shutdowns(signal_rx).await });

    let server = fixture.start_pending_server().await;
    wait_for_count_bounded(&server.dispatched, &server.dispatches, 2).await;
    assert_eq!(
        control.events.lock().await.first().map(String::as_str),
        Some("readiness:ready"),
        "the child accepted work before durable readiness"
    );
    fixture.crash();
    wait_for_count_bounded(&control.fail_started, &control.fail_started_count, 2).await;
    assert_eq!(
        control
            .events
            .lock()
            .await
            .iter()
            .filter(|event| event.as_str() == "readiness:not_ready")
            .count(),
        1,
        "the crashed child remained durably ready"
    );
    assert_eq!(fixture.start_count(), 1);
    assert!(
        tokio::time::timeout(RESTART_DELAY + Duration::from_millis(100), async {
            while fixture.start_count() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_err(),
        "replacement started while lease failures were unacknowledged"
    );
    *control.acquire_mode.lock().await = AcquireMode::Idle;
    fail_gate.notify_one();
    wait_for_event_count(control.as_ref(), "fail-ack", 1).await;
    assert_eq!(fixture.start_count(), 1);
    fail_gate.notify_one();
    wait_for_process_starts(&fixture, 2).await;
    wait_for_event_count(control.as_ref(), "readiness:ready", 2).await;
    assert_eq!(
        control.events.lock().await.as_slice(),
        [
            "readiness:ready",
            "readiness:not_ready",
            "fail-ack",
            "fail-ack",
            "readiness:ready",
        ],
        "restart lifecycle must bracket lease settlement and child startup with durable readiness"
    );

    assert_eq!(
        control.failures.lock().await.as_slice(),
        &[FailureClass::WorkerCrash, FailureClass::WorkerCrash]
    );

    signal_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.stop().await;
}

#[tokio::test(start_paused = true)]
async fn incarnation_conflict_fences_inflight_child_without_terminal_mutation() {
    let control = Arc::new(FakeControlPlane::default());
    *control.acquire_mode.lock().await = AcquireMode::Conflict;
    let exit = match acquire_one(&context(control.clone()), Arc::new(Semaphore::new(1))).await {
        Err(fatal) => RuntimeExit::Fatal(fatal),
        Ok(_) => RuntimeExit::Graceful,
    };
    assert_eq!(shutdown_kind_for_exit(&exit), ShutdownKind::Fenced);

    let worker = Arc::new(FakeWorker::new(WorkerMode::Silent));
    let permits = Arc::new(Semaphore::new(1));
    let (_cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let task = tokio::spawn(run_lease(
        held_lease(Arc::clone(&permits)).await,
        worker.clone(),
        credentials(),
        context(control.clone()),
        cancel_rx,
        shutdown_rx,
        StdRng::seed_from_u64(459),
    ));
    wait_for_count(&worker.dispatched, &worker.dispatches, 1).await;
    shutdown_tx.send(ShutdownKind::Fenced).unwrap();
    task.await.unwrap();
    assert!(control.failures.lock().await.is_empty());
    assert!(permits.try_acquire_owned().is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_shutdown_settles_before_child_reap_and_deactivation() {
    let fixture = ProcessWorkerFixture::new();
    let control = Arc::new(FakeControlPlane::default());
    let fail_gate = Arc::new(Notify::new());
    let deactivate_gate = Arc::new(Notify::new());
    *control.fail_gate.lock().await = Some(Arc::clone(&fail_gate));
    *control.deactivate_gate.lock().await = Some(Arc::clone(&deactivate_gate));
    *control.acquire_mode.lock().await =
        AcquireMode::Leases(Arc::new(StdMutex::new([dispatch_with_lease(11)].into())));
    let runtime = AgentRuntime::with_client(
        loaded_config_with_worker(fixture.worker(1)),
        control.clone(),
    );
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move { runtime.run_with_shutdowns(signal_rx).await });

    let server = fixture.start_pending_server().await;
    wait_for_count_bounded(&server.dispatched, &server.dispatches, 1).await;
    signal_tx.send(()).unwrap();
    wait_for_count_bounded(&control.fail_started, &control.fail_started_count, 1).await;
    assert!(fixture.process_is_alive());
    assert!(!fixture.has_exited());
    assert_eq!(control.deactivate_started_count.load(Ordering::SeqCst), 0);

    fail_gate.notify_one();
    wait_for_count_bounded(
        &control.deactivate_started,
        &control.deactivate_started_count,
        1,
    )
    .await;
    assert!(fixture.has_exited());
    assert!(!fixture.process_is_alive());
    deactivate_gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.stop().await;
    assert_eq!(
        control.failures.lock().await.as_slice(),
        &[FailureClass::UserCancellation]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn restart_child_reports_exhausted_startup_budget() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("failing-worker.sh");
    let starts = temp.path().join("starts");
    std::fs::write(&script, "printf '%s\\n' \"$$\" >> \"$1\"\nexit 1\n").unwrap();
    let mut failing_worker = worker();
    failing_worker.program = PathBuf::from("/bin/sh");
    failing_worker.args = vec![script.display().to_string(), starts.display().to_string()];
    let spec = ChildSpec::from_worker(&failing_worker, credentials());

    let error = restart_child(&context(Arc::new(FakeControlPlane::default())), spec)
        .await
        .unwrap_err();

    assert_eq!(error, ChildErrorKind::RestartExhausted);
    assert_eq!(std::fs::read_to_string(starts).unwrap().lines().count(), 3);
}

#[tokio::test]
async fn coordinator_exit_maps_coordinator_results() {
    let mut coordinators = JoinSet::new();
    coordinators.spawn(async { CoordinatorExit::RestartExhausted });
    let restart = coordinator_exit(coordinators.join_next().await);
    assert_eq!(
        shutdown_kind_for_exit(&restart),
        ShutdownKind::RestartExhausted
    );

    coordinators
        .spawn(async { CoordinatorExit::Fatal(RuntimeFatal::Internal("fatal".to_owned())) });
    let fatal = coordinator_exit(coordinators.join_next().await);
    assert_eq!(fatal.into_error().to_string(), "internal error: fatal");

    coordinators.spawn(async { CoordinatorExit::Shutdown(LeaseSettlement::Completed) });
    let stopped = coordinator_exit(coordinators.join_next().await);
    assert_eq!(
        stopped.into_error().to_string(),
        "internal error: worker coordinator stopped before shutdown"
    );

    let absent = coordinator_exit(None);
    assert_eq!(
        absent.into_error().to_string(),
        "internal error: worker coordinator stopped before shutdown"
    );
}

#[tokio::test]
async fn coordinator_exit_maps_join_failure() {
    let mut coordinators = JoinSet::new();
    coordinators.spawn(std::future::pending());
    coordinators.abort_all();

    let failed = coordinator_exit(coordinators.join_next().await);

    assert!(
        failed
            .into_error()
            .to_string()
            .starts_with("internal error: worker coordinator task failed:")
    );
}

#[tokio::test]
async fn synthetic_shutdown_sequence_delivers_first_and_second_signals_in_order() {
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let forwarder = tokio::spawn(forward_shutdowns(
        async move {
            let _ = first_rx.await;
        },
        async move {
            let _ = second_rx.await;
        },
        signal_tx,
    ));

    assert!(signal_rx.try_recv().is_err());
    first_tx.send(()).unwrap();
    signal_rx.recv().await.unwrap();
    assert!(signal_rx.try_recv().is_err());
    second_tx.send(()).unwrap();
    signal_rx.recv().await.unwrap();
    forwarder.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn second_signal_interrupts_blocked_settlement_then_reap_completes() {
    let control = Arc::new(FakeControlPlane::default());
    *control.fail_gate.lock().await = Some(Arc::new(Notify::new()));
    let worker = Arc::new(FakeWorker::new(WorkerMode::Error));
    let permits = Arc::new(Semaphore::new(1));
    let (cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    leases.spawn(run_lease(
        held_lease(Arc::clone(&permits)).await,
        worker,
        credentials(),
        context(control.clone()),
        cancel_rx,
        shutdown_rx.clone(),
        StdRng::seed_from_u64(459),
    ));
    shutdown_tx.send(ShutdownKind::User).unwrap();

    let coordinator_control = control.clone();
    let coordinator = tokio::spawn(async move {
        let forced = settle_leases_for_shutdown(
            &cancel_tx,
            &mut leases,
            &mut shutdown_rx,
            ShutdownKind::User,
            None,
        )
        .await;
        coordinator_control
            .events
            .lock()
            .await
            .push("reap".to_owned());
        forced
    });
    wait_for_count(&control.fail_started, &control.fail_started_count, 1).await;
    assert!(Arc::clone(&permits).try_acquire_owned().is_err());

    shutdown_tx.send(ShutdownKind::Forced).unwrap();
    assert_eq!(
        coordinator.await.unwrap(),
        LeaseSettlement::Forced(ShutdownForce::Signal)
    );
    assert_eq!(control.events.lock().await.as_slice(), &["reap"]);
    assert!(permits.try_acquire_owned().is_ok());
}

#[tokio::test]
async fn child_crash_shutdown_preserves_forced_final_wait() {
    let (cancel_tx, _cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    leases.spawn(std::future::pending());

    shutdown_tx.send(ShutdownKind::User).unwrap();
    let settling = settle_leases_after_child_crash(
        &cancel_tx,
        &mut leases,
        &mut shutdown_rx,
        Duration::from_hours(1),
    );
    tokio::pin!(settling);
    tokio::select! {
        biased;
        result = &mut settling => {
            assert!(result.is_none(), "settled before force: {result:?}");
        }
        () = tokio::task::yield_now() => {}
    }

    shutdown_tx.send(ShutdownKind::Forced).unwrap();
    assert_eq!(
        settling.await,
        Some(LeaseSettlement::Forced(ShutdownForce::Signal)),
        "the production crash-settlement handoff must preserve final force"
    );
}

#[tokio::test]
async fn coordinator_reaping_aggregates_forced_settlement() {
    let mut coordinators = JoinSet::new();
    coordinators
        .spawn(async { CoordinatorExit::Shutdown(LeaseSettlement::Forced(ShutdownForce::Signal)) });
    let (shutdown_tx, _shutdown_rx) = watch::channel(ShutdownKind::Fenced);
    let (_signal_tx, mut signal_rx) = mpsc::unbounded_channel();

    let progress = wait_for_coordinators(
        &mut coordinators,
        &shutdown_tx,
        &mut signal_rx,
        ShutdownSignalPhase::AwaitingFirst,
    )
    .await
    .unwrap();

    assert_eq!(progress.signal_phase, ShutdownSignalPhase::AwaitingFirst);
    assert_eq!(progress.forced, Some(ShutdownForce::Signal));
}

#[tokio::test]
async fn coalesced_forced_shutdown_cancels_before_aborting_leases() {
    let trace = Arc::new(StdMutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let (started_tx, started_rx) = oneshot::channel();
    let mut leases = JoinSet::new();
    leases.spawn({
        let trace = Arc::clone(&trace);
        async move {
            let _guard = CancellationBeforeDropTrace {
                cancellation: cancel_rx,
                trace,
            };
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        }
    });
    started_rx.await.unwrap();

    shutdown_tx.send(ShutdownKind::User).unwrap();
    shutdown_tx.send(ShutdownKind::Forced).unwrap();
    let kind = *shutdown_rx.borrow_and_update();
    let settlement = tokio::time::timeout(
        Duration::from_secs(5),
        settle_leases_for_shutdown(&cancel_tx, &mut leases, &mut shutdown_rx, kind, None),
    )
    .await
    .unwrap();

    assert_eq!(settlement, LeaseSettlement::Forced(ShutdownForce::Signal));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        &["cancellation-fenced", "task-drop"]
    );
}

#[tokio::test]
async fn second_signal_interrupts_deactivation_only_after_reap() {
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    control.events.lock().await.push("reap".to_owned());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let deactivation = tokio::spawn(async move {
        let mut signal_phase = ShutdownSignalPhase::ForceEnabled;
        runtime
            .deactivate_or_second_signal(
                incarnation(),
                NodeIncarnationEndReason::GracefulShutdown,
                &mut signal_rx,
                &mut signal_phase,
                // Far future: these test the signal, not the budget.
                Instant::now() + Duration::from_hours(1),
            )
            .await
    });
    wait_for_count(
        &control.deactivate_started,
        &control.deactivate_started_count,
        1,
    )
    .await;

    signal_tx.send(()).unwrap();
    let error = deactivation.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("termination signal"));
    assert_eq!(control.events.lock().await.as_slice(), &["reap"]);
}

#[tokio::test]
async fn restart_exhausted_deactivation_requires_a_genuine_second_signal() {
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    // Far-future call budget: this test gates deactivate with a Notify that is
    // never notified, so the production 10s budget would be a wall-clock race.
    let runtime = AgentRuntime::with_client_and_budgets(
        loaded_config(),
        control.clone(),
        ShutdownBudgets {
            call: Duration::from_hours(1),
            ..ShutdownBudgets::DEFAULT
        },
    );
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let finishing = runtime.finish_shutdown_lifecycle(
        incarnation(),
        RuntimeExit::RestartExhausted,
        Ok(ShutdownProgress {
            signal_phase: ShutdownSignalPhase::AwaitingFirst,
            forced: None,
        }),
        pending_heartbeat_handle(),
        &mut signal_rx,
    );
    tokio::pin!(finishing);
    let early = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut finishing => Some(result),
            () = wait_for_count(
                &control.deactivate_started,
                &control.deactivate_started_count,
                1,
            ) => None,
        }
    })
    .await
    .unwrap();
    assert!(early.is_none(), "finished before deactivation: {early:?}");

    signal_tx.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut finishing)
            .await
            .is_err(),
        "the first signal must leave restart-exhausted deactivation pending"
    );

    signal_tx.send(()).unwrap();
    let error = finishing.await.unwrap_err();
    assert!(error.to_string().contains("termination signal"));
}

#[tokio::test]
async fn child_startup_failure_deactivation_requires_a_genuine_second_signal() {
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    // Far-future call budget: this test gates deactivate with a Notify that is
    // never notified, so the production 10s budget would be a wall-clock race.
    let runtime = AgentRuntime::with_client_and_budgets(
        loaded_config(),
        control.clone(),
        ShutdownBudgets {
            call: Duration::from_hours(1),
            ..ShutdownBudgets::DEFAULT
        },
    );
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let running = runtime.run_with_shutdowns(signal_rx);
    tokio::pin!(running);
    let early = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut running => Some(result),
            () = wait_for_count(
                &control.deactivate_started,
                &control.deactivate_started_count,
                1,
            ) => None,
        }
    })
    .await
    .unwrap();
    assert!(early.is_none(), "finished before deactivation: {early:?}");

    signal_tx.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut running)
            .await
            .is_err(),
        "the first signal must not replace the child-startup failure"
    );

    signal_tx.send(()).unwrap();
    let error = running.await.unwrap_err();
    assert!(error.to_string().contains("termination signal"));
}

#[tokio::test]
async fn fatal_exit_stops_heartbeat_before_return_and_skips_deactivation() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let trace = Arc::new(StdMutex::new(vec!["reap"]));
    let heartbeat = observed_heartbeat_handle({
        let trace = Arc::clone(&trace);
        Arc::new(move || trace.lock().unwrap().push("heartbeat-stop"))
    });
    let (_signal_tx, mut signal_rx) = mpsc::unbounded_channel();

    let error = runtime
        .finish_shutdown_lifecycle(
            incarnation(),
            RuntimeExit::Fatal(RuntimeFatal::Internal("fatal".to_owned())),
            Ok(ShutdownProgress {
                signal_phase: ShutdownSignalPhase::ForceEnabled,
                forced: Some(ShutdownForce::Signal),
            }),
            heartbeat,
            &mut signal_rx,
        )
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "internal error: fatal");
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        &["reap", "heartbeat-stop"]
    );
    assert_eq!(control.deactivate_started_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn settling_a_crash_reports_a_shutdown_it_consumed() {
    let control = Arc::new(FakeControlPlane::default());
    let (cancel_tx, _cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    let _ = control;
    // Hold settlement open so the shutdown branch is the only ready one; an empty JoinSet
    // would make the select a coin flip between the two.
    let blocked = Arc::new(Notify::new());
    leases.spawn({
        let blocked = Arc::clone(&blocked);
        async move { blocked.notified().await }
    });

    // A single (non-forced) shutdown during crash settlement must be reported back. The
    // wait consumes the watch notification, so if this returned None the coordinator would
    // restart the child and then block forever on a `changed()` that has already fired.
    shutdown_tx.send(ShutdownKind::User).unwrap();
    // Bounded: swallowing the shutdown makes this wait forever, and a hanging test would
    // burn the CI timeout instead of reporting the defect.
    let settled = tokio::time::timeout(
        Duration::from_secs(5),
        cancel_and_wait(
            &cancel_tx,
            LeaseCancellation::Crash,
            &mut leases,
            &mut shutdown_rx,
        ),
    )
    .await;

    assert!(
        settled.is_ok(),
        "crash settlement swallowed the shutdown instead of reporting it"
    );
    assert_eq!(settled.unwrap(), Some((ShutdownKind::User, None)));
    leases.abort_all();
}

#[tokio::test(start_paused = true)]
async fn crash_after_a_clean_start_exhausts_the_budget_instead_of_respawning_forever() {
    let mut budget = CrashBudget::new();

    // A child that starts cleanly and then dies still spends the budget: ChildSupervisor
    // resets its own counter on every successful launch, so it can never bound this.
    for _ in 0..CRASH_LIMIT {
        assert!(!budget.record_and_exhausted());
    }
    assert!(budget.record_and_exhausted());
}

#[tokio::test(start_paused = true)]
async fn crash_budget_recovers_after_a_quiet_window() {
    let mut budget = CrashBudget::new();
    for _ in 0..CRASH_LIMIT {
        assert!(!budget.record_and_exhausted());
    }

    tokio::time::advance(CRASH_WINDOW + Duration::from_secs(1)).await;

    assert!(
        !budget.record_and_exhausted(),
        "an isolated later crash must not inherit a stale window"
    );
}

#[tokio::test]
async fn second_signal_interrupts_a_non_graceful_deactivation() {
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let deactivation = tokio::spawn(async move {
        let mut signal_phase = ShutdownSignalPhase::ForceEnabled;
        runtime
            .deactivate_or_second_signal(
                incarnation(),
                NodeIncarnationEndReason::ChildRestartExhausted,
                &mut signal_rx,
                &mut signal_phase,
                // Far future: these test the signal, not the budget.
                Instant::now() + Duration::from_hours(1),
            )
            .await
    });
    wait_for_count(
        &control.deactivate_started,
        &control.deactivate_started_count,
        1,
    )
    .await;

    signal_tx.send(()).unwrap();

    let error = deactivation.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("termination signal"));
}

#[tokio::test(start_paused = true)]
async fn unreachable_control_plane_fences_the_lease_at_the_ttl_deadline() {
    let control = Arc::new(FakeControlPlane::default());
    control
        .heartbeat_actions
        .lock()
        .await
        .push_back(HeartbeatAction::Hang);
    let (fence_tx, mut fence_rx) = mpsc::channel(1);
    let (_stop_tx, stop_rx) = watch::channel(false);
    let loop_context = context(control.clone());
    let running = tokio::spawn(async move {
        lease_heartbeat_loop(
            loop_context,
            LeaseId(1),
            1,
            6,
            stop_rx,
            fence_tx,
            StdRng::seed_from_u64(459),
        )
        .await;
    });

    // lease_ttl is 6s. The first beat fires at 1s and hangs, so the remaining budget is 5s
    // and the fence must land at 6s -- not merely "eventually". try_recv keeps paused time
    // from auto-advancing to a later deadline and passing this by accident.
    advance_seconds(5).await;
    assert!(
        fence_rx.try_recv().is_err(),
        "fenced before the lease ttl elapsed"
    );

    advance_seconds(2).await;
    assert_eq!(
        fence_rx.try_recv(),
        Ok(()),
        "did not fence once the lease ttl elapsed"
    );
    running.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn ongoing_heartbeat_uses_granted_ttl_not_local_configuration() {
    let control = Arc::new(FakeControlPlane::default());
    let (fence_tx, mut fence_rx) = mpsc::channel(1);
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut loop_context = context(control.clone());
    loop_context.lease_ttl = Duration::from_secs(20);
    let running = tokio::spawn(async move {
        lease_heartbeat_loop(
            loop_context,
            LeaseId(1),
            1,
            6,
            stop_rx,
            fence_tx,
            StdRng::seed_from_u64(459),
        )
        .await;
    });

    advance_seconds(1).await;
    wait_for_count(
        &control.lease_heartbeat_started,
        &control.lease_heartbeats,
        1,
    )
    .await;

    assert_eq!(control.heartbeat_ttls.lock().await.as_slice(), &[6]);
    assert!(fence_rx.try_recv().is_err());
    stop_tx.send(true).unwrap();
    running.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn unreachable_control_plane_fails_the_node_at_the_incarnation_ttl() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
    let heartbeat = runtime
        .start_node_heartbeat(incarnation(), 6, fatal_tx, StdRng::seed_from_u64(459))
        .await
        .unwrap();
    control.node_heartbeat_hangs.store(true, Ordering::SeqCst);

    advance_seconds(10).await;

    let fatal = fatal_rx.try_recv().unwrap();
    assert!(
        format!("{fatal:?}").contains("incarnation ttl"),
        "{fatal:?}"
    );
    heartbeat.stop();
}

#[tokio::test(start_paused = true)]
async fn validation_gives_the_lease_back_after_a_bounded_number_of_stale_attempts() {
    let control = Arc::new(FakeControlPlane::default());
    for _ in 0..VALIDATION_ATTEMPTS {
        control
            .heartbeat_actions
            .lock()
            .await
            .push_back(HeartbeatAction::Delay(Duration::from_secs(5)));
    }
    let context = context(control.clone());

    let outcome = validate_lease(&context, &dispatch(json!({})))
        .await
        .unwrap();

    assert!(matches!(outcome, ValidationOutcome::Terminal));
    assert_eq!(
        control.lease_heartbeats.load(Ordering::SeqCst),
        VALIDATION_ATTEMPTS as usize
    );
}

#[tokio::test(start_paused = true)]
async fn validation_uses_a_longer_granted_ttl_than_local_configuration() {
    let control = Arc::new(FakeControlPlane::default());
    control
        .heartbeat_actions
        .lock()
        .await
        .push_back(HeartbeatAction::Delay(Duration::from_secs(5)));
    let mut dispatch = dispatch(json!({}));
    dispatch.lease_ttl_seconds = 20;

    let outcome = validate_lease(&context(control.clone()), &dispatch)
        .await
        .unwrap();

    assert!(matches!(outcome, ValidationOutcome::Fresh));
    assert_eq!(control.lease_heartbeats.load(Ordering::SeqCst), 1);
    assert_eq!(control.heartbeat_ttls.lock().await.as_slice(), &[20]);
}

#[tokio::test(start_paused = true)]
async fn validation_rejects_a_shorter_granted_ttl_than_local_configuration() {
    let control = Arc::new(FakeControlPlane::default());
    for _ in 0..VALIDATION_ATTEMPTS {
        control
            .heartbeat_actions
            .lock()
            .await
            .push_back(HeartbeatAction::Delay(Duration::from_secs(5)));
    }
    let mut context = context(control.clone());
    context.lease_ttl = Duration::from_secs(20);
    let mut dispatch = dispatch(json!({}));
    dispatch.lease_ttl_seconds = 6;

    let outcome = validate_lease(&context, &dispatch).await.unwrap();

    assert!(matches!(outcome, ValidationOutcome::Terminal));
    assert_eq!(
        control.lease_heartbeats.load(Ordering::SeqCst),
        VALIDATION_ATTEMPTS as usize
    );
    assert_eq!(control.heartbeat_ttls.lock().await.as_slice(), &[6, 6, 6]);
}

#[tokio::test]
async fn shutdown_signal_interrupts_activation_before_children_start() {
    let control = Arc::new(FakeControlPlane::default());
    *control.activate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move { runtime.run_with_shutdowns(signal_rx).await });
    wait_for_count(
        &control.activate_started,
        &control.activate_started_count,
        1,
    )
    .await;

    signal_tx.send(()).unwrap();

    running.await.unwrap().unwrap();
    assert!(control.events.lock().await.is_empty());
}

#[derive(Debug, Clone)]
enum HeartbeatAction {
    Success,
    Delay(Duration),
    /// Never resolves, standing in for the client's unbounded retry against an
    /// unreachable control plane.
    Hang,
    Conflict,
}

#[derive(Debug, Clone)]
enum AcquireMode {
    Idle,
    Lease(Box<LeaseDispatch>),
    Leases(Arc<StdMutex<VecDeque<LeaseDispatch>>>),
    GatedIdle(Arc<Notify>),
    Conflict,
}

#[derive(Debug)]
struct FakeControlPlane {
    node_heartbeats: AtomicUsize,
    lease_heartbeats: AtomicUsize,
    lease_heartbeat_started: Notify,
    heartbeat_actions: Mutex<VecDeque<HeartbeatAction>>,
    heartbeat_keys: Mutex<Vec<String>>,
    heartbeat_ttls: Mutex<Vec<i64>>,
    acquire_mode: Mutex<AcquireMode>,
    acquire_started: AtomicUsize,
    active_acquires: AtomicUsize,
    max_active_acquires: AtomicUsize,
    acquire_notify: Notify,
    failures: Mutex<Vec<FailureClass>>,
    fail_gate: Mutex<Option<Arc<Notify>>>,
    fail_started: Notify,
    fail_started_count: AtomicUsize,
    node_heartbeat_hangs: AtomicBool,
    activate_gate: Mutex<Option<Arc<Notify>>>,
    activate_started: Notify,
    activate_started_count: AtomicUsize,
    activation_bodies: Mutex<Vec<JsonValue>>,
    deactivate_gate: Mutex<Option<Arc<Notify>>>,
    deactivate_started: Notify,
    deactivate_started_count: AtomicUsize,
    readiness_gate: Mutex<Option<Arc<Notify>>>,
    readiness_started: Notify,
    readiness_started_count: AtomicUsize,
    complete_calls: AtomicUsize,
    reject_complete: AtomicBool,
    events: Mutex<Vec<String>>,
}

impl Default for FakeControlPlane {
    fn default() -> Self {
        Self {
            node_heartbeats: AtomicUsize::new(0),
            lease_heartbeats: AtomicUsize::new(0),
            lease_heartbeat_started: Notify::new(),
            heartbeat_actions: Mutex::new(VecDeque::new()),
            heartbeat_keys: Mutex::new(Vec::new()),
            heartbeat_ttls: Mutex::new(Vec::new()),
            acquire_mode: Mutex::new(AcquireMode::Idle),
            acquire_started: AtomicUsize::new(0),
            active_acquires: AtomicUsize::new(0),
            max_active_acquires: AtomicUsize::new(0),
            acquire_notify: Notify::new(),
            failures: Mutex::new(Vec::new()),
            fail_gate: Mutex::new(None),
            fail_started: Notify::new(),
            fail_started_count: AtomicUsize::new(0),
            node_heartbeat_hangs: AtomicBool::new(false),
            activate_gate: Mutex::new(None),
            activate_started: Notify::new(),
            activate_started_count: AtomicUsize::new(0),
            activation_bodies: Mutex::new(Vec::new()),
            deactivate_gate: Mutex::new(None),
            deactivate_started: Notify::new(),
            deactivate_started_count: AtomicUsize::new(0),
            readiness_gate: Mutex::new(None),
            readiness_started: Notify::new(),
            readiness_started_count: AtomicUsize::new(0),
            complete_calls: AtomicUsize::new(0),
            reject_complete: AtomicBool::new(false),
            events: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ControlPlaneApi for FakeControlPlane {
    async fn activate(
        &self,
        node_id: NodeId,
        request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError> {
        self.activate_started_count.fetch_add(1, Ordering::SeqCst);
        self.activate_started.notify_waiters();
        if let Some(gate) = self.activate_gate.lock().await.clone() {
            gate.notified().await;
        }
        let body: JsonValue = serde_json::from_slice(request.body()).unwrap();
        let incarnation_id = body["incarnation_id"].as_str().unwrap().parse().unwrap();
        self.activation_bodies.lock().await.push(body.clone());
        Ok(ActivateOutcome {
            node_id,
            node_epoch: 1,
            incarnation_id,
            heartbeat_ttl_seconds: 6,
            workers: vec![ActivatedWorker {
                logical_name: "echo".to_owned(),
                worker_id: WorkerId(14),
                worker_epoch: 1,
            }],
        })
    }

    async fn worker_readiness(
        &self,
        node_id: NodeId,
        worker_id: WorkerId,
        request: &RetryRequest<WorkerReadinessRequest>,
    ) -> Result<WorkerReadinessOutcome, VoomError> {
        self.readiness_started_count.fetch_add(1, Ordering::SeqCst);
        self.readiness_started.notify_waiters();
        if let Some(gate) = self.readiness_gate.lock().await.clone() {
            gate.notified().await;
        }
        let body: JsonValue = serde_json::from_slice(request.body()).unwrap();
        let incarnation_id = body["incarnation_id"].as_str().unwrap().parse().unwrap();
        let readiness: WorkerReadiness = serde_json::from_value(body["readiness"].clone()).unwrap();
        self.events.lock().await.push(format!(
            "readiness:{}",
            match readiness {
                WorkerReadiness::Ready => "ready",
                WorkerReadiness::NotReady => "not_ready",
            }
        ));
        Ok(WorkerReadinessOutcome {
            node_id,
            incarnation_id,
            worker_id,
            readiness,
        })
    }

    async fn deactivate(
        &self,
        node_id: NodeId,
        _request: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError> {
        self.deactivate_started_count.fetch_add(1, Ordering::SeqCst);
        self.deactivate_started.notify_waiters();
        if let Some(gate) = self.deactivate_gate.lock().await.clone() {
            gate.notified().await;
        }
        self.events.lock().await.push("deactivate".to_owned());
        Ok(DeactivateOutcome {
            node_id,
            node_epoch: 1,
            incarnation_id: incarnation(),
            status: NodeIncarnationStatus::Retired,
            reason: NodeIncarnationEndReason::GracefulShutdown,
            retired_worker_ids: vec![WorkerId(14)],
        })
    }

    async fn node_heartbeat(
        &self,
        node_id: NodeId,
        _request: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError> {
        self.node_heartbeats.fetch_add(1, Ordering::SeqCst);
        if self.node_heartbeat_hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(NodeHeartbeatOutcome {
            node_id,
            status: "active".to_owned(),
        })
    }

    async fn acquire(
        &self,
        _request: &RetryRequest<AcquireRequest>,
    ) -> Result<AcquireOutcome, VoomError> {
        let mode = self.acquire_mode.lock().await.clone();
        match mode {
            AcquireMode::Idle => Ok(idle()),
            AcquireMode::Lease(dispatch) => Ok(AcquireOutcome::Leased(*dispatch)),
            AcquireMode::Leases(dispatches) => Ok(dispatches
                .lock()
                .unwrap()
                .pop_front()
                .map_or_else(idle, AcquireOutcome::Leased)),
            AcquireMode::Conflict => Err(VoomError::Conflict("incarnation fenced".to_owned())),
            AcquireMode::GatedIdle(gate) => {
                self.acquire_started.fetch_add(1, Ordering::SeqCst);
                let active = self.active_acquires.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active_acquires.fetch_max(active, Ordering::SeqCst);
                self.acquire_notify.notify_waiters();
                gate.notified().await;
                self.active_acquires.fetch_sub(1, Ordering::SeqCst);
                Ok(idle())
            }
        }
    }

    async fn lease_heartbeat(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError> {
        self.lease_heartbeats.fetch_add(1, Ordering::SeqCst);
        self.heartbeat_keys
            .lock()
            .await
            .push(request.idempotency_key().to_owned());
        let body: JsonValue = serde_json::from_slice(request.body()).unwrap();
        self.heartbeat_ttls
            .lock()
            .await
            .push(body["lease_ttl_seconds"].as_i64().unwrap());
        self.lease_heartbeat_started.notify_waiters();
        let action = self
            .heartbeat_actions
            .lock()
            .await
            .pop_front()
            .unwrap_or(HeartbeatAction::Success);
        match action {
            HeartbeatAction::Success => Ok(lease_heartbeat_outcome(lease_id)),
            HeartbeatAction::Delay(delay) => {
                tokio::time::sleep(delay).await;
                Ok(lease_heartbeat_outcome(lease_id))
            }
            HeartbeatAction::Hang => std::future::pending().await,
            HeartbeatAction::Conflict => Err(VoomError::Conflict("lease expired".to_owned())),
        }
    }

    async fn complete(
        &self,
        lease_id: LeaseId,
        _request: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        if self.reject_complete.load(Ordering::SeqCst) {
            return Err(VoomError::Conflict(
                "remote complete rejected: artifact access validation missing".to_owned(),
            ));
        }
        Ok(CompleteOutcome {
            lease_id,
            ticket_id: TicketId(13),
            worker_id: WorkerId(14),
            artifact_access_plan: access_plan(),
        })
    }

    async fn fail(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError> {
        let body: JsonValue = serde_json::from_slice(request.body()).unwrap();
        let class = body["class"].as_str().and_then(FailureClass::from_wire_str);
        if let Some(class) = class {
            self.failures.lock().await.push(class);
        }
        self.fail_started_count.fetch_add(1, Ordering::SeqCst);
        self.fail_started.notify_waiters();
        if let Some(gate) = self.fail_gate.lock().await.clone() {
            gate.notified().await;
        }
        self.events.lock().await.push("fail-ack".to_owned());
        Ok(FailOutcome {
            lease_id,
            ticket_id: TicketId(13),
            worker_id: WorkerId(14),
            artifact_access_plan: access_plan(),
        })
    }

    async fn commit_open(
        &self,
        _request: &RetryRequest<CommitOpenRequest>,
    ) -> Result<CommitOpenOutcome, VoomError> {
        Ok(CommitOpenOutcome { intents: vec![] })
    }

    async fn authorize_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        _request: &RetryRequest<CommitAuthorizeRequest>,
    ) -> Result<CommitAuthorizeOutcome, VoomError> {
        Err(VoomError::NotFound(format!("commit intent {intent_id}")))
    }

    async fn report_commit_applying(
        &self,
        intent_id: ArtifactCommitIntentId,
        _request: &RetryRequest<CommitApplyingRequest>,
    ) -> Result<CommitApplyingOutcome, VoomError> {
        Ok(CommitApplyingOutcome { intent_id })
    }

    async fn report_commit_outcome(
        &self,
        intent_id: ArtifactCommitIntentId,
        _request: &RetryRequest<CommitOutcomeRequest>,
    ) -> Result<CommitReceiptOutcome, VoomError> {
        Ok(CommitReceiptOutcome {
            intent_id,
            kind: "applied".to_owned(),
        })
    }

    async fn complete_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        _request: &RetryRequest<CommitCompleteRequest>,
    ) -> Result<CommitCompleteOutcome, VoomError> {
        Ok(CommitCompleteOutcome {
            intent_id,
            commit_record_id: ArtifactCommitRecordId(1),
            result_file_version_id: None,
            result_file_location_id: None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkerMode {
    Silent,
    Error,
}

#[derive(Debug)]
struct FakeWorker {
    mode: WorkerMode,
    dispatches: AtomicUsize,
    dispatched: Notify,
}

impl FakeWorker {
    fn new(mode: WorkerMode) -> Self {
        Self {
            mode,
            dispatches: AtomicUsize::new(0),
            dispatched: Notify::new(),
        }
    }
}

#[async_trait]
impl ClientHandle for FakeWorker {
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<WorkerIdentityResponse, ProtocolError> {
        Ok(WorkerIdentityResponse {
            worker_id: credentials.worker_id,
            worker_epoch: credentials.worker_epoch,
            protocol_version: voom_core::PROTOCOL_VERSION,
            proof: String::new(),
        })
    }

    async fn dispatch(
        &self,
        _credentials: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        self.dispatched.notify_waiters();
        let (mut writer, reader) = tokio::io::duplex(4096);
        match self.mode {
            WorkerMode::Silent => {
                tokio::spawn(async move {
                    std::future::pending::<()>().await;
                    drop(writer);
                });
            }
            WorkerMode::Error => {
                tokio::spawn(async move {
                    let frame = ProgressFrame::Error {
                        lease_id: request.lease_id,
                        seq: 0,
                        emitted_at: OffsetDateTime::UNIX_EPOCH,
                        class: FailureClass::WorkerCrash,
                        code: voom_core::ErrorCode::Internal,
                        message: "scripted worker error".to_owned(),
                        payload: None,
                    };
                    let mut bytes = serde_json::to_vec(&frame).unwrap();
                    bytes.push(b'\n');
                    let _ = writer.write_all(&bytes).await;
                });
            }
        }
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(reader);
        Ok(DispatchStream {
            response: OperationResponse {
                lease_id: request.lease_id,
                accepted_at: OffsetDateTime::UNIX_EPOCH,
            },
            frames: NdjsonReader::new(reader, request.lease_id),
        })
    }
}

#[cfg(unix)]
struct ProcessWorkerFixture {
    _temp: TempDir,
    script: PathBuf,
    starts: PathBuf,
    secret: PathBuf,
    endpoint: PathBuf,
    exited: PathBuf,
}

#[cfg(unix)]
impl ProcessWorkerFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("worker-wrapper.sh");
        let starts = temp.path().join("starts");
        let secret = temp.path().join("secret");
        let endpoint = temp.path().join("endpoint");
        let exited = temp.path().join("exited");
        std::fs::write(
            &script,
            r#"umask 077
printf '%s\n' "$$" >> "$1"
secret_tmp="$2.tmp.$$"
printf '%s\n' "$VOOM_WORKER_SECRET" > "$secret_tmp"
/bin/mv "$secret_tmp" "$2"
while [ ! -s "$3" ]; do /bin/sleep 0.01; done
IFS= read -r endpoint < "$3"
printf 'BOUND addr=%s\n' "$endpoint"
while IFS= read -r line; do :; done
printf 'exited\n' > "$4"
"#,
        )
        .unwrap();
        Self {
            _temp: temp,
            script,
            starts,
            secret,
            endpoint,
            exited,
        }
    }

    fn worker(&self, max_parallel: u32) -> WorkerConfig {
        WorkerConfig {
            name: "echo".to_owned(),
            program: PathBuf::from("/bin/sh"),
            args: vec![
                self.script.display().to_string(),
                self.starts.display().to_string(),
                self.secret.display().to_string(),
                self.endpoint.display().to_string(),
                self.exited.display().to_string(),
            ],
            operations: vec![OperationKind::ProbeFile],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            dependencies: crate::config::WorkerDependencyPaths::default(),
            accelerator: None,
            max_parallel,
        }
    }

    fn start_count(&self) -> usize {
        std::fs::read_to_string(&self.starts)
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn latest_pid(&self) -> u32 {
        std::fs::read_to_string(&self.starts)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn process_is_alive(&self) -> bool {
        let pid = self.latest_pid().to_string();
        std::process::Command::new("/bin/kill")
            .args(["-0", pid.as_str()])
            .output()
            .unwrap()
            .status
            .success()
    }

    fn has_exited(&self) -> bool {
        self.exited.exists()
    }

    fn crash(&self) {
        let pid = self.latest_pid().to_string();
        let status = std::process::Command::new("/bin/kill")
            .args(["-KILL", pid.as_str()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    async fn start_pending_server(&self) -> PendingWorkerServer {
        let secret = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(secret) = std::fs::read_to_string(&self.secret) {
                    break secret.trim().to_owned();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let dispatches = Arc::new(AtomicUsize::new(0));
        let dispatched = Arc::new(Notify::new());
        let handler_dispatches = Arc::clone(&dispatches);
        let handler_dispatched = Arc::clone(&dispatched);
        let handler: OperationHandler = Arc::new(move |_| -> OperationFuture {
            handler_dispatches.fetch_add(1, Ordering::SeqCst);
            handler_dispatched.notify_waiters();
            Box::pin(std::future::pending())
        });
        let running = HttpServer::new(
            WorkerCredentials {
                worker_id: WorkerId(14),
                worker_epoch: 1,
                secret: SecretString::from(secret),
            },
            handler,
        )
        .serve("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
        let endpoint_tmp = self.endpoint.with_extension("tmp");
        std::fs::write(&endpoint_tmp, format!("{}\n", running.bound)).unwrap();
        std::fs::rename(endpoint_tmp, &self.endpoint).unwrap();
        PendingWorkerServer {
            running: Some(running),
            dispatches,
            dispatched,
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessWorkerFixture {
    fn drop(&mut self) {
        if self.start_count() == 0 || !self.process_is_alive() {
            return;
        }
        let pid = self.latest_pid().to_string();
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", pid.as_str()])
            .status();
    }
}

#[cfg(unix)]
struct PendingWorkerServer {
    running: Option<ServerRunning>,
    dispatches: Arc<AtomicUsize>,
    dispatched: Arc<Notify>,
}

#[cfg(unix)]
impl PendingWorkerServer {
    async fn stop(mut self) {
        let running = self.running.take().unwrap();
        let _ = running.shutdown.send(());
        running.joined.await.unwrap();
    }
}

#[cfg(unix)]
impl Drop for PendingWorkerServer {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.shutdown.send(());
            running.joined.abort();
        }
    }
}

#[cfg(unix)]
async fn wait_for_process_starts(fixture: &ProcessWorkerFixture, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.start_count() < expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
async fn wait_for_event_count(control: &FakeControlPlane, event: &str, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = control
                .events
                .lock()
                .await
                .iter()
                .filter(|recorded| recorded.as_str() == event)
                .count();
            if count >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_count_bounded(notify: &Notify, count: &AtomicUsize, expected: usize) {
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_count(notify, count, expected),
    )
    .await
    .unwrap();
}

fn loaded_config() -> LoadedAgentConfig {
    loaded_config_with_worker(worker())
}

fn loaded_config_with_worker(worker: WorkerConfig) -> LoadedAgentConfig {
    LoadedAgentConfig {
        config: AgentConfig {
            control_plane_url: "http://127.0.0.1:1".to_owned(),
            ca_cert: None,
            node_id: NodeId(7),
            poll_interval_ms: 50,
            lease_ttl_seconds: 6,
            progress_idle_timeout_seconds: 5,
            shutdown_grace_seconds: 1,
            storage_roots: Vec::new(),
            node_token: TokenSource::Env {
                name: "VOOM_NODE_TOKEN".to_owned(),
            },
            workers: vec![worker],
        },
        node_token: SecretString::from("node-secret"),
    }
}

fn context(control: Arc<FakeControlPlane>) -> CoordinatorContext {
    CoordinatorContext {
        client: control,
        scan_client: None,
        endpoints: ChildEndpointRegistry::new(&[]),
        node_id: NodeId(7),
        incarnation_id: incarnation(),
        worker_id: WorkerId(14),
        lease_ttl: Duration::from_secs(6),
        progress_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(50),
        shutdown_grace: Duration::from_secs(1),
        budgets: ShutdownBudgets::DEFAULT,
        worker: worker(),
        fatal_tx: mpsc::unbounded_channel().0,
        storage_roots: HashMap::new(),
    }
}

fn worker() -> WorkerConfig {
    WorkerConfig {
        name: "echo".to_owned(),
        program: PathBuf::from("/bin/echo"),
        args: Vec::new(),
        operations: vec![OperationKind::ProbeFile],
        artifact_access: vec![ArtifactAccessMode::SharedMount],
        dependencies: crate::config::WorkerDependencyPaths::default(),
        accelerator: None,
        max_parallel: 2,
    }
}

fn credentials() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(14),
        worker_epoch: 1,
        secret: SecretString::from("worker-secret"),
    }
}

async fn held_lease(permits: Arc<Semaphore>) -> HeldLeaseGuard {
    HeldLeaseGuard {
        dispatch: dispatch(json!({"path": "/media/a.mkv"})),
        _permit: permits.acquire_owned().await.unwrap(),
    }
}

fn dispatch(payload: JsonValue) -> LeaseDispatch {
    dispatch_with_payload(11, payload)
}

fn dispatch_with_lease(lease_id: u64) -> LeaseDispatch {
    dispatch_with_payload(lease_id, json!({"path": "/media/a.mkv"}))
}

fn dispatch_with_payload(lease_id: u64, payload: JsonValue) -> LeaseDispatch {
    LeaseDispatch {
        lease_id: LeaseId(lease_id),
        scheduler_decision_id: 12,
        ticket_id: TicketId(13),
        worker_id: WorkerId(14),
        operation: "hash_file".to_owned(),
        dispatch_payload: payload,
        lease_ttl_seconds: 6,
        heartbeat_after_seconds: 1,
        artifact_access_plan: access_plan(),
    }
}

fn access_plan() -> ArtifactAccessPlan {
    ArtifactAccessPlan {
        id: 15,
        owner_node_id: None,
        access_evidence: None,
    }
}

#[test]
fn legacy_artifact_access_plan_fields_rejected_on_decode() {
    // Removed-field rule: the legacy synthetic/shared-mount plan shape is no
    // longer an accepted input on any mirror of the dispatch contract.
    for legacy in [
        r#"{"id":15,"input_handles":["input"],"output_handles":["output"],
            "selected_access_mode":"shared_mount"}"#,
        r#"{"id":15,"owner_node_id":null,"access_evidence":null,
            "selected_access_mode":"shared_mount"}"#,
    ] {
        let err = serde_json::from_str::<ArtifactAccessPlan>(legacy).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
}

fn idle() -> AcquireOutcome {
    AcquireOutcome::Idle(AcquireIdle {
        worker_id: WorkerId(14),
        scheduler_decision_id: 12,
    })
}

fn lease_heartbeat_outcome(lease_id: LeaseId) -> LeaseHeartbeatOutcome {
    LeaseHeartbeatOutcome {
        lease_id,
        worker_id: WorkerId(14),
        ttl_seconds: 6,
    }
}

fn incarnation() -> NodeIncarnationId {
    "0123456789abcdef0123456789abcdef".parse().unwrap()
}

fn pending_heartbeat_handle() -> NodeHeartbeatHandle {
    let (stop_tx, _stop_rx) = watch::channel(false);
    let joined = tokio::spawn(std::future::pending());
    NodeHeartbeatHandle {
        stop_tx,
        joined,
        stop_observer: None,
    }
}

fn observed_heartbeat_handle(observer: Arc<dyn Fn() + Send + Sync>) -> NodeHeartbeatHandle {
    let (stop_tx, _stop_rx) = watch::channel(false);
    let joined = tokio::spawn(std::future::pending());
    NodeHeartbeatHandle {
        stop_tx,
        joined,
        stop_observer: Some(observer),
    }
}

struct CancellationBeforeDropTrace {
    cancellation: watch::Receiver<LeaseCancellation>,
    trace: Arc<StdMutex<Vec<&'static str>>>,
}

impl Drop for CancellationBeforeDropTrace {
    fn drop(&mut self) {
        if *self.cancellation.borrow() == LeaseCancellation::Fenced {
            self.trace.lock().unwrap().push("cancellation-fenced");
        }
        self.trace.lock().unwrap().push("task-drop");
    }
}

async fn wait_for_count(notify: &Notify, count: &AtomicUsize, expected: usize) {
    loop {
        let notified = notify.notified();
        if count.load(Ordering::SeqCst) >= expected {
            return;
        }
        notified.await;
    }
}

async fn advance_seconds(seconds: u64) {
    for _ in 0..seconds {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
}

/// A child client that records the operation request it was handed.
#[derive(Debug)]
struct CapturingWorker {
    request: Mutex<Option<OperationRequest>>,
}

impl CapturingWorker {
    fn new() -> Self {
        Self {
            request: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ClientHandle for CapturingWorker {
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<WorkerIdentityResponse, ProtocolError> {
        Ok(WorkerIdentityResponse {
            worker_id: credentials.worker_id,
            worker_epoch: credentials.worker_epoch,
            protocol_version: voom_core::PROTOCOL_VERSION,
            proof: String::new(),
        })
    }

    async fn dispatch(
        &self,
        _credentials: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        *self.request.lock().await = Some(request.clone());
        let frame = ProgressFrame::Result {
            lease_id: request.lease_id,
            seq: 0,
            emitted_at: OffsetDateTime::UNIX_EPOCH,
            payload: serde_json::json!({"ok": true, "validated": true}),
        };
        let (mut writer, reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut bytes = serde_json::to_vec(&frame).unwrap();
            bytes.push(b'\n');
            let _ = writer.write_all(&bytes).await;
        });
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(reader);
        Ok(DispatchStream {
            response: OperationResponse {
                lease_id: request.lease_id,
                accepted_at: OffsetDateTime::UNIX_EPOCH,
            },
            frames: NdjsonReader::new(reader, request.lease_id),
        })
    }
}

#[tokio::test]
async fn dispatch_to_child_forwards_owner_local_plan_and_normalized_operation() {
    // Issue #478: the control plane normalizes the operation before dispatch,
    // so the child request carries the bare wire token, and the dispatch's
    // owner-local plan rides along for the worker's validated echo.
    let child = CapturingWorker::new();
    let control = Arc::new(FakeControlPlane::default());
    let mut dispatch = dispatch_with_payload(31, json!({"path": "/media/a.mkv"}));
    dispatch.operation = "probe_file".to_owned();
    dispatch.artifact_access_plan = ArtifactAccessPlan {
        id: 32,
        owner_node_id: Some(7),
        access_evidence: Some(
            serde_json::from_value(json!({
                "declaration": [
                    {"target": {"kind": "storage_root", "storage_root_id": 5},
                     "rights": ["read"]}
                ],
                "root_epochs": [{"storage_root_id": 5, "root_epoch": 2}]
            }))
            .unwrap(),
        ),
    };

    let outcome = dispatch_to_child(
        &dispatch,
        &child,
        &credentials(),
        &context(Arc::clone(&control)),
    )
    .await;
    assert!(
        matches!(outcome, LeaseOutcome::Complete(_)),
        "expected a completed dispatch"
    );
    let request = child.request.lock().await.take().unwrap();
    assert!(
        request.operation == OperationKind::ProbeFile,
        "child was dispatched"
    );
    assert_eq!(request.operation, OperationKind::ProbeFile);
    assert_eq!(request.lease_id, LeaseId(31));
    assert_eq!(
        request.lease_id, dispatch.lease_id,
        "the owner-local dispatch reaches the child with its lease identity"
    );
}

#[test]
fn the_published_tail_bound_is_the_sum_of_every_inner_bound_plus_margin() {
    let budgets = ShutdownBudgets::DEFAULT;
    // docs/runbooks/operator-node-agent.md publishes the tail as
    // `shutdown_grace_seconds + 26`. If this changes, that runbook is wrong.
    assert_eq!(
        budgets.tail(Duration::from_secs(10)),
        Duration::from_secs(36)
    );
    // config.rs caps shutdown_grace_seconds at 18, and the worst case must finish before
    // the supported 45-second supervisor stop timeout. See ADR 0090.
    assert_eq!(
        budgets.tail(Duration::from_secs(18)),
        Duration::from_secs(44)
    );
    assert!(budgets.tail(Duration::from_secs(18)) < Duration::from_secs(45));
}

/// A lease `JoinSet` whose one task never finishes, with its cancellation and
/// shutdown channels. The `Sender` halves are returned so a test can publish a
/// kind and keep the channels open.
fn never_settling_leases() -> (
    watch::Sender<LeaseCancellation>,
    JoinSet<()>,
    watch::Sender<ShutdownKind>,
    watch::Receiver<ShutdownKind>,
) {
    leases_settling_after(None)
}

/// The same, with the one lease task finishing after `after` (or never, on `None`).
fn leases_settling_after(
    after: Option<Duration>,
) -> (
    watch::Sender<LeaseCancellation>,
    JoinSet<()>,
    watch::Sender<ShutdownKind>,
    watch::Receiver<ShutdownKind>,
) {
    let (cancel_tx, _cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    match after {
        Some(delay) => leases.spawn(async move { tokio::time::sleep(delay).await }),
        None => leases.spawn(std::future::pending()),
    };
    (cancel_tx, leases, shutdown_tx, shutdown_rx)
}

#[tokio::test(start_paused = true)]
async fn shutdown_deadline_forces_blocked_lease_settlement() {
    let (_cancel_tx, mut leases, _shutdown_tx, mut shutdown_rx) = never_settling_leases();
    let deadline = Instant::now() + Duration::from_secs(10);

    let observed = wait_or_force(&mut leases, &mut shutdown_rx, false, Some(deadline)).await;

    assert_eq!(
        observed,
        Some((ShutdownKind::Forced, Some(ShutdownForce::Deadline))),
        "an overrunning settlement must be abandoned at its own budget, and say so"
    );
}

#[tokio::test(start_paused = true)]
async fn the_crash_settlement_path_is_not_armed() {
    // An unarmed wait must settle whatever the clock does. Arming the steady-state
    // crash path would terminate a healthy running agent after one slow settlement.
    let (_cancel_tx, mut leases, _shutdown_tx, mut shutdown_rx) =
        leases_settling_after(Some(Duration::from_mins(1)));

    let observed = wait_or_force(&mut leases, &mut shutdown_rx, false, None).await;

    assert_eq!(observed, None, "an unarmed wait must settle, not force");
}

#[tokio::test(start_paused = true)]
async fn the_crash_settlement_budget_starts_when_its_own_wait_does() {
    // Regression for a deadline stamped too early. settle_leases_after_child_crash
    // waits twice: the first wait is unbounded and can run long, and only the second
    // takes the budget. A budget stamped before the first wait arrives at the second
    // already spent. Here the first wait runs 20s and the leases settle 20s after
    // that, so a 30s budget measured from the second wait is ample — and a 30s budget
    // measured from the call is already gone.
    let (cancel_tx, mut leases, shutdown_tx, mut shutdown_rx) =
        leases_settling_after(Some(Duration::from_secs(40)));

    // The sender stays in scope: dropping it would close the watch, which
    // `wait_or_force` treats as forced, and the test would pass for the wrong reason.
    let settling = settle_leases_after_child_crash(
        &cancel_tx,
        &mut leases,
        &mut shutdown_rx,
        Duration::from_secs(30),
    );
    tokio::pin!(settling);
    tokio::select! {
        settlement = &mut settling => panic!("settled before any shutdown: {settlement:?}"),
        () = tokio::time::sleep(Duration::from_secs(20)) => {}
    }
    shutdown_tx.send(ShutdownKind::User).unwrap();
    let settlement = settling.await;

    assert_eq!(
        settlement,
        Some(LeaseSettlement::Completed),
        "a 30s budget must still be 30s when the second wait starts, not zero"
    );
}

#[tokio::test(start_paused = true)]
async fn a_deadline_forced_settlement_reports_deadline() {
    // Obtained from the code under test, not hand-constructed. The feature is
    // nullified if settle_leases_for_shutdown collapses every force to Signal, and a
    // hand-built ShutdownProgress cannot see that.
    let (cancel_tx, mut leases, _shutdown_tx, mut shutdown_rx) = never_settling_leases();
    let deadline = Instant::now() + Duration::from_secs(10);

    let settlement = settle_leases_for_shutdown(
        &cancel_tx,
        &mut leases,
        &mut shutdown_rx,
        ShutdownKind::User,
        Some(deadline),
    )
    .await;

    assert_eq!(
        settlement,
        LeaseSettlement::Forced(ShutdownForce::Deadline),
        "a settlement abandoned at its budget must not report itself as a signal force"
    );
}

#[tokio::test(start_paused = true)]
async fn a_deadline_forced_settlement_still_deactivates() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (_signal_tx, mut signal_rx) = mpsc::unbounded_channel();

    let error = runtime
        .finish_shutdown_lifecycle(
            incarnation(),
            RuntimeExit::Graceful,
            Ok(ShutdownProgress {
                signal_phase: ShutdownSignalPhase::ForceEnabled,
                forced: Some(ShutdownForce::Deadline),
            }),
            pending_heartbeat_handle(),
            &mut signal_rx,
        )
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("shutdown budget"),
        "must name the budget, not a signal nobody sent: {error}"
    );
    assert_eq!(
        control.events.lock().await.as_slice(),
        &["deactivate"],
        "the Retired write is what the bound exists to protect"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_deadline_abandons_a_blocked_deactivation() {
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (_signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let mut signal_phase = ShutdownSignalPhase::ForceEnabled;
    let deadline = Instant::now() + Duration::from_secs(10);

    // The outer timeout is what makes this a regression rather than a hang: without the
    // deadline arm the inner call never returns and this fails on Elapsed.
    let error = tokio::time::timeout(
        Duration::from_mins(10),
        runtime.deactivate_or_second_signal(
            incarnation(),
            NodeIncarnationEndReason::GracefulShutdown,
            &mut signal_rx,
            &mut signal_phase,
            deadline,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();

    assert!(
        error.to_string().contains("shutdown budget"),
        "a blocked deactivation must report its budget: {error}"
    );
}

#[tokio::test]
async fn until_shutdown_returns_the_work_when_no_shutdown_arrives() {
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let value = until_shutdown(&shutdown_rx, std::future::ready(7)).await;
    assert_eq!(value, Some(7), "steady state must not disturb the work");
}

#[tokio::test]
async fn until_shutdown_abandons_the_work_once_a_shutdown_is_in_flight() {
    let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    shutdown_tx.send(ShutdownKind::User).unwrap();
    let value = until_shutdown(&shutdown_rx, std::future::pending::<u8>()).await;
    assert_eq!(
        value, None,
        "a blocked call must be released by the shutdown"
    );
}

#[tokio::test]
async fn until_shutdown_leaves_the_original_receivers_notification_unconsumed() {
    // The load-bearing property. The shutdown value is sent exactly once, and
    // cancel_and_wait's wait_or_force is deliberately unbounded with
    // `shutdown.changed()` as its only escape. If until_shutdown marked the value seen
    // on the caller's receiver, that wait would be stranded for the client's whole
    // retry budget. It races a clone instead.
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    shutdown_tx.send(ShutdownKind::User).unwrap();

    assert_eq!(
        until_shutdown(&shutdown_rx, std::future::pending::<u8>()).await,
        None
    );

    // The original receiver must still observe the change.
    let still_observable = tokio::time::timeout(Duration::from_secs(1), shutdown_rx.changed())
        .await
        .unwrap();
    assert!(still_observable.is_ok());
    assert_eq!(*shutdown_rx.borrow(), ShutdownKind::User);
}

#[tokio::test(start_paused = true)]
async fn the_tail_backstop_bounds_a_wait_no_inner_budget_covers() {
    // `pending()` stands in for a wait nobody raced. After the inner bounds land, no
    // reachable wait is like that — which is the design working — so the backstop is
    // tested at the function that implements it rather than through fault injection.
    let error = run_shutdown_tail_within(Duration::from_secs(30), std::future::pending())
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("please report this if no commit was in flight"),
        "the backstop must name itself, not reuse the deadline message: {error}"
    );
    assert!(
        message.contains("voom artifact recover-commit"),
        "the backstop is the only channel that names the commit-slot action: {error}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_coordinators_does_not_time_the_reap() {
    // R2's regression. Nothing may put a wall clock over the coordinator join: a
    // coordinator does not return until its child reap has finished, so a deadline here
    // would mark a routine stop forced whenever a worker used its grace — and since
    // main returns Result, that stop would exit FAILURE and leave the unit in `failed`.
    // This is why the settlement budget lives in wait_or_force. See ADR 0088.
    let mut coordinators = JoinSet::new();
    coordinators.spawn(async {
        tokio::time::sleep(Duration::from_mins(5)).await;
        CoordinatorExit::Shutdown(LeaseSettlement::Completed)
    });
    let (shutdown_tx, _shutdown_rx) = watch::channel(ShutdownKind::User);
    let (_signal_tx, mut signals) = mpsc::unbounded_channel();

    let progress = wait_for_coordinators(
        &mut coordinators,
        &shutdown_tx,
        &mut signals,
        ShutdownSignalPhase::ForceEnabled,
    )
    .await
    .unwrap();

    assert_eq!(
        progress.forced, None,
        "a slow reap must not be a forced shutdown"
    );
}

#[tokio::test(start_paused = true)]
async fn a_second_signal_after_a_deadline_force_still_forces() {
    // R4 is the one criterion this change narrows: ratified as unchanged in outcome,
    // explicitly changed in latency. A deadline force is recorded first, so the signal
    // arm's existing guard holds the first force — the operator's signal is consumed
    // later, in deactivate_or_second_signal, rather than here.
    let mut coordinators = JoinSet::new();
    // One coordinator forces on its own budget straight away; a second is still
    // reaping. The signal arrives only after the first has been recorded, which is the
    // ordering this test is about — queueing both up front would race the select arms.
    coordinators.spawn(async {
        CoordinatorExit::Shutdown(LeaseSettlement::Forced(ShutdownForce::Deadline))
    });
    coordinators.spawn(async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        CoordinatorExit::Shutdown(LeaseSettlement::Completed)
    });
    let (shutdown_tx, _shutdown_rx) = watch::channel(ShutdownKind::User);
    let (signal_tx, mut signals) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = signal_tx.send(());
    });

    let progress = wait_for_coordinators(
        &mut coordinators,
        &shutdown_tx,
        &mut signals,
        ShutdownSignalPhase::ForceEnabled,
    )
    .await
    .unwrap();

    assert_eq!(
        progress.forced,
        Some(ShutdownForce::Deadline),
        "first force recorded wins, exactly as before this change"
    );
}

#[tokio::test(start_paused = true)]
async fn a_queued_signal_still_forces_after_a_deadline_force() {
    // R4's ratified OUTCOME, one layer up from the guard. The layer above records
    // Deadline and lets the queued signal through to deactivate_or_second_signal, where
    // it must still force: forced_shutdown_error(), Retired write skipped. Only the
    // latency changed. Asserting progress.forced alone would prove the signal was
    // ignored, which is the opposite of what criterion 4 protects.
    let control = Arc::new(FakeControlPlane::default());
    // Never notified, so the signal is what resolves the call.
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    signal_tx.send(()).unwrap();

    let error = runtime
        .finish_shutdown_lifecycle(
            incarnation(),
            RuntimeExit::Graceful,
            Ok(ShutdownProgress {
                signal_phase: ShutdownSignalPhase::ForceEnabled,
                forced: Some(ShutdownForce::Deadline),
            }),
            pending_heartbeat_handle(),
            &mut signal_rx,
        )
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("termination signal"),
        "the operator's second signal must still be the cause: {error}"
    );
    assert!(
        !control
            .events
            .lock()
            .await
            .iter()
            .any(|e| e == "deactivate"),
        "and a signal force still skips the Retired write"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_sigterm_exits_the_agent_when_the_control_plane_never_answers() {
    // The charter's scenario end to end: one shutdown request, a control plane that
    // never answers the deactivation, and an agent that must still exit.
    //
    // Deliberately built from names that predate this change — with_client,
    // run_with_shutdowns, deactivate_gate — so it compiles against the pre-change tree
    // and fails there on Elapsed. That is what makes it R5 evidence rather than
    // coverage, and it is why it takes the default budgets and costs about 10s.
    let fixture = ProcessWorkerFixture::new();
    let control = Arc::new(FakeControlPlane::default());
    // Never notified: the control plane does not answer.
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(
        loaded_config_with_worker(fixture.worker(1)),
        control.clone(),
    );
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move { runtime.run_with_shutdowns(signal_rx).await });

    let server = fixture.start_pending_server().await;
    // Wait until the worker is ready, so the signal reaches the coordinator loop rather
    // than the activation-retry path, which exits successfully by design.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if control
                .events
                .lock()
                .await
                .iter()
                .any(|event| event == "readiness:ready")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    signal_tx.send(()).unwrap();

    // Far above the 86s tail, so pre-change this fails on Elapsed rather than hanging
    // the suite; post-change it returns at about one call budget.
    let error = tokio::time::timeout(Duration::from_mins(2), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();

    assert!(
        error.to_string().contains("shutdown budget"),
        "the agent must exit naming the budget it gave up at: {error}"
    );
    assert_eq!(
        control.deactivate_started_count.load(Ordering::SeqCst),
        1,
        "and it must have tried to retire the incarnation"
    );
    server.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_crashed_worker_release_does_not_wait_out_the_retry_budget() {
    // The wiring point, not the helper. restart_after_child_exit holds three waits that
    // reach the retrying client or a five-minute accelerator startup; without the
    // until_shutdown wrappings a coordinator parked in one holds the whole shutdown tail
    // open for production_request_budget(). Deleting any wrapping must fail this.
    let fixture = ProcessWorkerFixture::new();
    let control = Arc::new(FakeControlPlane::default());
    // Never notified: the control plane does not answer the readiness update.
    *control.readiness_gate.lock().await = Some(Arc::new(Notify::new()));
    let supervisor = ChildSupervisor::new(Duration::from_millis(50), Duration::from_millis(50));
    // The child binds and registers while the fixture's server is coming up, so these
    // run together the way the runtime starts them.
    let (children, server) = tokio::join!(
        supervisor.start_all(vec![ChildSpec::from_worker(
            &fixture.worker(1),
            credentials()
        )]),
        fixture.start_pending_server(),
    );
    let child = children.unwrap().pop().unwrap();

    let (cancel_tx, _cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    let mut crashes = CrashBudget::new();
    let context = context(control.clone());

    let restarting = restart_after_child_exit(
        child,
        &context,
        &cancel_tx,
        &mut leases,
        &mut shutdown_rx,
        &mut crashes,
    );
    tokio::pin!(restarting);
    // The future is lazy, so it has to be polled while waiting for its readiness call
    // to reach the fake.
    tokio::select! {
        exit = &mut restarting => panic!("returned before the readiness call: {exit:?}"),
        result = tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_count(&control.readiness_started, &control.readiness_started_count, 1),
        ) => result.unwrap(),
    }
    shutdown_tx.send(ShutdownKind::User).unwrap();

    // Well under production_request_budget() = 153.75s, which is what this would take
    // if the readiness call were not raced against the shutdown.
    let exit = tokio::time::timeout(Duration::from_secs(10), restarting)
        .await
        .unwrap();

    assert!(
        matches!(exit, Err(CoordinatorExit::Shutdown(_))),
        "a shutdown must release the readiness call and settle, not drain the retry budget"
    );
    server.stop().await;
}
