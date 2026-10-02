# Plan: self-heal retires a local worker only on a refused endpoint (#667)

Goal: a starting `run-local` supervisor's self-heal retires a same-kind row only on a
refused endpoint, or on an endpoint-less row older than the startup grace, and a peer
retirement race does not fail startup, per
`docs/workflow/specs/2026-10-01-self-heal-liveness-evidence-design.md` and ADR 0102.
Architecture: `local_worker.rs` keeps ownership of the destructive self-heal verdict and
replaces the dispatch handshake probe with a refused-connect check over the kind's recorded
endpoints plus an age check for endpoint-less rows. `compliance.rs` only exposes
`runtime_metadata`; dispatch liveness is untouched.
Tech stack: Rust 2024, tokio, sqlx/SQLite.

Expected implementation size: 150–200 changed lines (M) — self-heal rewrite and three
helpers (~70), manifest feature and visibility (2), five unit tests and fixtures (~100),
lifecycle-test wait (~15), harness message (~4).

## Global Constraints

- Sibling unit tests only (`local_worker_test.rs`, linked by the existing `#[path]`);
  `just check-test-layout` enforces it.
- Production code: no `unwrap`/`expect`/`panic` (workspace lints, pedantic clippy).
- DB-touching tests run on real time; never `tokio::time::pause` (ADR 0012).
- No new dependency. tokio's `net` feature is added to `voom-control-plane`'s existing
  tokio entry; it is already enabled in the build through `voom-worker-protocol`, so
  `Cargo.lock` does not change.
- Do not edit `probe_live_runtimes`, `live_policy_runtime_registry`,
  `LIVENESS_PROBE_TIMEOUT`, `reject_dead_endpoint_operations`, or `shutdown_and_retire`.
- Guardrails: `just fmt-check`, `just lint`, `just test`, `just check-adr-index`, `just ci`.

## Task 1 — evidence-based self-heal verdict and peer-race tolerance

Files: modify `crates/voom-control-plane/Cargo.toml`,
`crates/voom-control-plane/src/local_worker.rs`,
`crates/voom-control-plane/src/cases/policy/compliance.rs` (visibility only),
`crates/voom-control-plane/tests/local_worker_lifecycle.rs`; test
`crates/voom-control-plane/src/local_worker_test.rs`.

Interfaces: consumes existing `ControlPlane::list_worker_inspections`,
`ControlPlane::retire_worker(WorkerId, u64, OffsetDateTime) -> Result<Worker, VoomError>`,
`ControlPlane::get_worker_inspection(WorkerId) -> Result<Option<WorkerInspection>, VoomError>`,
`SqliteWorkerRepo::runtime_capabilities_for_operations(&[TicketOperation])`
(returns `RuntimeWorkerCapability { worker_id, worker_epoch, operation, extra }`),
`compliance::runtime_metadata(&serde_json::Value) -> Result<Option<(SocketAddr, SecretString)>, VoomError>`,
`#[cfg(test)] ControlPlane::register_supervisor_worker(NewWorker) -> Result<Worker, VoomError>`,
`ControlPlane::record_local_worker_registry(kind, WorkerId, &str, SocketAddr, None)`,
`crate::cases::cp()`, `crate::worker_process::random_hex_128()`, and the startup-timeout
constants `STARTUP_TIMEOUT`, `NVIDIA_STARTUP_TIMEOUT`, `VAAPI_STARTUP_TIMEOUT`,
`VIDEOTOOLBOX_STARTUP_TIMEOUT` in `local_worker.rs`. Adds private
`ControlPlane::recorded_endpoints(&self, LocalWorkerKind) -> Result<HashMap<WorkerId, SocketAddr>, VoomError>`,
`ControlPlane::retire_stale_worker(&self, WorkerId, u64, time::OffsetDateTime) -> Result<(), VoomError>`,
free `async fn endpoint_refuses_connections(SocketAddr) -> bool`, and constants
`SELF_HEAL_CONNECT_TIMEOUT`, `UNRECORDED_ENDPOINT_GRACE`.

Verification:
- Live sibling kept — Mode: focused-test.
  `self_heal_keeps_a_row_whose_endpoint_accepts_but_never_answers`; red at base: status
  `Retired`.
- Dead row retired — Mode: focused-test. `self_heal_retires_a_row_whose_endpoint_refuses`
  and lifecycle `start_local_worker_self_heals_a_stale_same_name_worker`.
- Endpoint-less grace — Mode: focused-test. `self_heal_keeps_a_fresh_row_without_an_endpoint`
  (red at base: retired), `self_heal_retires_a_row_without_an_endpoint_after_the_grace`,
  `unrecorded_endpoint_grace_outlasts_every_startup_timeout`.
- Peer race — Mode: focused-test. `retire_stale_worker_accepts_a_row_a_peer_already_retired`;
  red with `retire_stale_worker` as a plain `retire_worker` call (`CONFLICT … already
  retired`).
- Focused command for the four entries above:
  `cargo test -p voom-control-plane --lib -- self_heal_ retire_stale_worker_ grace` and
  `cargo test -p voom-control-plane --test local_worker_lifecycle`.
- Dispatch unchanged — Mode: focused-test. Existing tests stay green unmodified:
  `cargo test -p voom-control-plane --lib -- live_policy_runtime_registry_drops no_live_worker_for_remux`.

Steps:

1. In `crates/voom-control-plane/Cargo.toml` change the tokio line to
   `tokio = { workspace = true, features = ["fs", "io-util", "net", "process", "rt", "sync", "time"] }`.

2. In `local_worker_test.rs` add the imports

   ```rust
   use std::net::SocketAddr;
   use voom_core::{WorkerId, WorkerKind, WorkerStatus};
   use voom_store::repo::execution::workers::{NewWorker, Worker};
   use crate::ControlPlane;
   ```

   extend the existing `use super::{…}` list with `NVIDIA_STARTUP_TIMEOUT, STARTUP_TIMEOUT,
   UNRECORDED_ENDPOINT_GRACE`, and append:

   ```rust
   async fn register_local_row(
       cp: &ControlPlane,
       kind: LocalWorkerKind,
       registered_at: time::OffsetDateTime,
       endpoint: Option<SocketAddr>,
   ) -> Worker {
       let worker = cp
           .register_supervisor_worker(NewWorker {
               name: format!("{}-{}", kind.base_name(), crate::worker_process::random_hex_128()),
               kind: WorkerKind::Local,
               registered_at,
               node_id: None,
           })
           .await
           .unwrap();
       if let Some(endpoint) = endpoint {
           cp.record_local_worker_registry(kind, worker.id, "s3cret", endpoint, None)
               .await
               .unwrap();
       }
       worker
   }

   async fn worker_status(cp: &ControlPlane, id: WorkerId) -> WorkerStatus {
       cp.get_worker_inspection(id).await.unwrap().unwrap().worker.status
   }

   /// A loopback address with nothing bound to it: bind an ephemeral port, then release
   /// it. Holding the port bound without listening would refuse on Linux, but macOS drops
   /// the SYN for a bound socket instead of resetting it.
   fn closed_endpoint() -> SocketAddr {
       std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
   }

   #[tokio::test]
   async fn self_heal_keeps_a_row_whose_endpoint_accepts_but_never_answers() {
       // A busy or SIGSTOPped live worker still owns its listener, so the kernel
       // accepts connections it never answers. Retiring that row makes the worker
       // undispatchable for life and fails its supervisor's shutdown (#667).
       let (cp, _tmp) = crate::cases::cp().await;
       let stalled = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
       let kind = LocalWorkerKind::Mkvtoolnix;
       let endpoint = Some(stalled.local_addr().unwrap());
       let worker = register_local_row(&cp, kind, cp.clock().now(), endpoint).await;

       cp.self_heal_stale_workers(kind).await.unwrap();

       assert_ne!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }

   #[tokio::test]
   async fn self_heal_retires_a_row_whose_endpoint_refuses() {
       // A hard-killed supervisor's worker leaves a closed port: that row is stale
       // and must not accumulate.
       let (cp, _tmp) = crate::cases::cp().await;
       let endpoint = closed_endpoint();
       let kind = LocalWorkerKind::Mkvtoolnix;
       let worker = register_local_row(&cp, kind, cp.clock().now(), Some(endpoint)).await;

       cp.self_heal_stale_workers(kind).await.unwrap();

       assert_eq!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }

   #[tokio::test]
   async fn self_heal_keeps_a_fresh_row_without_an_endpoint() {
       // A sibling between registering its row and recording its endpoint is live;
       // retiring it reproduces #667 by another path.
       let (cp, _tmp) = crate::cases::cp().await;
       let kind = LocalWorkerKind::Mkvtoolnix;
       let worker = register_local_row(&cp, kind, cp.clock().now(), None).await;

       cp.self_heal_stale_workers(kind).await.unwrap();

       assert_ne!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }

   #[tokio::test]
   async fn self_heal_retires_a_row_without_an_endpoint_after_the_grace() {
       // Past every startup deadline no supervisor is still starting the row, so a
       // supervisor killed mid-startup does not leave it registered forever.
       let (cp, _tmp) = crate::cases::cp().await;
       let kind = LocalWorkerKind::Mkvtoolnix;
       let registered_at = cp.clock().now() - UNRECORDED_ENDPOINT_GRACE - Duration::from_secs(1);
       let worker = register_local_row(&cp, kind, registered_at, None).await;

       cp.self_heal_stale_workers(kind).await.unwrap();

       assert_eq!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }

   #[test]
   fn unrecorded_endpoint_grace_outlasts_every_startup_timeout() {
       // The grace is only evidence of an abandoned row if every supervisor has given
       // up starting by then, with room left for its registry writes.
       let longest = [
           STARTUP_TIMEOUT,
           NVIDIA_STARTUP_TIMEOUT,
           VAAPI_STARTUP_TIMEOUT,
           VIDEOTOOLBOX_STARTUP_TIMEOUT,
       ]
       .into_iter()
       .max()
       .unwrap();
       assert!(UNRECORDED_ENDPOINT_GRACE >= longest + Duration::from_secs(300));
   }

   #[tokio::test]
   async fn retire_stale_worker_accepts_a_row_a_peer_already_retired() {
       // Two starters can both judge one stale row dead; the slower one must not
       // fail its own startup because the faster one retired the row first.
       let (cp, _tmp) = crate::cases::cp().await;
       let kind = LocalWorkerKind::Mkvtoolnix;
       let worker = register_local_row(&cp, kind, cp.clock().now(), None).await;
       let now = cp.clock().now();
       cp.retire_worker(worker.id, worker.epoch, now).await.unwrap();

       cp.retire_stale_worker(worker.id, worker.epoch, now)
           .await
           .unwrap();

       assert_eq!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }
   ```

3. Add the two constants (step 5) and a first-cut `retire_stale_worker` that only calls
   `retire_worker`, so the file compiles against the old self-heal. Run
   `cargo test -p voom-control-plane --lib -- self_heal_ retire_stale_worker_ grace`.
   Expect failures in `self_heal_keeps_a_row_whose_endpoint_accepts_but_never_answers`,
   `self_heal_keeps_a_fresh_row_without_an_endpoint` (both: status `Retired`) and
   `retire_stale_worker_accepts_a_row_a_peer_already_retired` (`already retired`); the
   other three pass.

4. In `compliance.rs` change `fn runtime_metadata(` to `pub(crate) fn runtime_metadata(`.

5. In `local_worker.rs` add `use std::collections::HashMap;`,
   `use tokio::net::TcpStream;`, `use crate::cases::policy::compliance::runtime_metadata;`,
   and beside `SELF_HEAL_SCAN_LIMIT`:

   ```rust
   /// Bound on the refused-connect check. Only an inconclusive endpoint waits this long;
   /// a closed loopback port refuses at once (ADR 0102).
   const SELF_HEAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
   /// Age after which a row with no recorded endpoint has no supervisor still starting it:
   /// every startup deadline plus its registry writes ends well within it (ADR 0102).
   const UNRECORDED_ENDPOINT_GRACE: Duration = Duration::from_mins(15);
   ```

   Replace `self_heal_stale_workers` and add the helpers:

   ```rust
   /// Retire same-kind rows a hard-killed supervisor left behind. A row with a recorded
   /// endpoint is retired only when that endpoint refuses connections: a busy or stopped
   /// live worker still accepts them, and the dispatch handshake probe cannot tell the two
   /// apart. A row without one is retired only once no supervisor can still be starting it
   /// (ADR 0102).
   async fn self_heal_stale_workers(&self, kind: LocalWorkerKind) -> Result<(), VoomError> {
       let endpoints = self.recorded_endpoints(kind).await?;
       let inspections = self
           .list_worker_inspections(None, SELF_HEAL_SCAN_LIMIT)
           .await?;
       let now = self.clock().now();
       let prefix = format!("{}-", kind.base_name());
       for inspection in inspections {
           let worker = inspection.worker;
           if worker.name != kind.base_name() && !worker.name.starts_with(&prefix) {
               continue;
           }
           if !matches!(
               worker.status,
               WorkerStatus::Registered | WorkerStatus::Active
           ) {
               continue;
           }
           let stale = match endpoints.get(&worker.id) {
               Some(endpoint) => endpoint_refuses_connections(*endpoint).await,
               None => worker.registered_at + UNRECORDED_ENDPOINT_GRACE < now,
           };
           if stale {
               self.retire_stale_worker(worker.id, worker.epoch, now).await?;
           }
       }
       Ok(())
   }

   async fn recorded_endpoints(
       &self,
       kind: LocalWorkerKind,
   ) -> Result<HashMap<WorkerId, SocketAddr>, VoomError> {
       let operations: Vec<TicketOperation> = kind
           .operations()
           .iter()
           .copied()
           .map(TicketOperation::from)
           .collect();
       let mut endpoints = HashMap::new();
       for capability in self
           .workers
           .runtime_capabilities_for_operations(&operations)
           .await?
       {
           if let Some((endpoint, _secret)) = runtime_metadata(&capability.extra)? {
               endpoints.insert(capability.worker_id, endpoint);
           }
       }
       Ok(endpoints)
   }

   /// Retire a row self-heal judged stale. Worker epochs advance only on retirement, so a
   /// conflict on a row that now reads retired means a starting peer retired it first.
   async fn retire_stale_worker(
       &self,
       id: WorkerId,
       epoch: u64,
       now: time::OffsetDateTime,
   ) -> Result<(), VoomError> {
       match self.retire_worker(id, epoch, now).await {
           Ok(_) => Ok(()),
           Err(error @ VoomError::Conflict(_)) => {
               let retired = self
                   .get_worker_inspection(id)
                   .await?
                   .is_some_and(|inspection| inspection.worker.status == WorkerStatus::Retired);
               if retired { Ok(()) } else { Err(error) }
           }
           Err(error) => Err(error),
       }
   }
   ```

   and, as a free function after `current_epoch`:

   ```rust
   /// Positive evidence that nothing listens on a recorded endpoint (ADR 0102). A
   /// successful connect, a timeout, or any other error is inconclusive.
   async fn endpoint_refuses_connections(endpoint: SocketAddr) -> bool {
       matches!(
           timeout(SELF_HEAL_CONNECT_TIMEOUT, TcpStream::connect(endpoint)).await,
           Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused
       )
   }
   ```

   If clippy's arithmetic lints reject `registered_at + UNRECORDED_ENDPOINT_GRACE`, use
   `worker.registered_at.checked_add(…)` with `time::Duration::try_from(UNRECORDED_ENDPOINT_GRACE)`
   and treat an overflow as not stale.

6. In `tests/local_worker_lifecycle.rs`, in
   `start_local_worker_self_heals_a_stale_same_name_worker`, replace `drop(first);` with

   ```rust
   let first_endpoint = first.handle().endpoint;
   drop(first);
   wait_until_closed(first_endpoint).await;
   ```

   and add after `live_worker_ids`:

   ```rust
   /// `drop` sends SIGKILL without waiting for the exit. A hard-killed supervisor's worker
   /// is a closed port once the process is gone; wait for that state before restarting.
   async fn wait_until_closed(endpoint: std::net::SocketAddr) {
       let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
       while TcpStream::connect(endpoint).await.is_ok() {
           assert!(
               tokio::time::Instant::now() < deadline,
               "killed worker still accepts connections on {endpoint}"
           );
           tokio::time::sleep(Duration::from_millis(20)).await;
       }
   }
   ```

7. Run `cargo test -p voom-control-plane --lib -- self_heal_ retire_stale_worker_ grace` —
   expect 6 passed. Run the dispatch-unchanged command — expect 2 passed. Run
   `cargo test -p voom-control-plane --test local_worker_lifecycle` — expect 4 passed.
8. `just fmt-check && just lint`; commit `fix(control-plane): retire self-healed local
   workers only on evidence of death (#667)`.

Acceptance: the verification entries are green; the `compliance.rs` diff is the single
visibility change; `Cargo.lock` is unchanged.

## Task 2 — harness surfaces the stdout envelope

Files: modify `crates/voom-cli/tests/run_local_stdout_contract.rs`.

Verification:
- Harness message — Mode: task-test-not-applicable. Changed surface: the panic message of
  test helper `finish_shutdown`; no executable consumer reads it, and a test of its text
  would snapshot prose.

Steps:

1. In `finish_shutdown`, replace the nonzero-exit assertion with:

   ```rust
   assert!(
       status.success(),
       "run-local {} exited nonzero ({status}); stdout:\n{}\nstderr:\n{}",
       self.kind,
       self.stdout_lines.join("\n"),
       self.stderr_snapshot()
   );
   ```

2. Run `cargo test -p voom-cli --test run_local_stdout_contract` — expect 2 passed.
3. `just fmt-check && just lint`; commit `test(cli): print run-local stdout on a nonzero
   shutdown exit (#667)`.

## Task 3 — full gate

The ADR 0102 index row is already in `docs/adr/README.md` from the design commit.

1. `just check-adr-index` — expect `check-adr-index: OK`.
2. `just ci` — expect exit 0.

## Deferrals

None.
