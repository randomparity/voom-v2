# Plan: self-heal retires a local worker only on a refused endpoint (#667)

Goal: a starting `run-local` supervisor never retires a live same-kind sibling, and a
peer retirement race never fails startup, per
`docs/workflow/specs/2026-10-01-self-heal-liveness-evidence-design.md` and ADR 0102.
Architecture: `local_worker.rs` keeps ownership of the destructive self-heal verdict and
replaces the dispatch handshake probe with a refused-connect check over the kind's recorded
endpoints. `compliance.rs` only exposes `runtime_metadata`; dispatch liveness is untouched.
Tech stack: Rust 2024, tokio, sqlx/SQLite.

Expected implementation size: 110–150 changed lines (M) — self-heal rewrite and two helpers
(~55), one visibility keyword, three unit tests and two fixtures (~70), harness message (~4).

## Global Constraints

- Sibling unit tests only (`local_worker_test.rs`, linked by the existing `#[path]`);
  `just check-test-layout` enforces it.
- Production code: no `unwrap`/`expect`/`panic` (workspace lints, pedantic clippy).
- DB-touching tests run on real time; never `tokio::time::pause` (ADR 0012).
- No new dependency; `std`, `tokio` (`net` already used by the crate), `time` exist.
- Do not edit `probe_live_runtimes`, `live_policy_runtime_registry`,
  `LIVENESS_PROBE_TIMEOUT`, `reject_dead_endpoint_operations`, or `shutdown_and_retire`.
- Guardrails: `just fmt-check`, `just lint`, `just test`, `just check-adr-index`, `just ci`.

## Task 1 — refused-connect verdict and peer-race tolerance

Files: modify `crates/voom-control-plane/src/local_worker.rs`,
`crates/voom-control-plane/src/cases/policy/compliance.rs` (visibility only); test
`crates/voom-control-plane/src/local_worker_test.rs`.

Interfaces: consumes existing `ControlPlane::list_worker_inspections`,
`ControlPlane::retire_worker(WorkerId, u64, OffsetDateTime) -> Result<Worker, VoomError>`,
`ControlPlane::get_worker_inspection(WorkerId) -> Result<Option<WorkerInspection>, VoomError>`,
`SqliteWorkerRepo::runtime_capabilities_for_operations(&[TicketOperation])`,
`compliance::runtime_metadata(&serde_json::Value) -> Result<Option<(SocketAddr, SecretString)>, VoomError>`,
`#[cfg(test)] ControlPlane::register_supervisor_worker(NewWorker)`,
`ControlPlane::record_local_worker_registry(kind, WorkerId, &str, SocketAddr, None)`,
`crate::cases::cp()`. Adds private `ControlPlane::recorded_endpoints(&self, LocalWorkerKind)
-> Result<HashMap<WorkerId, SocketAddr>, VoomError>`, `ControlPlane::retire_stale_worker(&self,
WorkerId, u64, time::OffsetDateTime) -> Result<(), VoomError>`, and free
`async fn endpoint_refuses_connections(SocketAddr) -> bool`.

Verification:
- Live sibling kept — Mode: focused-test.
  `self_heal_keeps_a_row_whose_endpoint_accepts_but_never_answers`; red at base: status
  `Retired`; green: `cargo test -p voom-control-plane --lib self_heal_`.
- Dead row retired — Mode: focused-test. `self_heal_retires_a_row_whose_endpoint_refuses`;
  green with the same command (it also passes at base, guarding the self-heal purpose).
- Peer race — Mode: focused-test. `retire_stale_worker_accepts_a_row_a_peer_already_retired`;
  red with `retire_stale_worker` as a plain `retire_worker` call (`CONFLICT … already
  retired`); green: `cargo test -p voom-control-plane --lib retire_stale_worker_`.
- Dispatch unchanged — Mode: focused-test. Existing tests stay green unmodified:
  `cargo test -p voom-control-plane --lib -- live_policy_runtime_registry_drops no_live_worker_for_remux`.

Steps:

1. In `local_worker_test.rs` add these imports:

   ```rust
   use std::net::SocketAddr;
   use voom_core::{WorkerId, WorkerKind, WorkerStatus};
   use voom_store::repo::execution::workers::{NewWorker, Worker};
   use crate::ControlPlane;
   ```

   and append the fixtures and tests:

   ```rust
   async fn register_local_row(
       cp: &ControlPlane,
       kind: LocalWorkerKind,
       endpoint: SocketAddr,
   ) -> Worker {
       let worker = cp
           .register_supervisor_worker(NewWorker {
               name: format!("{}-{}", kind.base_name(), crate::worker_process::random_hex_128()),
               kind: WorkerKind::Local,
               registered_at: cp.clock().now(),
               node_id: None,
           })
           .await
           .unwrap();
       cp.record_local_worker_registry(kind, worker.id, "s3cret", endpoint, None)
           .await
           .unwrap();
       worker
   }

   async fn worker_status(cp: &ControlPlane, id: WorkerId) -> WorkerStatus {
       cp.get_worker_inspection(id).await.unwrap().unwrap().worker.status
   }

   /// An address nothing listens on: bind an ephemeral port, then release it.
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
       let worker = register_local_row(&cp, kind, stalled.local_addr().unwrap()).await;

       cp.self_heal_stale_workers(kind).await.unwrap();

       assert_ne!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }

   #[tokio::test]
   async fn self_heal_retires_a_row_whose_endpoint_refuses() {
       // A hard-killed supervisor's worker leaves a closed port: that row is stale
       // and must not accumulate.
       let (cp, _tmp) = crate::cases::cp().await;
       let kind = LocalWorkerKind::Mkvtoolnix;
       let worker = register_local_row(&cp, kind, closed_endpoint()).await;

       cp.self_heal_stale_workers(kind).await.unwrap();

       assert_eq!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }

   #[tokio::test]
   async fn retire_stale_worker_accepts_a_row_a_peer_already_retired() {
       // Two starters can both judge one stale row dead; the slower one must not
       // fail its own startup because the faster one retired the row first.
       let (cp, _tmp) = crate::cases::cp().await;
       let worker = register_local_row(&cp, LocalWorkerKind::Mkvtoolnix, closed_endpoint()).await;
       let now = cp.clock().now();
       cp.retire_worker(worker.id, worker.epoch, now).await.unwrap();

       cp.retire_stale_worker(worker.id, worker.epoch, now)
           .await
           .unwrap();

       assert_eq!(worker_status(&cp, worker.id).await, WorkerStatus::Retired);
   }
   ```

2. Add a first-cut `retire_stale_worker` that only calls `retire_worker` (so the file
   compiles) and run `cargo test -p voom-control-plane --lib self_heal_ retire_stale_worker_`.
   Expect: `self_heal_keeps_…` fails (status `Retired`),
   `retire_stale_worker_accepts_…` fails (`already retired`),
   `self_heal_retires_…` passes.

3. In `compliance.rs` change `fn runtime_metadata(` to `pub(crate) fn runtime_metadata(`.

4. In `local_worker.rs`: add `use std::collections::HashMap;`,
   `use tokio::net::TcpStream;`, `use crate::cases::policy::compliance::runtime_metadata;`,
   and beside `SELF_HEAL_SCAN_LIMIT`:

   ```rust
   /// Bound on the refused-connect check. Only an inconclusive endpoint waits this long;
   /// a closed loopback port refuses at once (ADR 0102).
   const SELF_HEAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
   ```

   Replace `self_heal_stale_workers` and add the helpers:

   ```rust
   /// Retire same-kind rows a hard-killed supervisor left behind. A row with a recorded
   /// endpoint is retired only when that endpoint refuses connections: a busy or stopped
   /// live worker still accepts them, and the dispatch handshake probe cannot tell the two
   /// apart (ADR 0102).
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
           if let Some(endpoint) = endpoints.get(&worker.id)
               && !endpoint_refuses_connections(*endpoint).await
           {
               continue;
           }
           self.retire_stale_worker(worker.id, worker.epoch, now).await?;
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

5. Run `cargo test -p voom-control-plane --lib self_heal_ retire_stale_worker_` — expect 3
   passed. Run the dispatch-unchanged command above — expect 2 passed. Run
   `cargo test -p voom-control-plane --test local_worker_lifecycle` — expect 4 passed.
6. `just fmt-check && just lint`; commit `fix(control-plane): retire self-healed local
   workers only on a refused endpoint (#667)`.

Acceptance: the four verification entries are green; `git diff` of `compliance.rs` is the
single visibility change.

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

## Task 3 — ADR index row and full gate

Files: `docs/adr/README.md` (one row after 0101).

1. Append `| [0102](0102-self-heal-retires-only-on-a-refused-endpoint.md) | Local-worker
   self-heal retires only on a refused endpoint |` after the 0101 row; touch no other row.
2. `just check-adr-index` — expect `check-adr-index: OK`. Commit with the design set.
3. `just ci` — expect exit 0.

## Deferrals

None.
