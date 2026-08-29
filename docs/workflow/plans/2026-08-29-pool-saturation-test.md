# Pool-saturation test implementation plan

## Goal

Add a deterministic lease-repository regression test proving that more callers than the on-disk
SQLite pool can serve queue safely behind a held writer and converge after release.

ADR 0093 selects twelve concurrent heartbeats against one held lease. The test observes all eight
connections checked out, proves the remaining callers cannot have acquired connections, releases
the writer before asserting any captured failure, and verifies durable lease state afterward.

Tech stack: Rust, Tokio multi-thread tests, sqlx SQLite, existing `voom-test-support` fixtures.

## Global constraints

- Branch: `feat/pool-saturation-test-580`; base: `main`; scope token: `q580-fc6e0258`.
- Host architecture: x86_64; target architectures: none declared; relationship:
  no-target-declared.
- Modify only `crates/voom-store/src/repo/execution/leases_test.rs`, plus the committed design
  records. Production edits are transient only for the bite check and must be restored.
- Use the existing real on-disk `setup()` fixture and real Tokio time. Never pause or advance
  Tokio time around the SQLite pool.
- Use twelve heartbeat callers and one held `BEGIN IMMEDIATE` writer. Do not shorten production
  lock-wait or pool-acquire budgets and do not add a pool constructor.
- Capture the saturation observation and writer commit result, consume the writer, and join all
  tasks before asserting any captured failure.
- Guardrails: `cargo test -p voom-store pool_saturation_queues_heartbeats_until_writer_releases`;
  `just test-repeat voom-store pool_saturation_queues_heartbeats_until_writer_releases 25`;
  `just fmt-check`; `just lint`; `just check-test-layout`; `just check-paused-time-db`;
  `just check-transaction-openers`; `just test`; and finally `just ci`.

## Task 1 — Prove saturation, liveness, and recovery

Files:

- Modify and test: `crates/voom-store/src/repo/execution/leases_test.rs`.

Interfaces:

- Consume the existing `setup()`, `SqliteLeaseRepo::acquire`, `SqliteLeaseRepo::heartbeat`,
  `SqliteLeaseRepo::get`, `NewLease`, `LeaseState`, `T0`, and on-disk `SqlitePool` returned by the
  fixture.
- Consume sqlx 0.8.6 `Pool::size() -> u32` and `Pool::num_idle() -> usize`, verified in the pinned
  dependency source.
- Produce one test named `pool_saturation_queues_heartbeats_until_writer_releases`.
- No production or later-task interface is added.

Steps:

1. Add the following test beside the existing concurrent-writer lease tests. It acquires a held
   lease, retains a writer, barrier-releases twelve heartbeat tasks, and captures the saturation
   observation without asserting:

   ```rust
   #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
   async fn pool_saturation_queues_heartbeats_until_writer_releases() {
       const CALLERS: usize = 12;
       let (pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
       let lease = lrepo
           .acquire(NewLease {
               ticket_id: tid,
               worker_id: wid,
               ttl: Duration::seconds(60),
               now: T0,
           })
           .await
           .unwrap();
       let writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
       let lease_id = lease.id;
       let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CALLERS + 1));
       let heartbeat_at = T0 + Duration::seconds(10);
       let mut handles = Vec::with_capacity(CALLERS);
       for _ in 0..CALLERS {
           let lrepo = lrepo.clone();
           let barrier = std::sync::Arc::clone(&barrier);
           handles.push(tokio::spawn(async move {
               barrier.wait().await;
               lrepo
                   .heartbeat(lease_id, Duration::seconds(60), heartbeat_at)
                   .await
           }));
       }
       barrier.wait().await;

       let saturated = tokio::time::timeout(std::time::Duration::from_secs(5), async {
           loop {
               if pool.size() == 8 && pool.num_idle() == 0 {
                   return;
               }
               tokio::task::yield_now().await;
           }
       })
       .await;
       let finished_while_locked = handles.iter().filter(|handle| handle.is_finished()).count();

       let writer_result = writer.commit().await;
       let mut heartbeat_results = Vec::with_capacity(CALLERS);
       for handle in handles {
           heartbeat_results.push(handle.await);
       }

       writer_result.expect("held writer must commit before heartbeat assertions");
       saturated.unwrap_or_else(|error| {
           panic!(
               "pool did not saturate while the writer was held: size={}, idle={}, \
                finished={finished_while_locked}, error={error}",
               pool.size(),
               pool.num_idle()
           )
       });
       let available_beside_writer = usize::try_from(pool.size()).unwrap() - 1;
       assert!(
           CALLERS > available_beside_writer,
           "{CALLERS} callers must exceed {available_beside_writer} non-writer connections"
       );
       assert_eq!(finished_while_locked, 0);
       for result in heartbeat_results {
           let heartbeat = result
               .expect("heartbeat task panicked")
               .expect("heartbeat must wait for the writer and succeed");
           assert_eq!(heartbeat.id, lease_id);
           assert_eq!(heartbeat.state, LeaseState::Held);
       }

       let stored = lrepo.get(lease_id).await.unwrap().unwrap();
       assert_eq!(stored.state, LeaseState::Held);
       assert!(stored.expires_at >= lease.expires_at);
       assert_eq!(stored.last_heartbeat_at, heartbeat_at);
       assert_eq!(stored.epoch, lease.epoch + u64::try_from(CALLERS).unwrap());

       let converged = lrepo
           .heartbeat(
               lease_id,
               Duration::seconds(60),
               T0 + Duration::seconds(20),
           )
           .await
           .unwrap();
       assert_eq!(converged.epoch, stored.epoch + 1);
   }
   ```

2. Run
   `cargo test -p voom-store pool_saturation_queues_heartbeats_until_writer_releases`; expect one
   passed test and elapsed test time below ten seconds. If the compiler identifies an existing
   interface mismatch, correct only the borrowed signature or type named by that diagnostic and
   keep the behavior above unchanged.
3. Verify the test bites: temporarily set `CALLERS` to `7`, rerun the same focused command, and
   require failure at the `callers must exceed ... non-writer connections` assertion after every
   spawned task has joined. Restore `CALLERS` to `12` with `apply_patch`, rerun the focused command,
   and require it green. Do not commit the controlled fault.
4. Run
   `just test-repeat voom-store pool_saturation_queues_heartbeats_until_writer_releases 25`; expect
   all 25 repetitions to pass and each iteration to stay below the issue's ten-second default-suite
   threshold.
5. Run `just fmt-check`, `just lint`, `just check-test-layout`, `just check-paused-time-db`,
   `just check-transaction-openers`, and `just test`; expect every command to exit zero with no
   skipped test reported by the focused or repeated runs.
6. Commit the focused test as `test: cover SQLite pool saturation recovery` after the guardrails
   are green.

Acceptance:

- Twelve tasks are released together while one writer holds the SQLite write lock.
- The test observes eight checked-out connections, zero idle connections, zero completed
  heartbeats, and more callers than the seven non-writer slots before releasing the writer.
- Writer commit is captured, all task handles are joined, and only then can assertions panic.
- Every heartbeat succeeds; the lease stays held, does not shorten its deadline, records the fixed
  heartbeat time, and increments its epoch once per caller.
- A later uncontended heartbeat succeeds and advances the epoch once more.
- The controlled seven-caller fault fails, the focused test stays below ten seconds, and 25
  repeated runs pass.

## Durable workflow checkpoint

- Current phase: design complete; next phase: scope audit, then TDD build.
- Branch: `feat/pool-saturation-test-580`; base branch: `main`.
- Scope token: `q580-fc6e0258`.
- Open findings and deferrals: none. Spec-review suppression: ADR 0093 settles the arithmetic
  proof that twelve unfinished callers beside one held writer exceed the seven non-writer slots.
