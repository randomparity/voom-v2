# Implementation plan — race N claimers at one ticket (#578)

**Goal.** Add two control-plane tests that prove at most one worker can hold a
lease on a single contended ticket: one racing six workers on one node, one
racing six workers across six registered nodes.

**Architecture.** Both tests are sibling unit tests inside `voom-control-plane`,
driving real use-case entry points (`ControlPlane::try_acquire_lease` and
`ControlPlane::remote_acquire`) against a real on-disk WAL SQLite database
supplied by `voom_test_support::TempDatabase`. Claimers are `tokio::spawn`ed
tasks released together by a `tokio::sync::Barrier` on a multi-threaded runtime.
No production code changes are planned; if an assertion reddens, see *If a test
fails* below.

**Tech stack.** Rust (workspace toolchain in `rust-toolchain.toml`), tokio,
sqlx + SQLite, `just` for all guardrails.

**Design sources.** Spec:
`docs/workflow/specs/2026-08-26-race-one-ticket-exclusivity-design.md`. ADR:
`docs/adr/0085-contention-tests-at-the-use-case-level.md`. Read both before
starting; this plan does not restate their reasoning, only their consequences.

## Global constraints

Transcribed from the spec. Every task's requirements implicitly include these.

- **Claimer count is exactly 6.** The pool is `max_connections = 8`
  (`crates/voom-store/src/pool.rs:62`). Do not raise the count; a count above the
  pool makes claimers queue for a connection instead of contending, and this is
  not #580's pool-saturation test.
- **The raced ticket must have `max_attempts >= 2`, and both tests assert it.**
  With `max_attempts = 1`, a loser's snapshot is emptied by
  `attempt < max_attempts` rather than by `state = 'ready'`, which silently
  disarms the tripwire in Test B. The assertion is not optional and its comment
  is not decoration.
- **Never call `tokio::time::pause()` or `advance()` in either file.** ADR 0012,
  enforced by `just check-paused-time-db`, which fails when a file references
  `SqlitePool`/`ControlPlane` and also pauses tokio's clock. Domain time is the
  caller-supplied `now` (`T0`) and the fixture clock.
- **Sibling test layout.** ADR 0004, enforced by `just check-test-layout`. Both
  target files already exist and are already linked by `#[path]` from their
  sibling sources. Do not add a new `#[cfg(test)] mod tests { ... }` block in any
  `src/` file.
- **Temp databases live on the pinned `.test-tmp/` root.** ADR 0079.
  `TempDatabase` handles this. Never call `mktemp`/`tempdir` directly.
- **Clippy runs `cargo clippy --workspace --all-targets --all-features -- -D
  warnings`.** Test code in both target files already uses `.unwrap()` freely;
  match that. Do not add `#[allow]`/`#[expect]` without a `reason`.
- **Guardrail suite is `just ci`.** It runs `fmt-check`, `lint`,
  `check-test-layout`, `check-paused-time-db`, `check-control-plane-sql-boundary`,
  `check-check-constraint-bypass`, `check-payload-deny-unknown`,
  `check-adr-index`, their selftests, then `test`, `doc`, `deny`, `audit`.
- **Commits** follow Conventional Commits 1.0.0, imperative mood, subject
  <= 72 chars, and end with the trailer
  `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.

### Observed costs on the development host

- `cargo build -p voom-control-plane --all-features --tests`, warm: ~35s.
- `cargo test -p voom-control-plane --all-features concurrent_`, prebuilt: ~1.7s.
- `just ci`: not yet measured. Budget generously on its first run.

## File map

| File | Change | Answerable for |
|---|---|---|
| `crates/voom-control-plane/src/cases/execution/leases_test.rs` | modify — append one test | Test A: node-local claimers at one ticket |
| `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs` | modify — append one fixture + one test | Test B: the multi-node fixture and the remote race |

No other file is created or modified. In particular: no new crate, no change to
`voom-test-support`, no change to any `src/` production file, and no change to
either file's existing helpers.

## Task 1 — Test A: node-local claimers at one ticket

**Where this fits.** The first of the two tests. It is the only one of the pair
that exercises the ticket CAS as the deciding gate, because the local path has
no readiness pre-filter ahead of it.

**Files.** Modifies and tests
`crates/voom-control-plane/src/cases/execution/leases_test.rs`.

### Interfaces

Consumed from the existing codebase (all confirmed present at these signatures):

```rust
// crates/voom-control-plane/src/cases/mod_test.rs, re-exported via cases::mod
pub(crate) async fn cp() -> (crate::ControlPlane, voom_test_support::TempDatabase);
pub(crate) async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize;

// crates/voom-control-plane/src/cases/execution/leases_test.rs (same file)
fn ticket(kind: &str, max_attempts: u32) -> NewTicket;
async fn eligible_worker(cp: &crate::ControlPlane, name: &str, operation: &TicketOperation) -> Worker;
async fn grant_capacity(cp: &crate::ControlPlane, worker: &Worker, operation: &TicketOperation, limit: u32);
const T0: OffsetDateTime; // = OffsetDateTime::UNIX_EPOCH

// crates/voom-control-plane/src/lib.rs
impl ControlPlane {                       // #[derive(Clone)]
    pub fn tickets(&self) -> &SqliteTicketRepo;
    // `pool` is a private field on a struct defined at the crate root,
    // so `&cp.pool` is visible crate-wide; this file already uses that form.
}
// crates/voom-control-plane/src/cases/execution/leases.rs
impl ControlPlane {
    pub(crate) async fn try_acquire_lease(&self, input: NewLease)
        -> Result<LeaseAcquireOutcome, VoomError>;
}

// crates/voom-store/src/repo/execution/leases.rs
pub enum LeaseAcquireOutcome {
    Acquired(Lease),
    CapacityFull(WorkerCapacitySaturation),
    TicketNotReady { ticket_id: TicketId },
    WorkerIneligible { worker_id: WorkerId, operation: TicketOperation, reason: LeaseIneligibilityReason },
}
pub struct Lease { pub id: LeaseId, pub ticket_id: TicketId, pub worker_id: WorkerId, /* ... */ }

// crates/voom-store/src/repo/execution/tickets.rs
pub struct Ticket { pub id: TicketId, pub state: TicketState, pub attempt: u32,
                    pub max_attempts: u32, pub epoch: u64, /* ... */ }
```

Provided to later tasks: nothing. Task 2 is independent of Task 1.

Already imported at the top of this file — do **not** re-import: `std::sync::Arc`,
`time::Duration as TDuration`, `LeaseAcquireOutcome`, `TicketState`,
`crate::cases::{count, cp}`, `voom_core::TicketOperation`, `voom_events::EventKind`.

### Steps

**1.1 — Write the failing test.** Append to the end of
`crates/voom-control-plane/src/cases/execution/leases_test.rs`:

```rust
/// Six workers race one ready ticket on the local claim path.
///
/// Unlike `concurrent_local_acquire_never_exceeds_worker_operation_capacity`,
/// which races N claimers over N *distinct* tickets under a capacity limit,
/// every claimer here targets the same ticket and every claimer has spare
/// capacity — so the ticket CAS is the only thing that can reject them, and a
/// weakened CAS predicate produces a second lease instead of a clean loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_local_acquire_at_one_ticket_leases_exactly_once() {
    const CLAIMERS: usize = 6;

    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::new("noop").unwrap();

    let created = cp.create_ticket(ticket("noop", 2)).await.unwrap();
    cp.mark_ready_if_unblocked(created.id, T0).await.unwrap();
    let before = cp.tickets().get(created.id).await.unwrap().unwrap();

    // Load-bearing precondition, not a sanity check. The ready-ticket snapshot
    // and the CAS share `attempt < max_attempts`; with max_attempts = 1 a loser
    // would be rejected by the attempt budget rather than by `state = 'ready'`,
    // which is the predicate this test exists to pin. See ADR 0085.
    assert!(
        before.max_attempts >= 2,
        "raced ticket needs max_attempts >= 2, got {}",
        before.max_attempts
    );

    let mut workers = Vec::with_capacity(CLAIMERS);
    for index in 0..CLAIMERS {
        let worker = eligible_worker(&cp, &format!("one-ticket-{index}"), &operation).await;
        // Every claimer has room for this single ticket, so no claimer can be
        // turned away for capacity. That is what forces each one to the CAS,
        // and what keeps a rolled-back savepoint from hiding a second
        // transition behind an unchanged `attempt`.
        grant_capacity(&cp, &worker, &operation, 1).await;
        workers.push(worker);
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(CLAIMERS));
    let mut handles = Vec::with_capacity(CLAIMERS);
    for worker in &workers {
        let cp = cp.clone();
        let barrier = barrier.clone();
        let input = NewLease {
            ticket_id: created.id,
            worker_id: worker.id,
            ttl: TDuration::seconds(60),
            now: T0,
        };
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            cp.try_acquire_lease(input).await
        }));
    }

    let mut acquired = Vec::new();
    for handle in handles {
        match handle.await.unwrap() {
            Ok(LeaseAcquireOutcome::Acquired(lease)) => acquired.push(lease),
            Ok(LeaseAcquireOutcome::TicketNotReady { ticket_id }) => {
                assert_eq!(ticket_id, created.id, "loser named the wrong ticket");
            }
            Ok(other) => panic!(
                "every loser must lose at the ticket CAS; a capacity or \
                 eligibility rejection means the claimer never reached it: {other:?}"
            ),
            Err(error) => panic!("no claimer may error under contention: {error:?}"),
        }
    }

    assert_eq!(acquired.len(), 1, "exactly one claimer may acquire");

    let leases: Vec<(i64, i64)> =
        sqlx::query_as("SELECT id, worker_id FROM leases WHERE state = 'held'")
            .fetch_all(&cp.pool)
            .await
            .unwrap();
    assert_eq!(leases.len(), 1, "exactly one held lease expected");
    assert_eq!(
        u64::try_from(leases[0].1).unwrap(),
        acquired[0].worker_id.0,
        "the held lease belongs to a different worker than the winner"
    );
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&cp.pool)
        .await
        .unwrap();
    assert_eq!(total, 1, "a rejected claimer must leave no lease row behind");

    let after = cp.tickets().get(created.id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Leased);
    assert_eq!(
        after.attempt,
        before.attempt + 1,
        "the ticket transition must have happened exactly once"
    );
    assert_eq!(after.epoch, before.epoch + 1);

    assert_eq!(count(&cp, EventKind::LeaseAcquired).await, 1);
}
```

**1.2 — Confirm it compiles and passes.** Run:

```sh
cargo test -p voom-control-plane --all-features \
  concurrent_local_acquire_at_one_ticket_leases_exactly_once
```

Expect `test result: ok. 1 passed`. If it fails, **do not weaken an assertion** —
go to *If a test fails* below.

**1.3 — Confirm it bites.** Edit
`crates/voom-store/src/repo/execution/leases.rs`, in the `UPDATE tickets`
statement inside `acquire_guarded` (around line 419), deleting only
`AND state = 'ready'`. Re-run the command from 1.2.

Expect a failure on `exactly one claimer may acquire` or `exactly one held lease
expected`, reporting **2**, not 6 — the CAS still carries
`attempt < max_attempts` and the ticket's `max_attempts` is 2, so the third
claimer still matches zero rows. Record the exact assertion message.

**1.4 — Restore and re-confirm.** `git checkout -- crates/voom-store/src/repo/execution/leases.rs`,
then re-run 1.2 and expect `1 passed` again. Verify with
`git status --short` that the only modified file is `leases_test.rs`.

**1.5 — Check stability.** Run:

```sh
just test-repeat voom-control-plane concurrent_local_acquire_at_one_ticket 25
```

Expect `no failure in 25 runs`. Then:

```sh
cargo test -p voom-control-plane --all-features concurrent_local_acquire_at_one_ticket -- --test-threads=1
```

Expect `1 passed`.

**1.6 — Run the local guardrails and commit.**

```sh
just fmt-check && just check-test-layout && just check-paused-time-db && just lint
```

All four must exit 0. Then commit `leases_test.rs` alone with subject
`test: race six local claimers at one ticket`.

### Acceptance criteria

- The test exists in `leases_test.rs`, carries no `#[ignore]`, and passes.
- Deleting `AND state = 'ready'` from the CAS makes it fail; restoring makes it
  pass. The observed failure count is recorded.
- 25 repeat runs pass, and it passes under `--test-threads=1`.
- `just fmt-check`, `just check-test-layout`, `just check-paused-time-db`, and
  `just lint` are green.

## Task 2 — Test B: remote claimers across six nodes

**Where this fits.** The second test. It proves multi-node exclusivity as a
composite of the ready-ticket snapshot and the CAS; per ADR 0085 it does **not**
test the CAS in isolation, and a CAS-only weakening will not redden it.

**Files.** Modifies and tests
`crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`.

### Interfaces

Consumed from the existing codebase (all confirmed present at these signatures):

```rust
// crates/voom-control-plane/src/cases/workers/nodes.rs
pub struct RegisterNodeInput { pub name: String, pub kind: NodeKind,
                               pub heartbeat_ttl_seconds: u32, pub metadata: JsonValue }
pub struct RegisteredNode { pub node: Node, pub token: SecretString }
impl ControlPlane { pub async fn register_node(&self, input: RegisterNodeInput)
                        -> Result<RegisteredNode, VoomError>; }

// crates/voom-control-plane/src/cases/workers/registry.rs
pub struct RegisterWorkerForNodeInput { pub node_id: NodeId, pub token: SecretString,
    pub name: String, pub kind: WorkerKind,
    pub capabilities: Vec<NewWorkerCapabilityDraft>, pub grants: Vec<NewWorkerGrantDraft> }
pub struct NewWorkerCapabilityDraft { pub operation: TicketOperation, pub codecs: Vec<String>,
    pub hardware: Vec<String>, pub artifact_access: Vec<String>, pub extra: JsonValue }
pub struct NewWorkerGrantDraft { pub can_execute: Vec<TicketOperation>,
    pub can_access_read: Vec<TicketOperation>, pub can_access_write: Vec<TicketOperation>,
    pub denies: Vec<TicketOperation>, pub max_parallel: JsonValue }

// crates/voom-control-plane/src/cases/execution/remote_execution/mod.rs
pub struct RemoteWorkerReadinessInput { pub node_id: NodeId, pub token: SecretString,
    pub incarnation_id: NodeIncarnationId, pub worker_id: WorkerId, pub readiness: WorkerReadiness }
pub struct RemoteAcquireInput { pub node_id: NodeId, pub token: SecretString,
    pub incarnation_id: NodeIncarnationId, pub worker_id: WorkerId,
    pub idempotency_key: String, pub request_hash: String, pub lease_ttl_seconds: i64 }
pub enum RemoteAcquireOutcome {
    Idle { worker_id: WorkerId, scheduler_decision_id: u64 },
    NoCandidate { worker_id: WorkerId, scheduler_decision_id: u64 },
    Leased(RemoteLeaseDispatch),
}

// crates/voom-store/src/repo/execution/scheduler_decisions.rs
pub enum SchedulerDecisionKind { LeaseAcquire, Idle, NoCandidate }
impl SqliteSchedulerDecisionRepo {
    pub async fn get(&self, id: u64) -> Result<Option<SchedulerDecision>, VoomError>;
}
// `cp.scheduler_decisions` is a pub(crate) field on ControlPlane (lib.rs:174).

// crates/voom-core/src/taxonomy/ids.rs
// NodeIncarnationId parses from exactly 32 LOWERCASE hex characters.

// existing helpers in this same file
async fn cp_at(now: OffsetDateTime) -> (crate::ControlPlane, voom_test_support::TempDatabase);
fn node_input(name: &str, kind: NodeKind) -> RegisterNodeInput;
fn ticket_op(kind: &str) -> TicketOperation;
const T0: OffsetDateTime;
const OP: &str; // = "test.remote"
```

**One import must be added** to the existing `use` block for
`voom_store::repo::execution::scheduler_decisions`: `SchedulerDecisionKind`. The
file currently imports `SchedulerDecisionFilter, SchedulerDecisionOutcome,
SchedulerReasonCode` from that module.

### Steps

**2.1 — Add the multi-node fixture.** Append near the other fixture helpers at
the end of `mod_test.rs` (after `fixture_with_options`):

```rust
/// One registered node with its own token, incarnation, and ready remote worker.
struct RacingNode {
    node_id: NodeId,
    token: secrecy::SecretString,
    incarnation_id: NodeIncarnationId,
    worker_id: voom_core::WorkerId,
}

/// Several independent nodes against one control plane.
///
/// `fixture_with_options` registers exactly one node and hardcodes a single
/// incarnation id, so it cannot express a race between nodes.
struct MultiNodeFixture {
    cp: crate::ControlPlane,
    _tmp: voom_test_support::TempDatabase,
    nodes: Vec<RacingNode>,
}

impl MultiNodeFixture {
    /// A ready ticket every node's worker is eligible for.
    ///
    /// This repeats `RemoteFixture::ready_ticket_with_priority` rather than
    /// sharing it: two copies is not yet the third repetition that earns a
    /// helper, and extracting one would edit a method twenty-odd existing
    /// tests depend on.
    async fn ready_ticket(&self, kind: &str, max_attempts: u32) -> TicketId {
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: ticket_op(kind),
                priority: 0,
                payload: json!({
                    "dispatch": {"kind": kind},
                    "artifact_access": {
                        "inputs": ["handle:input:test"],
                        "outputs": ["handle:output:test"]
                    }
                }),
                max_attempts,
                created_at: T0,
            })
            .await
            .unwrap();
        self.cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
        ticket.id
    }

    fn acquire_input(&self, index: usize, idempotency_key: &str, request_hash: &str)
        -> RemoteAcquireInput
    {
        let node = &self.nodes[index];
        RemoteAcquireInput {
            node_id: node.node_id,
            token: node.token.clone(),
            incarnation_id: node.incarnation_id,
            worker_id: node.worker_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            lease_ttl_seconds: 60,
        }
    }
}

async fn multi_node_remote_fixture(node_count: usize, operation: &str) -> MultiNodeFixture {
    let (cp, tmp) = cp_at(T0).await;
    let mut nodes = Vec::with_capacity(node_count);

    for index in 0..node_count {
        let registered = cp
            .register_node(node_input(&format!("racing-node-{index}"), NodeKind::Remote))
            .await
            .unwrap();
        let worker = cp
            .register_worker_for_node(RegisterWorkerForNodeInput {
                node_id: registered.node.id,
                token: registered.token.clone(),
                name: format!("racing-worker-{index}"),
                kind: WorkerKind::Remote,
                capabilities: vec![NewWorkerCapabilityDraft {
                    operation: ticket_op(operation),
                    codecs: vec!["json".to_owned()],
                    hardware: Vec::new(),
                    artifact_access: vec!["shared_mount".to_owned()],
                    extra: json!({}),
                }],
                grants: vec![NewWorkerGrantDraft {
                    can_execute: vec![ticket_op(operation)],
                    can_access_read: Vec::new(),
                    can_access_write: Vec::new(),
                    denies: Vec::new(),
                    max_parallel: json!({"*": 1}),
                }],
            })
            .await
            .unwrap();

        // Exactly 32 lowercase hex characters, distinct per node and
        // deterministic across runs.
        let incarnation_id: NodeIncarnationId =
            format!("{:032x}", 0x0123_4567_89ab_cdef_u128 + index as u128)
                .parse()
                .unwrap();

        let mut tx = cp.pool_for_test().begin().await.unwrap();
        cp.node_incarnations
            .insert_in_tx(
                &mut tx,
                NewNodeIncarnation {
                    id: incarnation_id,
                    node_id: registered.node.id,
                    started_at: T0,
                },
            )
            .await
            .unwrap();
        cp.nodes
            .activate_incarnation_in_tx(&mut tx, registered.node.id, None, incarnation_id, T0)
            .await
            .unwrap();
        cp.workers
            .bind_incarnation_in_tx(&mut tx, worker.id, registered.node.id, incarnation_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        cp.remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: registered.node.id,
            token: registered.token.clone(),
            incarnation_id,
            worker_id: worker.id,
            readiness: WorkerReadiness::Ready,
        })
        .await
        .unwrap();

        nodes.push(RacingNode {
            node_id: registered.node.id,
            token: registered.token,
            incarnation_id,
            worker_id: worker.id,
        });
    }

    MultiNodeFixture { cp, _tmp: tmp, nodes }
}
```

**2.2 — Confirm the fixture builds.** Run
`cargo build -p voom-control-plane --all-features --tests`. Expect it to
finish with no errors. Dead-code warnings for the not-yet-used fixture are
expected at this step and are removed by 2.3; do not silence them with
`#[allow]`.

**2.3 — Write the test.** Append after the fixture:

```rust
/// Six workers on six different nodes race one ready ticket.
///
/// Per ADR 0085 this proves multi-node exclusivity, not the CAS: the
/// ready-ticket snapshot and the CAS carry the same predicates inside one
/// `BEGIN IMMEDIATE` transaction, so a loser is eliminated at the snapshot and
/// returns `Idle` without reaching the CAS. Weakening the CAS alone will not
/// redden this test; weakening the snapshot's `state = 'ready'` will, on the
/// loser assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_remote_acquire_across_nodes_at_one_ticket_leases_exactly_once() {
    const CLAIMERS: usize = 6;

    let fixture = multi_node_remote_fixture(CLAIMERS, OP).await;
    let ticket_id = fixture.ready_ticket(OP, 2).await;
    let before = fixture.cp.tickets().get(ticket_id).await.unwrap().unwrap();

    // See the identical assertion in Test A and ADR 0085: with max_attempts = 1
    // every loser's snapshot is emptied by the attempt budget instead of by
    // `state = 'ready'`, and the loser assertion below stops detecting a
    // snapshot regression.
    assert!(
        before.max_attempts >= 2,
        "raced ticket needs max_attempts >= 2, got {}",
        before.max_attempts
    );

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CLAIMERS));
    let mut handles = Vec::with_capacity(CLAIMERS);
    for index in 0..CLAIMERS {
        let cp = fixture.cp.clone();
        let barrier = barrier.clone();
        let input = fixture.acquire_input(
            index,
            &format!("one-ticket-{index}"),
            &format!("hash-one-ticket-{index}"),
        );
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            cp.remote_acquire(input).await
        }));
    }

    let mut leased = Vec::new();
    let mut idle_decisions = Vec::new();
    for handle in handles {
        match handle.await.unwrap() {
            Ok(RemoteAcquireOutcome::Leased(dispatch)) => leased.push(dispatch),
            Ok(RemoteAcquireOutcome::Idle {
                scheduler_decision_id,
                ..
            }) => idle_decisions.push(scheduler_decision_id),
            Ok(RemoteAcquireOutcome::NoCandidate {
                scheduler_decision_id,
                ..
            }) => panic!(
                "a loser reached the post-selection gate, which means the \
                 ready-ticket snapshot let a leased ticket through; decision {scheduler_decision_id}"
            ),
            Err(error) => panic!("no claimer may error under contention: {error:?}"),
        }
    }

    assert_eq!(leased.len(), 1, "exactly one claimer may acquire");
    assert_eq!(idle_decisions.len(), CLAIMERS - 1);
    assert_eq!(leased[0].ticket_id, ticket_id);

    // Every loser must have been eliminated on ticket readiness. `Idle` is
    // produced only by the empty-candidate branch, so a capacity or
    // eligibility rejection would arrive as `NoCandidate` and has already
    // panicked above; this pins the durable record as well as the outcome.
    for decision_id in idle_decisions {
        let decision = fixture
            .cp
            .scheduler_decisions
            .get(decision_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            decision.decision_kind,
            SchedulerDecisionKind::Idle,
            "loser decision {decision_id} is not an idle elimination"
        );
    }

    let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases WHERE state = 'held'")
        .fetch_one(fixture.cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(held, 1, "exactly one held lease expected, found {held}");
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(fixture.cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(total, 1, "a rejected claimer must leave no lease row behind");

    let after = fixture.cp.tickets().get(ticket_id).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Leased);
    assert_eq!(
        after.attempt,
        before.attempt + 1,
        "the ticket transition must have happened exactly once"
    );
}
```

**2.4 — Confirm it passes.** Run:

```sh
cargo test -p voom-control-plane --all-features \
  concurrent_remote_acquire_across_nodes_at_one_ticket_leases_exactly_once
```

Expect `test result: ok. 1 passed`. If it fails, see *If a test fails*. Note in
particular: a failure on the `NoCandidate` panic means the snapshot returned a
leased ticket, which contradicts the spec's Background — investigate before
changing anything.

**2.5 — Bite arm (i): CAS alone.** Delete `AND state = 'ready'` from the
`UPDATE tickets` statement in `acquire_guarded`
(`crates/voom-store/src/repo/execution/leases.rs`, around line 419). Re-run 2.4.
**Expect it to still pass** — this is the documented blind spot. Record the
result. Restore the file with `git checkout --`.

**2.6 — Bite arm (ii): snapshot alone.** Delete `state = 'ready' AND ` from the
`QueryBuilder` string in `ready_for_operations_in_tx`
(`crates/voom-store/src/repo/execution/tickets.rs`, around line 1119). Re-run 2.4.
**Expect a failure on the `NoCandidate` panic**, and confirm by a separate query
or by the surviving assertions that exactly one lease is still held. Record the
message. Restore with `git checkout --`.

**2.7 — Bite arm (iii): both.** Apply the edits from 2.5 and 2.6 together.
Re-run 2.4. **Expect a failure on `exactly one claimer may acquire`, reporting 2
leased.** Record the message. Restore both files with `git checkout --` and
re-run 2.4 expecting `1 passed`. Confirm with `git status --short` that
`mod_test.rs` is the only modified file.

If any arm's observed result differs from the expectation stated here, that is a
finding against the spec and ADR 0085 — record the observation and correct those
documents. Do not adjust the test to match.

**2.8 — Check stability.** Run:

```sh
just test-repeat voom-control-plane concurrent_remote_acquire_across_nodes_at_one_ticket 25
```

Expect `no failure in 25 runs`. Then:

```sh
cargo test -p voom-control-plane --all-features concurrent_remote_acquire_across_nodes_at_one_ticket -- --test-threads=1
```

Expect `1 passed`.

**2.9 — Run the local guardrails and commit.**

```sh
just fmt-check && just check-test-layout && just check-paused-time-db && just lint
```

All four must exit 0. Then commit `mod_test.rs` alone with subject
`test: race six remote claimers across nodes at one ticket`.

**2.10 — Full guardrail suite.** Run `just ci` bare — no pipe, no redirect — and
require exit 0.

### Acceptance criteria

- The fixture and test exist in `mod_test.rs`, the test carries no `#[ignore]`,
  and it passes.
- All three bite arms were run and their observed results recorded; any
  divergence from the predicted table was written back into the spec and ADR 0085.
- 25 repeat runs pass, and it passes under `--test-threads=1`.
- `just ci` is green.
- `git status --short` is clean apart from the intended files.

## If a test fails

These tests are written after the code they cover, so a red assertion is a
finding about the code until proven otherwise.

1. **Do not adjust the assertion.** Run `$detect-curse` and establish the cause
   before changing anything.
2. If the cause is a production defect, fix the production path. The permitted
   surface is `crates/voom-store/src/repo/execution/leases.rs` and the
   `crates/voom-control-plane/src/cases/execution/` paths the failing assertion
   identifies, plus a `migrations/*.sql` addition only if the proven defect is
   schema-level. Anything wider needs the operator.
3. If the cause is the test's own setup, fix the setup and record what the wrong
   setup made the test appear to prove.
4. Either way, report the failure and the diagnosis — a passing suite reached by
   softening an assertion is the failure mode this whole change exists to
   prevent.

## Rollback

Both tasks are additive to two test files and touch no production code. To
abandon: `git checkout -- <file>` before the commit, or revert the commit after.
The temporary production edits in steps 1.3, 2.5, 2.6, and 2.7 are always
restored with `git checkout --` in the same step that makes them; `git status
--short` at the end of each task is what confirms none leaked.
