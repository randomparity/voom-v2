#![expect(
    clippy::exit,
    reason = "the test-executable helper must model exact child exit statuses"
)]
#![expect(
    clippy::panic,
    reason = "the cancellation-safety contract requires a post-spawn caller panic"
)]

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use serde_json::json;
use voom_core::WorkerId;
use voom_worker_protocol::{
    HttpServer, OperationHandler, OperationKind, ServerHandle, WorkerCredentials,
    load_worker_bind_addr_from_env, load_worker_credentials_from_env,
};

use super::*;

const HELPER_ENV: &str = "VOOM_PROCESS_SUPERVISOR_TEST_HELPER";
const READY_EXIT: u64 = 1;
const OVERSIZED_READINESS: u64 = 2;
const EXIT_BEFORE_READINESS: u64 = 3;
const READY_UNTIL_STDIN_CLOSES: u64 = 4;
const PENDING_READINESS: u64 = 5;
const READY_DELAYED_EXIT: u64 = 6;
const READY_IGNORE_STDIN: u64 = 7;
const PROCESS_CRASH_WORKER: u64 = 8;
const PROCESS_CLEAN_EXIT_WORKER: u64 = 9;

#[test]
fn process_supervisor_test_helper() {
    let Ok(mode) = std::env::var(HELPER_ENV) else {
        return;
    };
    let Ok(mode) = mode.parse::<u64>() else {
        std::process::exit(125);
    };
    match mode {
        READY_EXIT => {
            write_readiness();
            std::thread::sleep(Duration::from_millis(25));
            std::process::exit(17);
        }
        OVERSIZED_READINESS => {
            write_helper_output(&vec![b'x'; 4097]);
            wait_for_stdin_close();
            std::process::exit(32);
        }
        EXIT_BEFORE_READINESS => std::process::exit(23),
        READY_UNTIL_STDIN_CLOSES => {
            write_readiness();
            wait_for_stdin_close();
            std::process::exit(0);
        }
        PENDING_READINESS => {
            wait_for_stdin_close();
            std::process::exit(33);
        }
        READY_DELAYED_EXIT => {
            write_readiness();
            std::thread::sleep(Duration::from_millis(250));
            std::process::exit(19);
        }
        READY_IGNORE_STDIN => {
            write_readiness();
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(0);
        }
        PROCESS_CRASH_WORKER => run_process_worker(101),
        PROCESS_CLEAN_EXIT_WORKER => run_process_worker(0),
        _ => std::process::exit(124),
    }
}

fn run_process_worker(exit_code: i32) -> ! {
    let credentials =
        load_worker_credentials_from_env().unwrap_or_else(|_| std::process::exit(121));
    let bind = load_worker_bind_addr_from_env().unwrap_or_else(|_| std::process::exit(120));
    let handler: OperationHandler = Arc::new(move |request| {
        Box::pin(async move {
            let valid = request.operation == OperationKind::TranscodeVideo
                && request.lease_id.0 > 0
                && request.payload == json!({"mode":"crash","path":"/stress/process-crash"})
                && request.heartbeat_deadline_ms == 1_000
                && request.progress_idle_deadline_ms == 1_000;
            std::process::exit(if valid { exit_code } else { 126 });
        })
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|_| std::process::exit(119));
    let running = runtime
        .block_on(HttpServer::new(credentials, handler).serve(bind))
        .unwrap_or_else(|_| std::process::exit(118));
    write_helper_output(format!("BOUND addr={}\n", running.bound).as_bytes());
    let shutdown = running.shutdown;
    let stdin = std::thread::spawn(move || {
        wait_for_stdin_close();
        let _ = shutdown.send(());
    });
    runtime
        .block_on(running.joined)
        .unwrap_or_else(|_| std::process::exit(117));
    stdin.join().unwrap_or_else(|_| std::process::exit(116));
    std::process::exit(0);
}

fn write_readiness() {
    write_helper_output(b"BOUND addr=127.0.0.1:43123\n");
}

fn write_helper_output(bytes: &[u8]) {
    let mut stderr = std::io::stderr().lock();
    if stderr.write_all(bytes).is_err() || stderr.flush().is_err() {
        std::process::exit(123);
    }
}

fn wait_for_stdin_close() {
    let mut bytes = Vec::new();
    if std::io::stdin().read_to_end(&mut bytes).is_err() {
        std::process::exit(122);
    }
}

fn credentials(mode: u64) -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(mode),
        worker_epoch: 1,
        secret: SecretString::from("process-supervisor-test"),
    }
}

fn test_binary() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}

enum WaitStep {
    Exit(ChildExit),
    Error(io::ErrorKind, &'static str),
}

struct ScriptedChild {
    waits: VecDeque<WaitStep>,
    kill_error: Option<&'static str>,
    wait_count: usize,
    kill_count: usize,
}

impl ScriptedChild {
    fn new(waits: impl IntoIterator<Item = WaitStep>) -> Self {
        Self {
            waits: waits.into_iter().collect(),
            kill_error: None,
            wait_count: 0,
            kill_count: 0,
        }
    }
}

impl SupervisedChild for ScriptedChild {
    async fn wait(&mut self, _child_id: ChildId) -> io::Result<ChildExit> {
        self.wait_count += 1;
        match self.waits.pop_front().unwrap() {
            WaitStep::Exit(status) => Ok(status),
            WaitStep::Error(kind, detail) => Err(io::Error::new(kind, detail)),
        }
    }

    fn start_kill(&mut self) -> io::Result<()> {
        self.kill_count += 1;
        self.kill_error
            .map_or(Ok(()), |detail| Err(io::Error::other(detail)))
    }
}

fn scripted_exit(child_id: ChildId) -> ChildExit {
    ChildExit {
        child_id,
        code: Some(17),
        success: false,
    }
}

struct PendingOuterOwner<T> {
    supervisor: ProcessSupervisor,
    completion: Result<T, JoinError>,
}

struct ReapedOuterOwner<T> {
    exits: Vec<ChildExit>,
    completion: Result<T, JoinError>,
}

impl<T> PendingOuterOwner<T> {
    async fn shutdown(self) -> Result<ReapedOuterOwner<T>, ProcessSupervisorError> {
        let exits = self.supervisor.shutdown().await?;
        Ok(ReapedOuterOwner {
            exits,
            completion: self.completion,
        })
    }
}

impl<T> ReapedOuterOwner<T> {
    fn exits(&self) -> &[ChildExit] {
        &self.exits
    }

    fn into_join_error(self) -> JoinError {
        match self.completion {
            Ok(_) => panic!("inner caller unexpectedly completed"),
            Err(error) => error,
        }
    }
}

#[tokio::test]
async fn readiness_success_reports_child_identity_and_loopback_bound_address() {
    let supervisor = ProcessSupervisor::start();
    let ready = supervisor
        .spawn(test_binary(), credentials(READY_EXIT))
        .await
        .unwrap();

    assert_ne!(ready.pid, 0);
    assert!(ready.bound.ip().is_loopback());
    assert_eq!(ready.bound.port(), 43123);
    let exited = supervisor.wait(ready.child_id).await.unwrap();
    assert_eq!(exited.child_id, ready.child_id);
    assert_eq!(exited.code, Some(17));
    assert!(!exited.success);
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[tokio::test]
async fn readiness_above_four_kib_without_newline_is_rejected_after_reap() {
    let supervisor = ProcessSupervisor::start();
    let error = supervisor
        .spawn(test_binary(), credentials(OVERSIZED_READINESS))
        .await
        .unwrap_err();

    assert!(matches!(error, ProcessSupervisorError::Readiness { .. }));
    assert!(error.to_string().contains("4096"));
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[tokio::test]
async fn child_exit_before_readiness_is_reported_after_reap() {
    let supervisor = ProcessSupervisor::start();
    let error = supervisor
        .spawn(test_binary(), credentials(EXIT_BEFORE_READINESS))
        .await
        .unwrap_err();

    assert!(matches!(error, ProcessSupervisorError::Readiness { .. }));
    assert!(error.to_string().contains("before readiness"));
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[tokio::test]
async fn natural_exit_remains_observable_until_delayed_wait() {
    let supervisor = ProcessSupervisor::start();
    let ready = supervisor
        .spawn(test_binary(), credentials(READY_EXIT))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(75)).await;

    let exited = supervisor.wait(ready.child_id).await.unwrap();
    assert_eq!(exited.code, Some(17));
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[test]
fn cancelled_late_wait_restores_tombstone_for_next_live_wait() {
    let child_id = ChildId(81);
    let status = scripted_exit(child_id);
    let mut actor = Actor::new(TestMilestones::default());
    actor.registry.insert(
        child_id,
        ChildState::Exited {
            status: status.clone(),
        },
    );

    let (cancelled_reply, cancelled_receiver) = tokio::sync::oneshot::channel();
    drop(cancelled_receiver);
    actor.register_wait(child_id, cancelled_reply);

    let (reply, mut receiver) = tokio::sync::oneshot::channel();
    actor.register_wait(child_id, reply);
    assert_eq!(receiver.try_recv().unwrap().unwrap(), status);
}

#[tokio::test]
async fn duplicate_wait_is_rejected_while_first_wait_remains_registered() {
    let supervisor = ProcessSupervisor::start();
    let ready = supervisor
        .spawn(test_binary(), credentials(READY_UNTIL_STDIN_CLOSES))
        .await
        .unwrap();
    let mut first_wait = Box::pin(supervisor.wait(ready.child_id));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut first_wait)
            .await
            .is_err()
    );

    let duplicate = supervisor.wait(ready.child_id).await.unwrap_err();
    assert!(matches!(duplicate, ProcessSupervisorError::Protocol { .. }));
    assert!(duplicate.to_string().contains("already has a waiter"));

    drop(first_wait);
    let shutdown_exits = supervisor.shutdown().await.unwrap();
    assert_eq!(shutdown_exits.len(), 1);
    assert_eq!(shutdown_exits[0].child_id, ready.child_id);
}

#[tokio::test]
async fn shutdown_kills_and_reaps_a_child_that_stays_alive() {
    let supervisor = ProcessSupervisor::start();
    let ready = supervisor
        .spawn(test_binary(), credentials(READY_IGNORE_STDIN))
        .await
        .unwrap();

    let exits = supervisor.shutdown().await.unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, ready.child_id);
    assert!(!exits[0].success);
}

#[tokio::test]
async fn live_wait_error_kills_and_reaps_before_returning() {
    let child_id = ChildId(91);
    let mut child = ScriptedChild::new([
        WaitStep::Error(io::ErrorKind::Other, "injected live wait failure"),
        WaitStep::Exit(scripted_exit(child_id)),
    ]);
    let mut stdin = None;
    let (_shutdown, mut shutdown_rx) = tokio::sync::oneshot::channel();

    let exit = wait_or_shutdown(child_id, &mut child, &mut stdin, &mut shutdown_rx)
        .await
        .unwrap();

    assert_eq!(exit, scripted_exit(child_id));
    assert_eq!(child.wait_count, 2);
    assert_eq!(child.kill_count, 1);
}

#[tokio::test]
async fn start_kill_error_still_performs_final_reap() {
    let child_id = ChildId(92);
    let mut child = ScriptedChild::new([
        WaitStep::Error(io::ErrorKind::Other, "injected graceful wait failure"),
        WaitStep::Exit(scripted_exit(child_id)),
    ]);
    child.kill_error = Some("injected kill failure");
    let mut stdin = None;

    let exit = shutdown_child(child_id, &mut child, &mut stdin)
        .await
        .unwrap();

    assert_eq!(exit, scripted_exit(child_id));
    assert_eq!(child.wait_count, 2);
    assert_eq!(child.kill_count, 1);
}

#[tokio::test]
async fn final_reap_error_reports_unrecoverable_child_ownership() {
    let child_id = ChildId(93);
    let mut child = ScriptedChild::new([
        WaitStep::Error(io::ErrorKind::Other, "injected graceful wait failure"),
        WaitStep::Error(io::ErrorKind::Other, "injected final reap failure"),
    ]);
    let mut stdin = None;

    let error = shutdown_child(child_id, &mut child, &mut stdin)
        .await
        .unwrap_err();

    assert_eq!(child.wait_count, 2);
    assert_eq!(child.kill_count, 1);
    assert!(error.to_string().contains("ownership became unrecoverable"));
    assert!(error.to_string().contains("injected final reap failure"));
}

#[tokio::test]
async fn interrupted_final_reap_is_retried_until_status_is_observed() {
    let child_id = ChildId(94);
    let mut child = ScriptedChild::new([
        WaitStep::Error(io::ErrorKind::Other, "injected graceful wait failure"),
        WaitStep::Error(io::ErrorKind::Interrupted, "injected interrupted reap"),
        WaitStep::Exit(scripted_exit(child_id)),
    ]);
    let mut stdin = None;

    let exit = shutdown_child(child_id, &mut child, &mut stdin)
        .await
        .unwrap();

    assert_eq!(exit, scripted_exit(child_id));
    assert_eq!(child.wait_count, 3);
    assert_eq!(child.kill_count, 1);
}

#[tokio::test]
async fn caller_cancellation_after_natural_exit_preserves_unwaited_status() {
    let supervisor = Arc::new(ProcessSupervisor::start());
    let inner_supervisor = Arc::clone(&supervisor);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let inner = tokio::spawn(async move {
        let ready = inner_supervisor
            .spawn(test_binary(), credentials(READY_EXIT))
            .await
            .unwrap();
        spawned_tx.send(ready.child_id).unwrap();
        std::future::pending::<()>().await;
    });
    let child_id = spawned_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(75)).await;
    inner.abort();
    assert!(inner.await.unwrap_err().is_cancelled());

    let supervisor = Arc::try_unwrap(supervisor).ok().unwrap();
    let exits = supervisor.shutdown().await.unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert_eq!(exits[0].code, Some(17));
}

#[tokio::test]
async fn shutdown_attempts_every_registered_child() {
    let supervisor = ProcessSupervisor::start();
    let first = supervisor
        .spawn(test_binary(), credentials(READY_UNTIL_STDIN_CLOSES))
        .await
        .unwrap();
    let second = supervisor
        .spawn(test_binary(), credentials(READY_IGNORE_STDIN))
        .await
        .unwrap();

    let mut exits = supervisor.shutdown().await.unwrap();
    exits.sort_by_key(|exit| exit.child_id.0);
    assert_eq!(exits.len(), 2);
    assert_eq!(exits[0].child_id, first.child_id);
    assert_eq!(exits[1].child_id, second.child_id);
}

#[tokio::test]
async fn abort_while_awaiting_readiness_reaps_before_propagating_cancellation() {
    let supervisor = Arc::new(ProcessSupervisor::start());
    let inner_supervisor = Arc::clone(&supervisor);
    let inner = tokio::spawn(async move {
        inner_supervisor
            .spawn(test_binary(), credentials(PENDING_READINESS))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    inner.abort();
    let completion = inner.await;
    let supervisor = Arc::try_unwrap(supervisor).ok().unwrap();
    let owner = PendingOuterOwner {
        supervisor,
        completion,
    };
    let reaped = owner.shutdown().await.unwrap();
    let exits = reaped.exits();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].code, Some(33));
    let cancellation = reaped.into_join_error();
    assert!(cancellation.is_cancelled());
}

#[tokio::test]
async fn abort_while_awaiting_exit_reaps_before_propagating_cancellation() {
    let supervisor = Arc::new(ProcessSupervisor::start());
    let inner_supervisor = Arc::clone(&supervisor);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let inner = tokio::spawn(async move {
        let ready = inner_supervisor
            .spawn(test_binary(), credentials(READY_DELAYED_EXIT))
            .await
            .unwrap();
        spawned_tx.send(ready.child_id).unwrap();
        inner_supervisor.wait(ready.child_id).await
    });
    let child_id = spawned_rx.await.unwrap();
    inner.abort();
    let completion = inner.await;
    let supervisor = Arc::try_unwrap(supervisor).ok().unwrap();
    let owner = PendingOuterOwner {
        supervisor,
        completion,
    };
    let reaped = owner.shutdown().await.unwrap();
    let exits = reaped.exits();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert_eq!(exits[0].code, Some(19));
    let cancellation = reaped.into_join_error();
    assert!(cancellation.is_cancelled());
}

#[tokio::test]
async fn panic_after_spawn_reaps_before_propagating_panic() {
    let supervisor = Arc::new(ProcessSupervisor::start());
    let inner_supervisor = Arc::clone(&supervisor);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let inner = tokio::spawn(async move {
        let ready = inner_supervisor
            .spawn(test_binary(), credentials(READY_UNTIL_STDIN_CLOSES))
            .await
            .unwrap();
        spawned_tx.send(ready.child_id).unwrap();
        panic!("injected post-spawn panic");
    });
    let child_id = spawned_rx.await.unwrap();
    let completion = inner.await;
    let supervisor = Arc::try_unwrap(supervisor).ok().unwrap();
    let owner = PendingOuterOwner {
        supervisor,
        completion,
    };
    let reaped = owner.shutdown().await.unwrap();
    let exits = reaped.exits();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert_eq!(exits[0].code, Some(0));
    let panic = reaped.into_join_error();
    assert!(panic.is_panic());
}
