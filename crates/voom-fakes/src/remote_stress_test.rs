use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use secrecy::ExposeSecret;
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use voom_api::router_with_control_plane;
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::clock_test_support::ManualClock;
use voom_core::{Clock, OperationKind, TicketId, TicketOperation};
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::repo::execution::leases::{Lease, LeaseFilter, LeaseState, SqliteLeaseRepo};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::scheduler_node_limits::SqliteSchedulerNodeLimitRepo;
use voom_store::repo::execution::tickets::{
    NewTicket, SqliteTicketRepo, Ticket, TicketFilter, TicketState,
};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_worker_protocol::WorkerCredentials;

use crate::process_supervisor::{
    ChildExit, ChildId, ProcessSupervisor, ProcessSupervisorError, ProcessSupervisorMilestone,
};
use crate::remote_runner::{
    ExecutionAction, ExecutionRecord, ProcessCrashObservation, RemoteExecutionState,
    RemoteFaultPolicy, RemoteNodeSession, RemoteNodeSessionConfig, RemoteRunnerConfig,
    RemoteSyntheticRunner, RemoteWorkerConfig,
};

const STRESS_START: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const EVENT_PAGE_SIZE: u32 = 137;

#[derive(Debug, Clone, PartialEq, Eq)]
struct StressConfig {
    nodes: usize,
    runners_per_node: usize,
    max_parallel: u32,
    tickets: usize,
    dependency_percent: u8,
    stall_percent: u8,
    crash_percent: u8,
    process_crash_percent: u8,
    seed: u64,
    drain_timeout: StdDuration,
}

impl Default for StressConfig {
    fn default() -> Self {
        Self {
            nodes: 4,
            runners_per_node: 8,
            max_parallel: 2,
            tickets: 1_000,
            dependency_percent: 20,
            stall_percent: 0,
            crash_percent: 0,
            process_crash_percent: 0,
            seed: 581,
            drain_timeout: StdDuration::from_mins(2),
        }
    }
}

fn stress_config_from_env() -> Result<StressConfig, String> {
    stress_config_from_lookup(|name| match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("{name} could not be read: {error}")),
    })
}

fn stress_config_from_lookup(
    lookup: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<StressConfig, String> {
    let defaults = StressConfig::default();
    let stall_percent = config_number(
        &lookup,
        "VOOM_STRESS_STALL_PERCENT",
        defaults.stall_percent,
        0,
        25,
    )?;
    let crash_percent = config_number(
        &lookup,
        "VOOM_STRESS_CRASH_PERCENT",
        defaults.crash_percent,
        0,
        25,
    )?;
    let process_crash_percent = config_number(
        &lookup,
        "VOOM_STRESS_PROCESS_CRASH_PERCENT",
        defaults.process_crash_percent,
        0,
        25,
    )?;
    if stall_percent
        .saturating_add(crash_percent)
        .saturating_add(process_crash_percent)
        > 25
    {
        return Err("VOOM_STRESS_STALL_PERCENT + VOOM_STRESS_CRASH_PERCENT + \
             VOOM_STRESS_PROCESS_CRASH_PERCENT must be <= 25"
            .to_owned());
    }
    Ok(StressConfig {
        nodes: config_number(&lookup, "VOOM_STRESS_NODES", defaults.nodes, 1, 32)?,
        runners_per_node: config_number(
            &lookup,
            "VOOM_STRESS_RUNNERS_PER_NODE",
            defaults.runners_per_node,
            1,
            32,
        )?,
        max_parallel: config_number(
            &lookup,
            "VOOM_STRESS_MAX_PARALLEL",
            defaults.max_parallel,
            2,
            16,
        )?,
        tickets: config_number(&lookup, "VOOM_STRESS_TICKETS", defaults.tickets, 1, 10_000)?,
        dependency_percent: config_number(
            &lookup,
            "VOOM_STRESS_DEPENDENCY_PERCENT",
            defaults.dependency_percent,
            0,
            90,
        )?,
        stall_percent,
        crash_percent,
        process_crash_percent,
        seed: config_number(&lookup, "VOOM_STRESS_SEED", defaults.seed, 0, u64::MAX)?,
        drain_timeout: StdDuration::from_secs(config_number(
            &lookup,
            "VOOM_STRESS_DRAIN_SECONDS",
            defaults.drain_timeout.as_secs(),
            1,
            600,
        )?),
    })
}

fn process_crash_count(tickets: usize, percent: u8) -> usize {
    if percent == 0 || tickets == 0 {
        0
    } else {
        (tickets.saturating_mul(usize::from(percent)) / 100)
            .max(1)
            .min(tickets)
    }
}

fn config_number<T>(
    lookup: &impl Fn(&str) -> Result<Option<String>, String>,
    name: &str,
    default: T,
    min: T,
    max: T,
) -> Result<T, String>
where
    T: Copy + Ord + std::str::FromStr + std::fmt::Display,
{
    let value = match lookup(name)? {
        Some(raw) => raw
            .parse::<T>()
            .map_err(|_| format!("{name} must be an integer in {min}..={max}, got {raw:?}"))?,
        None => default,
    };
    if value < min || value > max {
        return Err(format!("{name} must be in {min}..={max}, got {value}"));
    }
    Ok(value)
}

fn select_fault(
    seed: u64,
    ticket_id: TicketId,
    acquisition_ordinal: u32,
    stall_percent: u8,
    crash_percent: u8,
) -> ExecutionAction {
    let mut input = Vec::with_capacity(20);
    input.extend_from_slice(&seed.to_le_bytes());
    input.extend_from_slice(&ticket_id.0.to_le_bytes());
    input.extend_from_slice(&acquisition_ordinal.to_le_bytes());
    let bucket = u16::from(blake3::hash(&input).as_bytes()[0]) * 100 / 256;
    if acquisition_ordinal == 1 && bucket < u16::from(crash_percent) {
        ExecutionAction::Abandoned
    } else if bucket < u16::from(crash_percent.saturating_add(stall_percent)) {
        ExecutionAction::StalledThenCompleted
    } else {
        ExecutionAction::Completed
    }
}

#[derive(Debug)]
struct StressFaultPolicy(StressConfig);

impl RemoteFaultPolicy for StressFaultPolicy {
    fn action(&self, ticket_id: TicketId, acquisition_ordinal: u32) -> ExecutionAction {
        select_fault(
            self.0.seed,
            ticket_id,
            acquisition_ordinal,
            self.0.stall_percent,
            self.0.crash_percent,
        )
    }
}

#[derive(Debug, Clone)]
struct EventObservation {
    id: u64,
    kind: String,
    subject_id: Option<u64>,
    payload: Value,
}

#[derive(Debug)]
struct ConservationInput {
    seeded: Vec<TicketId>,
    tickets: Vec<Ticket>,
    leases: Vec<Lease>,
    events: Vec<EventObservation>,
    executions: Vec<ExecutionRecord>,
    dependencies: Vec<(TicketId, TicketId)>,
}

#[expect(
    clippy::too_many_lines,
    reason = "one accumulator keeps every conservation mismatch in a single deterministic report"
)]
fn assert_conservation(input: &ConservationInput) -> Result<(), String> {
    let ticket_by_id = input
        .tickets
        .iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let lease_by_id = input
        .leases
        .iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let mut errors = Vec::new();
    let mut records_by_ticket: HashMap<TicketId, Vec<&ExecutionRecord>> = HashMap::new();
    for record in &input.executions {
        records_by_ticket
            .entry(record.ticket_id)
            .or_default()
            .push(record);
        match lease_by_id.get(&record.lease_id) {
            Some(lease)
                if lease.ticket_id == record.ticket_id && lease.worker_id == record.worker_id => {}
            _ => errors.push(format!(
                "execution {:?} does not match its durable lease",
                record.lease_id
            )),
        }
    }
    for ticket_id in &input.seeded {
        let Some(ticket) = ticket_by_id.get(ticket_id) else {
            errors.push(format!("ticket {ticket_id} is missing"));
            continue;
        };
        if !matches!(ticket.state, TicketState::Succeeded | TicketState::Failed) {
            errors.push(format!(
                "ticket {ticket_id} is not terminal: {:?}",
                ticket.state
            ));
        }
        let records = records_by_ticket
            .get(ticket_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let acquired_events = input
            .events
            .iter()
            .filter(|event| {
                event.kind == "lease.acquired"
                    && payload_id(&event.payload, "ticket_id") == Some(ticket_id.0)
            })
            .count();
        if usize::try_from(ticket.attempt).ok() != Some(records.len())
            || records.len() != acquired_events
        {
            errors.push(format!(
                "ticket {ticket_id} attempt/log/event mismatch: attempt={} log={} events={acquired_events}",
                ticket.attempt,
                records.len()
            ));
        }
        let non_abandoned = records
            .iter()
            .filter(|record| record.action != ExecutionAction::Abandoned)
            .count();
        if non_abandoned > 1 {
            errors.push(format!(
                "ticket {ticket_id} has duplicate non-abandoned execution"
            ));
        }
        let expected_kind = match ticket.state {
            TicketState::Succeeded => "ticket.succeeded",
            TicketState::Failed => "ticket.failed_terminal",
            _ => continue,
        };
        let terminal_events = input
            .events
            .iter()
            .filter(|event| {
                event.subject_id == Some(ticket_id.0)
                    && matches!(
                        event.kind.as_str(),
                        "ticket.succeeded" | "ticket.failed_terminal"
                    )
            })
            .collect::<Vec<_>>();
        if terminal_events.len() != 1 || terminal_events[0].kind != expected_kind {
            errors.push(format!(
                "ticket {ticket_id} terminal event does not match {:?}",
                ticket.state
            ));
        }
    }
    for lease in &input.leases {
        if lease.state == LeaseState::Held {
            errors.push(format!("lease {} remains held", lease.id));
        }
        let terminal_count = input
            .events
            .iter()
            .filter(|event| {
                matches!(event.kind.as_str(), "lease.released" | "lease.expired")
                    && payload_id(&event.payload, "lease_id") == Some(lease.id.0)
            })
            .count();
        if terminal_count != 1 {
            errors.push(format!(
                "lease {} has {terminal_count} terminal events",
                lease.id
            ));
        }
    }
    let duplicate_lease_workers =
        input
            .executions
            .iter()
            .fold(HashMap::<_, HashSet<_>>::new(), |mut workers, record| {
                workers
                    .entry(record.lease_id)
                    .or_default()
                    .insert(record.worker_id);
                workers
            });
    for (lease_id, workers) in duplicate_lease_workers {
        if workers.len() > 1 {
            errors.push(format!("lease {lease_id} appears under multiple workers"));
        }
    }
    for (dependent, prerequisite) in &input.dependencies {
        let prerequisite_event = event_id(&input.events, "ticket.succeeded", prerequisite.0);
        let dependent_acquire = input
            .events
            .iter()
            .filter(|event| {
                event.kind == "lease.acquired"
                    && payload_id(&event.payload, "ticket_id") == Some(dependent.0)
            })
            .map(|event| event.id)
            .min();
        if prerequisite_event
            .zip(dependent_acquire)
            .is_none_or(|(success, acquire)| success >= acquire)
        {
            errors.push(format!(
                "dependency order violated: ticket {dependent} acquired before prerequisite {prerequisite} succeeded"
            ));
        }
    }
    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn payload_id(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64).or_else(|| {
        payload
            .as_object()?
            .values()
            .find_map(|inner| inner.get(field).and_then(Value::as_u64))
    })
}

fn event_id(events: &[EventObservation], kind: &str, subject_id: u64) -> Option<u64> {
    events
        .iter()
        .find(|event| event.kind == kind && event.subject_id == Some(subject_id))
        .map(|event| event.id)
}

#[test]
fn stress_fault_selection_is_deterministic_and_retries_do_not_crash() {
    let first = select_fault(581, TicketId(9), 1, 5, 25);
    assert_eq!(first, select_fault(581, TicketId(9), 1, 5, 25));
    assert_ne!(
        select_fault(581, TicketId(9), 2, 0, 100),
        ExecutionAction::Abandoned
    );
    assert_eq!(
        select_fault(581, TicketId(9), 1, 0, 0),
        ExecutionAction::Completed
    );
}

#[test]
fn stress_config_rejects_out_of_range_values() {
    let error =
        stress_config_from_lookup(|name| Ok((name == "VOOM_STRESS_NODES").then(|| "0".to_owned())))
            .unwrap_err();
    assert!(error.contains("VOOM_STRESS_NODES"));
}

#[test]
fn stress_process_config_defaults_to_zero() {
    let config = stress_config_from_lookup(|_| Ok(None)).unwrap();

    assert_eq!(config.process_crash_percent, 0);
}

#[test]
fn stress_process_config_accepts_its_bounds_and_rejects_above_maximum() {
    for accepted in [0, 25] {
        let config = stress_config_from_lookup(|name| {
            Ok((name == "VOOM_STRESS_PROCESS_CRASH_PERCENT").then(|| accepted.to_string()))
        })
        .unwrap();
        assert_eq!(config.process_crash_percent, accepted);
    }

    let error = stress_config_from_lookup(|name| {
        Ok((name == "VOOM_STRESS_PROCESS_CRASH_PERCENT").then(|| "26".to_owned()))
    })
    .unwrap_err();
    assert!(error.contains("VOOM_STRESS_PROCESS_CRASH_PERCENT"));
}

#[test]
fn stress_process_config_limits_the_three_way_fault_sum() {
    let accepted = stress_config_from_lookup(|name| {
        Ok(match name {
            "VOOM_STRESS_STALL_PERCENT" => Some("5".to_owned()),
            "VOOM_STRESS_CRASH_PERCENT" | "VOOM_STRESS_PROCESS_CRASH_PERCENT" => {
                Some("10".to_owned())
            }
            _ => None,
        })
    })
    .unwrap();
    assert_eq!(accepted.process_crash_percent, 10);

    let error = stress_config_from_lookup(|name| {
        Ok(match name {
            "VOOM_STRESS_STALL_PERCENT" => Some("5".to_owned()),
            "VOOM_STRESS_CRASH_PERCENT" => Some("10".to_owned()),
            "VOOM_STRESS_PROCESS_CRASH_PERCENT" => Some("11".to_owned()),
            _ => None,
        })
    })
    .unwrap_err();
    assert!(error.contains("must be <= 25"));
}

#[test]
fn stress_process_count_rounds_down_with_a_nonzero_minimum() {
    assert_eq!(process_crash_count(8, 25), 2);
    assert_eq!(process_crash_count(7, 25), 1);
    assert_eq!(process_crash_count(1, 1), 1);
    assert_eq!(process_crash_count(10_000, 25), 2_500);
    assert_eq!(process_crash_count(10_000, 0), 0);
}

#[tokio::test]
async fn stress_process_seed_keeps_only_the_selected_prefix_ready_until_release() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    let config = StressConfig {
        tickets: 4,
        dependency_percent: 90,
        process_crash_percent: 25,
        ..StressConfig::default()
    };

    let mut workload = seed_workload(&cp, &config).await.unwrap();
    assert_eq!(workload.process_ticket_ids, workload.ticket_ids[..1]);
    let repo = SqliteTicketRepo::new(voom_store::connect(&url).await.unwrap());
    let selected = repo
        .get(workload.process_ticket_ids[0])
        .await
        .unwrap()
        .unwrap();
    let higher_priority_non_selected = repo.get(workload.ticket_ids[3]).await.unwrap().unwrap();
    assert_eq!(selected.state, TicketState::Ready);
    assert_eq!(higher_priority_non_selected.priority, 50);
    assert_eq!(higher_priority_non_selected.state, TicketState::Pending);

    release_remaining_workload(&cp, &config, &mut workload)
        .await
        .unwrap();
    let released_root = repo.get(workload.ticket_ids[1]).await.unwrap().unwrap();
    assert_eq!(released_root.state, TicketState::Ready);
    assert!(
        workload
            .dependencies
            .iter()
            .all(|(dependent, prerequisite)| {
                !workload.process_ticket_ids.contains(dependent)
                    && !workload.process_ticket_ids.contains(prerequisite)
            })
    );
}

fn process_harness_credentials(mode: u64) -> WorkerCredentials {
    WorkerCredentials {
        worker_id: voom_core::WorkerId(mode),
        worker_epoch: 1,
        secret: "process-supervisor-test".to_owned().into(),
    }
}

struct ProcessHarnessFinished<T> {
    completion: Result<Result<T, String>, tokio::task::JoinError>,
    cleanup: Result<Vec<ChildExit>, ProcessSupervisorError>,
}

async fn finish_process_harness<T>(
    supervisor: Arc<ProcessSupervisor>,
    inner: tokio::task::JoinHandle<Result<T, String>>,
) -> ProcessHarnessFinished<T> {
    let completion = inner.await;
    let cleanup = match Arc::try_unwrap(supervisor) {
        Ok(supervisor) => supervisor.shutdown().await,
        Err(_) => Err(ProcessSupervisorError::Protocol {
            detail: "process harness retained a supervisor owner after its inner task ended"
                .to_owned(),
        }),
    };
    ProcessHarnessFinished {
        completion,
        cleanup,
    }
}

async fn await_registered_readiness(
    milestones: &mut tokio::sync::mpsc::UnboundedReceiver<ProcessSupervisorMilestone>,
) -> ChildId {
    let mut registered = None;
    let mut awaiting_readiness = None;
    while registered.is_none() || awaiting_readiness.is_none() {
        match milestones.recv().await.unwrap() {
            ProcessSupervisorMilestone::ChildRegistered(child_id) => {
                registered = Some(child_id);
            }
            ProcessSupervisorMilestone::AwaitingReadiness(child_id) => {
                awaiting_readiness = Some(child_id);
            }
            ProcessSupervisorMilestone::WaitRegistered(_)
            | ProcessSupervisorMilestone::TombstoneStored(_)
            | ProcessSupervisorMilestone::WatcherCompleted(_)
            | ProcessSupervisorMilestone::RegistryEmpty => {}
        }
    }
    assert_eq!(registered, awaiting_readiness);
    registered.unwrap()
}

async fn await_registered_wait(
    milestones: &mut tokio::sync::mpsc::UnboundedReceiver<ProcessSupervisorMilestone>,
    expected_child_id: ChildId,
) {
    loop {
        if let ProcessSupervisorMilestone::WaitRegistered(child_id) =
            milestones.recv().await.unwrap()
            && child_id == expected_child_id
        {
            return;
        }
    }
}

fn resolve_process_harness<T>(
    finished: ProcessHarnessFinished<T>,
) -> Result<(T, Vec<ChildExit>), String> {
    match (finished.completion, finished.cleanup) {
        (Ok(Ok(value)), Ok(exits)) => Ok((value, exits)),
        (Ok(Err(error)), Ok(_)) => Err(error),
        (Err(join_error), Ok(_)) => Err(format!("process harness inner task: {join_error}")),
        (Ok(Ok(_)), Err(cleanup_error)) => {
            Err(format!("process supervisor cleanup: {cleanup_error}"))
        }
        (Ok(Err(error)), Err(cleanup_error)) => Err(format!(
            "{error}; process supervisor cleanup: {cleanup_error}"
        )),
        (Err(join_error), Err(cleanup_error)) => Err(format!(
            "process harness inner task: {join_error}; process supervisor cleanup: {cleanup_error}"
        )),
    }
}

#[tokio::test]
async fn process_crash_owner_reaps_before_propagating_readiness_cancellation() {
    let (supervisor, mut milestones) = ProcessSupervisor::start_with_test_milestones();
    let supervisor = Arc::new(supervisor);
    let inner_supervisor = Arc::clone(&supervisor);
    let inner = tokio::spawn(async move {
        inner_supervisor
            .spawn(
                std::env::current_exe().unwrap(),
                process_harness_credentials(5),
            )
            .await
            .map_err(|error| error.to_string())
    });
    let child_id = await_registered_readiness(&mut milestones).await;
    inner.abort();

    let finished = finish_process_harness(supervisor, inner).await;

    let exits = finished.cleanup.unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert_eq!(exits[0].code, Some(33));
    assert!(finished.completion.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn process_crash_owner_reaps_before_propagating_exit_cancellation() {
    let (supervisor, mut milestones) = ProcessSupervisor::start_with_test_milestones();
    let supervisor = Arc::new(supervisor);
    let inner_supervisor = Arc::clone(&supervisor);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let inner = tokio::spawn(async move {
        let ready = inner_supervisor
            .spawn(
                std::env::current_exe().unwrap(),
                process_harness_credentials(4),
            )
            .await
            .map_err(|error| error.to_string())?;
        spawned_tx.send(ready.child_id).unwrap();
        inner_supervisor
            .wait(ready.child_id)
            .await
            .map_err(|error| error.to_string())
    });
    let child_id = spawned_rx.await.unwrap();
    await_registered_wait(&mut milestones, child_id).await;
    inner.abort();

    let finished = finish_process_harness(supervisor, inner).await;

    let exits = finished.cleanup.unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert_eq!(exits[0].code, Some(0));
    assert!(finished.completion.unwrap_err().is_cancelled());
}

#[tokio::test]
#[expect(
    clippy::panic,
    reason = "the harness-owner test must propagate a deliberately injected inner-task panic"
)]
async fn process_crash_owner_reaps_before_propagating_post_spawn_panic() {
    let supervisor = Arc::new(ProcessSupervisor::start());
    let inner_supervisor = Arc::clone(&supervisor);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let inner = tokio::spawn(async move {
        let ready = inner_supervisor
            .spawn(
                std::env::current_exe().unwrap(),
                process_harness_credentials(4),
            )
            .await
            .map_err(|error| error.to_string())?;
        spawned_tx.send(ready.child_id).unwrap();
        panic!("injected process harness panic");
        #[expect(
            unreachable_code,
            reason = "the typed task result follows the injected panic"
        )]
        Ok(ready)
    });
    let child_id = spawned_rx.await.unwrap();

    let finished = finish_process_harness(supervisor, inner).await;

    let exits = finished.cleanup.unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert_eq!(exits[0].code, Some(0));
    assert!(finished.completion.unwrap_err().is_panic());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in distributed stress harness; run with just stress"]
#[expect(
    clippy::print_stderr,
    reason = "the opt-in harness prints its effective configuration for exact reproduction"
)]
async fn distributed_stress_conserves_every_ticket() {
    let config = stress_config_from_env().unwrap();
    eprintln!("VOOM stress config: {config:?}");
    run_stress(config).await.unwrap();
}

#[expect(
    clippy::too_many_lines,
    clippy::print_stderr,
    reason = "the opt-in harness owns one lifecycle and reports its reproducible result"
)]
async fn run_stress(config: StressConfig) -> Result<(), String> {
    let process_count = process_crash_count(config.tickets, config.process_crash_percent);
    let process_binary = if process_count == 0 {
        None
    } else {
        Some(
            voom_test_support::worker::cargo_bin_or_build("voom-fakes", "chaos-worker")
                .map_err(|error| error.to_string())?,
        )
    };
    let tmp = TempDatabase::new().map_err(|error| error.to_string())?;
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url)
        .await
        .map_err(|error| error.to_string())?;
    let pool = voom_store::connect(&url)
        .await
        .map_err(|error| error.to_string())?;
    let clock = Arc::new(ManualClock::new(STRESS_START));
    let cp = ControlPlane::open_with_pool(pool.clone(), clock.clone())
        .await
        .map_err(|error| error.to_string())?;
    let health = HealthPlane::open(&url)
        .await
        .map_err(|error| error.to_string())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| error.to_string())?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let server_control_plane = cp.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router_with_control_plane(health, server_control_plane),
        )
        .await
    });

    let mut workload = seed_workload(&cp, &config).await?;
    let executions = Arc::new(RemoteExecutionState::new(Arc::new(StressFaultPolicy(
        config.clone(),
    ))));
    let mut process_runners = Vec::with_capacity(process_count);
    for process_index in 0..process_count {
        let registered = cp
            .register_node(RegisterNodeInput {
                name: format!("stress-process-node-{process_index}"),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 60,
                metadata: json!({"stress": true, "process_crash": true}),
            })
            .await
            .map_err(|error| error.to_string())?;
        SqliteSchedulerNodeLimitRepo::new(pool.clone())
            .set_node_limit(registered.node.id, 1, STRESS_START)
            .await
            .map_err(|error| error.to_string())?;
        process_runners.push(RemoteSyntheticRunner::new(RemoteRunnerConfig {
            base_url: format!("http://{address}"),
            node_id: registered.node.id,
            token: registered.token.expose_secret().to_owned().into(),
            worker_logical_name: format!("stress-process-{process_index}"),
            operations: vec![OperationKind::TranscodeVideo],
            artifact_access: vec!["shared_mount".to_owned()],
            max_parallel: 1,
            max_polls: 1,
            idle_timeout: StdDuration::from_secs(1),
            lease_heartbeat_interval: StdDuration::from_millis(10),
            lease_ttl_seconds: 1,
            healthy_heartbeat_ttl_seconds: 3,
        }));
    }
    let mut process_observations = Vec::with_capacity(process_count);
    if let Some(binary) = process_binary {
        let supervisor = Arc::new(ProcessSupervisor::start());
        let inner_supervisor = Arc::clone(&supervisor);
        let expected = workload
            .process_ticket_ids
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let inner = tokio::spawn(async move {
            run_process_prelude(inner_supervisor, binary, process_runners, expected).await
        });
        let finished = finish_process_harness(supervisor, inner).await;
        let ((records, observations), cleanup_exits) = resolve_process_harness(finished)?;
        if !cleanup_exits.is_empty() {
            return Err(format!(
                "process supervisor retained {} completed children after explicit waits",
                cleanup_exits.len()
            ));
        }
        for record in records {
            executions
                .record_process_crash(record)
                .await
                .map_err(|error| error.to_string())?;
        }
        process_observations = observations;
    }
    let observed_process_tickets = process_observations
        .iter()
        .map(|observation| observation.ticket_id)
        .collect::<HashSet<_>>();
    let selected_process_tickets = workload
        .process_ticket_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    if observed_process_tickets != selected_process_tickets {
        return Err(format!(
            "process ticket observation mismatch: observed={observed_process_tickets:?} \
             selected={selected_process_tickets:?}"
        ));
    }
    release_remaining_workload(&cp, &config, &mut workload).await?;
    let expected_process_leases = process_observations
        .iter()
        .map(|observation| observation.lease_id)
        .collect::<HashSet<_>>();
    let mut recovered = HashSet::new();
    recover_abandoned(&cp, &pool, &clock, &[], &executions, &mut recovered).await?;
    if recovered != expected_process_leases {
        return Err(format!(
            "process lease recovery mismatch: recovered={recovered:?} \
             expected={expected_process_leases:?}"
        ));
    }
    let mut sessions = Vec::with_capacity(config.nodes);
    for node_index in 0..config.nodes {
        let registered = cp
            .register_node(RegisterNodeInput {
                name: format!("stress-node-{node_index}"),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 60,
                metadata: json!({"stress": true}),
            })
            .await
            .map_err(|error| error.to_string())?;
        let node_parallelism = u32::try_from(config.runners_per_node)
            .ok()
            .and_then(|workers| workers.checked_mul(config.max_parallel))
            .ok_or_else(|| "stress node parallelism exceeds u32".to_owned())?;
        SqliteSchedulerNodeLimitRepo::new(pool.clone())
            .set_node_limit(registered.node.id, node_parallelism, STRESS_START)
            .await
            .map_err(|error| error.to_string())?;
        sessions.push(RemoteNodeSession::new(
            RemoteNodeSessionConfig {
                base_url: format!("http://{address}"),
                node_id: registered.node.id,
                token: registered.token.expose_secret().to_owned().into(),
                workers: (0..config.runners_per_node)
                    .map(|runner_index| RemoteWorkerConfig {
                        logical_name: format!("stress-{node_index}-{runner_index}"),
                        operations: vec![OperationKind::TranscodeVideo],
                        artifact_access: vec!["shared_mount".to_owned()],
                        max_parallel: config.max_parallel,
                    })
                    .collect(),
                max_polls: 1,
                idle_timeout: StdDuration::from_secs(1),
                poll_interval: StdDuration::from_millis(10),
                lease_ttl_seconds: 1,
                healthy_heartbeat_ttl_seconds: 3,
            },
            executions.clone(),
        ));
    }
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let tasks = sessions
        .iter()
        .cloned()
        .map(|session| {
            let stop = stop_rx.clone();
            tokio::spawn(async move { session.run_until_stopped(stop).await })
        })
        .collect::<Vec<_>>();
    wait_for_workers(
        &sessions,
        config.nodes * config.runners_per_node,
        config.drain_timeout,
    )
    .await?;
    let deadline = tokio::time::Instant::now() + config.drain_timeout;
    loop {
        recover_abandoned(&cp, &pool, &clock, &sessions, &executions, &mut recovered).await?;
        let tickets = SqliteTicketRepo::new(pool.clone())
            .list(
                TicketFilter::default(),
                None,
                u32::try_from(config.tickets).map_err(|_| "ticket count exceeds u32".to_owned())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        let held = SqliteLeaseRepo::new(pool.clone())
            .list(
                LeaseFilter {
                    state: Some(LeaseState::Held),
                },
                None,
                10_000,
            )
            .await
            .map_err(|error| error.to_string())?;
        if tickets.len() == config.tickets
            && tickets
                .iter()
                .all(|ticket| matches!(ticket.state, TicketState::Succeeded | TicketState::Failed))
            && held.is_empty()
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "stress drain timed out: tickets={} held={} executions={}",
                tickets.len(),
                held.len(),
                executions.records().await.len()
            ));
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    stop_tx.send(true).map_err(|error| error.to_string())?;
    for task in tasks {
        task.await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
    }
    server.abort();
    let _ = server.await;
    let input = collect_conservation(&pool, workload, executions.records().await).await?;
    let retries = input.executions.len().saturating_sub(config.tickets);
    let nonzero_process_exits = process_observations
        .iter()
        .filter(|observation| observation.exit_code.is_none_or(|code| code != 0))
        .count();
    eprintln!(
        "VOOM stress process crashes: selected={process_count} observed={} nonzero_exits={} \
         expired_first_leases={} supervised_children=0",
        process_observations.len(),
        nonzero_process_exits,
        expected_process_leases.len()
    );
    for observation in &process_observations {
        eprintln!("VOOM stress process observation: {observation:?}");
    }
    eprintln!(
        "VOOM stress drained: tickets={} attempts={} retries={retries} held_leases=0",
        config.tickets,
        input.executions.len()
    );
    assert_conservation(&input)
}

async fn run_process_prelude(
    supervisor: Arc<ProcessSupervisor>,
    binary: std::path::PathBuf,
    runners: Vec<RemoteSyntheticRunner>,
    mut expected_tickets: HashSet<TicketId>,
) -> Result<(Vec<ExecutionRecord>, Vec<ProcessCrashObservation>), String> {
    let mut records = Vec::with_capacity(runners.len());
    let mut observations = Vec::with_capacity(runners.len());
    for runner in runners {
        let (record, observation) = runner
            .run_once_to_process_crash(&supervisor, binary.clone(), &expected_tickets)
            .await
            .map_err(|error| error.to_string())?;
        if !expected_tickets.remove(&observation.ticket_id) {
            return Err(format!(
                "process crash ticket {} was observed more than once",
                observation.ticket_id
            ));
        }
        records.push(record);
        observations.push(observation);
    }
    if !expected_tickets.is_empty() {
        return Err(format!(
            "process crash prelude did not observe selected tickets {expected_tickets:?}"
        ));
    }
    Ok((records, observations))
}

#[derive(Debug)]
struct SeededWorkload {
    ticket_ids: Vec<TicketId>,
    process_ticket_ids: Vec<TicketId>,
    dependencies: Vec<(TicketId, TicketId)>,
}

async fn seed_workload(cp: &ControlPlane, config: &StressConfig) -> Result<SeededWorkload, String> {
    let mut ticket_ids = Vec::with_capacity(config.tickets);
    for index in 0..config.tickets {
        let ticket = cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: TicketOperation::new("transcode_video").map_err(|error| error.to_string())?,
                priority: [-10, 0, 10, 50][index % 4],
                payload: stress_payload(index),
                max_attempts: 2,
                created_at: STRESS_START,
            })
            .await
            .map_err(|error| error.to_string())?;
        ticket_ids.push(ticket.id);
    }
    let process_count = process_crash_count(config.tickets, config.process_crash_percent);
    let process_ticket_ids = ticket_ids[..process_count].to_vec();
    for ticket_id in &process_ticket_ids {
        cp.mark_ready_if_unblocked(*ticket_id, STRESS_START)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(SeededWorkload {
        ticket_ids,
        process_ticket_ids,
        dependencies: Vec::new(),
    })
}

async fn release_remaining_workload(
    cp: &ControlPlane,
    config: &StressConfig,
    workload: &mut SeededWorkload,
) -> Result<(), String> {
    let first_remaining = workload.process_ticket_ids.len();
    for index in first_remaining.saturating_add(1)..workload.ticket_ids.len() {
        let bucket = workload_bucket(
            config.seed,
            u64::try_from(index).map_err(|_| "ticket index overflow".to_owned())?,
        );
        if bucket < config.dependency_percent {
            let prerequisite_index =
                first_remaining + usize::from(bucket) % (index - first_remaining);
            cp.tickets()
                .add_dependency(
                    workload.ticket_ids[index],
                    workload.ticket_ids[prerequisite_index],
                )
                .await
                .map_err(|error| error.to_string())?;
            workload.dependencies.push((
                workload.ticket_ids[index],
                workload.ticket_ids[prerequisite_index],
            ));
        }
    }
    for ticket_id in &workload.ticket_ids[first_remaining..] {
        cp.mark_ready_if_unblocked(*ticket_id, STRESS_START)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn workload_bucket(seed: u64, index: u64) -> u8 {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    bytes[8..].copy_from_slice(&index.to_le_bytes());
    u8::try_from(u16::from(blake3::hash(&bytes).as_bytes()[0]) * 100 / 256).unwrap_or(99)
}

fn stress_payload(index: usize) -> Value {
    let output =
        std::env::temp_dir().join(format!("voom-stress-{}-{index}.mkv", std::process::id()));
    json!({
        "input": {"path": format!("/library/stress-{index}.mkv"), "expected": {"size_bytes": 5, "content_hash": "blake3:input"}},
        "output": {"staging_root": output.parent().unwrap().to_string_lossy(), "path": output.to_string_lossy(), "container": "mkv", "video_codec": "hevc", "overwrite": true},
        "profile": {"name": "stress", "target_codec": "hevc", "encoder": "libx265", "crf": 23, "preset": "medium"},
        "artifact_access": {"inputs": ["handle:input:stress"], "outputs": ["handle:output:stress"]}
    })
}

async fn wait_for_workers(
    sessions: &[RemoteNodeSession],
    expected: usize,
    timeout: StdDuration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut total = 0;
        for session in sessions {
            total += session.active_worker_ids().await.len();
        }
        if total == expected {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "worker activation timed out: active={total} expected={expected}"
            ));
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
}

async fn recover_abandoned(
    cp: &ControlPlane,
    pool: &sqlx::SqlitePool,
    clock: &ManualClock,
    sessions: &[RemoteNodeSession],
    executions: &RemoteExecutionState,
    recovered: &mut HashSet<voom_core::LeaseId>,
) -> Result<(), String> {
    let records = executions.records().await;
    let targets = records
        .iter()
        .filter(|record| {
            record.action == ExecutionAction::Abandoned && !recovered.contains(&record.lease_id)
        })
        .map(|record| record.lease_id)
        .collect::<HashSet<_>>();
    if targets.is_empty() {
        return Ok(());
    }
    let mut guards = Vec::with_capacity(sessions.len());
    for session in sessions {
        guards.push(session.recovery_guard().await);
    }
    let held = SqliteLeaseRepo::new(pool.clone())
        .list(
            LeaseFilter {
                state: Some(LeaseState::Held),
            },
            None,
            10_000,
        )
        .await
        .map_err(|error| error.to_string())?;
    let abandoned = held
        .iter()
        .filter(|lease| targets.contains(&lease.id))
        .collect::<Vec<_>>();
    if abandoned.is_empty() {
        return Ok(());
    }
    for lease in held.iter().filter(|lease| !targets.contains(&lease.id)) {
        let record = records
            .iter()
            .find(|record| record.lease_id == lease.id)
            .cloned()
            .unwrap_or(ExecutionRecord {
                ticket_id: lease.ticket_id,
                lease_id: lease.id,
                worker_id: lease.worker_id,
                acquisition_ordinal: 1,
                action: ExecutionAction::Completed,
            });
        let mut refreshed = false;
        for session in sessions {
            if session.active_worker_ids().await.contains(&lease.worker_id) {
                session
                    .heartbeat_execution(&record)
                    .await
                    .map_err(|error| error.to_string())?;
                refreshed = true;
                break;
            }
        }
        if !refreshed {
            return Err(format!("held lease {} has no active session", lease.id));
        }
    }
    let recovery_at = abandoned
        .iter()
        .map(|lease| lease.expires_at)
        .max()
        .ok_or_else(|| "abandoned lease snapshot was empty".to_owned())?
        + Duration::nanoseconds(1);
    if recovery_at <= clock.now() {
        return Err("recovery clock would move backward".to_owned());
    }
    clock.set(recovery_at);
    let report = cp
        .remote_recover(recovery_at)
        .await
        .map_err(|error| error.to_string())?;
    let actual = report
        .expired_leases
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let expected = abandoned
        .iter()
        .map(|lease| lease.id)
        .collect::<HashSet<_>>();
    if !report.stale_nodes.is_empty() || actual != expected {
        return Err(format!(
            "recovery mismatch: stale={:?} expired={actual:?} expected={expected:?}",
            report.stale_nodes
        ));
    }
    executions.mark_recovered(&actual).await;
    recovered.extend(actual);
    drop(guards);
    Ok(())
}

#[expect(
    clippy::print_stderr,
    reason = "the opt-in harness reports when event evidence crosses its logical page size"
)]
async fn collect_conservation(
    pool: &sqlx::SqlitePool,
    workload: SeededWorkload,
    executions: Vec<ExecutionRecord>,
) -> Result<ConservationInput, String> {
    let ticket_limit = u32::try_from(workload.ticket_ids.len())
        .map_err(|_| "ticket count exceeds u32".to_owned())?;
    let tickets = SqliteTicketRepo::new(pool.clone())
        .list(TicketFilter::default(), None, ticket_limit)
        .await
        .map_err(|error| error.to_string())?;
    let leases = SqliteLeaseRepo::new(pool.clone())
        .list(LeaseFilter::default(), None, 10_000)
        .await
        .map_err(|error| error.to_string())?;
    let repo = SqliteEventRepo::new(pool.clone());
    let mut cursor = None;
    let mut events = Vec::new();
    let mut page_count = 0_usize;
    loop {
        let page = repo
            .list(
                EventFilter::default(),
                Page {
                    limit: EVENT_PAGE_SIZE,
                    cursor,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        page_count += 1;
        for row in page.items {
            events.push(EventObservation {
                id: row.id.0,
                kind: row.envelope.payload.kind().as_str().to_owned(),
                subject_id: row.envelope.subject_id,
                payload: serde_json::to_value(row.envelope.payload)
                    .map_err(|error| error.to_string())?,
            });
        }
        let Some(next) = page.next_cursor else {
            break;
        };
        cursor = Some(next);
    }
    if page_count > 1 {
        eprintln!("VOOM stress event evidence spans {page_count} pages");
    }
    Ok(ConservationInput {
        seeded: workload.ticket_ids,
        tickets,
        leases,
        events,
        executions,
        dependencies: workload.dependencies,
    })
}
