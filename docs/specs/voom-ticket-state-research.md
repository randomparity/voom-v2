# Ticket State Management for Durable Job Queues: A Technical Reference for VOOM

## Executive Summary

VOOM's control plane routes all media operations through a pipeline of **durable tickets** whose lifecycle is `Policy → Plan DAG → Tickets → Scheduler Leases → Worker Results → Host Commit`. Every design decision about ticket state — how states are defined, how transitions are guarded, how concurrency is controlled, how failures are recovered — directly affects whether the system can guarantee that media operations are executed exactly once, without data loss, and without leaving the library in a partially mutated state. This document provides in-depth, actionable technical research on the foundational computer science concepts that govern correct ticket-queue management, mapped specifically to VOOM's architecture.

---

## 1. Finite-State Machines and the Ticket Lifecycle

### 1.1 FSM Fundamentals

A **finite-state machine (FSM)** is a mathematical model of computation defined by a finite set of states, an initial state, and a set of guarded transitions that move the machine from one state to another in response to inputs. In queue management, every job or ticket is itself an FSM: it starts in a well-known initial state, advances through exactly one valid sequence of states, and terminates in a terminal state. The key property is that **the machine can be in exactly one state at any given time** — a guarantee that is only meaningful when transitions are atomic.

For VOOM tickets, a canonical state model based on the spec is:

```
PENDING → READY → LEASED → SUCCEEDED
                         ↓
                       FAILED → RETRYING → READY   (bounded retry loop)
                         ↓
                       ABANDONED
```

Each arrow is a guarded transition. `PENDING` means dependencies are unsatisfied. `READY` means all upstream tickets have committed. `LEASED` means a specific worker holds the ticket under a time-bounded lease. `SUCCEEDED` and `FAILED` are terminal states that feed into the append-only event log. Adding a `RETRYING` sub-state or attempt counter keeps retry logic entirely within the FSM rather than in ad-hoc application code.

### 1.2 Why Terminal States Must Be Immutable

Once a ticket reaches `SUCCEEDED` or `ABANDONED`, the transition must be final and immutable. This preserves audit history and prevents the host commit from being re-attempted on a ticket that already applied a destructive filesystem mutation. VOOM's spec explicitly notes that "partially produced artifacts are either promoted only after verification or marked abandoned and eligible for cleanup," which is precisely the consequence of immutable terminal states.

### 1.3 Directed Acyclic Graph (DAG) Scheduling

VOOM's planner emits a full phase DAG upfront rather than scheduling one ticket at a time. The computer-science foundation for this is the **directed acyclic graph (DAG)**: a directed graph with no cycles, where each vertex is a ticket and each edge is a dependency constraint. DAGs are topologically sortable, meaning there always exists at least one valid linear ordering in which every ticket appears after all its dependencies.

**Topological sort** (Kahn's algorithm or DFS-based) is the canonical method for enumerating DAG tickets in dependency-respecting order. In VOOM, the scheduler does not need to commit to a fully linear order at plan creation time; it only needs to track which tickets have all predecessors in `SUCCEEDED` state. A ticket transitions from `PENDING` to `READY` when its **in-degree drops to zero** in the remaining dependency subgraph — which can be computed with a simple counter per ticket, decremented each time a predecessor commits.

The practical implementation in SQLite is:

```sql
-- Transition a single ticket to READY when all its dependencies are satisfied
UPDATE tickets
   SET status = 'ready'
 WHERE id = :ticket_id
   AND status = 'pending'
   AND NOT EXISTS (
       SELECT 1 FROM ticket_dependencies td
         JOIN tickets t ON t.id = td.depends_on_ticket_id
        WHERE td.ticket_id = :ticket_id
          AND t.status != 'succeeded'
   );
```

This runs inside the same transaction that marks a predecessor `SUCCEEDED`, making the unlock atomic.

---

## 2. Durability: Write-Ahead Logging and Transactional Safety

### 2.1 Write-Ahead Logging

**Write-ahead logging (WAL)** is the foundational durability mechanism in all serious RDBMS implementations, including SQLite. The core invariant is: *all modifications are written to an append-only log before they are applied to the main data file*. If a crash occurs, the log is replayed on restart to redo committed changes and undo uncommitted ones. SQLite in WAL mode provides this guarantee even for single-file embedded databases, making it suitable for VOOM's home-deployment profile.

For VOOM, the consequence is clear: **all ticket state transitions must be written inside a transaction**. A ticket moving from `READY` to `LEASED` (issuing a worker lease), from `LEASED` to `SUCCEEDED` (recording a commit), or from `LEASED` to `FAILED` (recording a worker crash) must each be atomic. The event log entry for the transition must be inserted **in the same transaction**, not afterwards — otherwise a crash between the state update and the event insert leaves an unauditable gap.

### 2.2 The Staged-Jobs Problem

A classic sharp edge in transactional job queues: if you insert a job into a queue *during* a database transaction, a fast worker may dequeue and execute the job before the transaction commits, finding none of the data the job expected. If the transaction rolls back, the job can never succeed regardless of retries.

VOOM avoids this by keeping tickets *inside* the SQLite database and never dispatching a worker until the `READY` transition is durably committed. The scheduler's inner loop reads committed `READY` tickets from the `tickets` table and issues leases — there is no external queue that can see an uncommitted ticket. This is the "transactionally staged job drain" pattern: jobs are staged in the relational database under ACID isolation, then picked up by the secondary process (the scheduler).

### 2.3 Atomic Phases and Recovery Points

For multi-step operations that include foreign state mutations (calling an external system, writing to object storage, committing a filesystem artifact), each logical step should be preceded by a committed **recovery point** stored in the database. This allows a crashed or restarted process to resume from the last durable checkpoint rather than restarting from the beginning.

In VOOM terms, a multi-phase plan like `containerize → transcode → verify → commit` maps naturally: each ticket is a recovery point. If the daemon crashes while a `transcode` ticket is LEASED, the stale lease expires via heartbeat timeout, the ticket returns to `READY`, and the scheduler re-leases it to a new worker. The previous worker's staged artifact is marked abandoned during cleanup.

---

## 3. Lease-Based Concurrency Control

### 3.1 Leases vs. Locks

A **lease** is a time-limited contract giving its holder specified rights to some resource. Unlike a traditional lock, which persists until explicitly released, a lease automatically expires at `expires_at` even if the holder crashes, disconnects, or deadlocks. This eliminates the primary failure mode of permanent lock starvation caused by a crashed client.

The foundational paper by Gray and Cheriton (1989) coined the term and identified the core trade-off: leases require the holder to heartbeat to extend the grant, but they guarantee that the system can make progress even when holders fail. VOOM's spec directly implements this: "automated, external, and worker-issued leases have a finite TTL. A lease without renewal is treated as expired at `expires_at` evaluated against the control-plane clock."

### 3.2 The Fencing Token Problem

A subtle danger with leases in distributed systems: a worker that believes it holds a valid lease may be operating in a garbage-collection pause or on a partitioned network while its lease has already expired and been re-granted to another worker. Kleppmann's analysis of distributed locking demonstrates that the only safe pattern is to attach a **monotonically increasing fencing token** to the lease, and have the resource (the database, the filesystem commit, the object store) reject writes with a stale (lower) token.

For VOOM, this means: when a worker holds lease token `N` and performs a staged artifact upload, the host commit transaction must validate that lease `N` is still valid *at commit time*, inside the same transaction. The spec's "Commit Safety Gate" section explicitly implements this: "delete, archive, replace, and move commits perform their lease check inside the host-side transaction that records the commit." This is the correct implementation of the fencing token pattern.

### 3.3 Clock Source Discipline

VOOM's `AssetUseLease` schema includes a `clock_source` field with explicit commentary: "the monotonic-plus-wall clock the control plane uses to evaluate freshness (named explicitly so external issuers cannot supply a drifting clock)." This is essential. Amazon's distributed systems guidance on leader election confirms that lease durations must depend only on **local elapsed time**, not a globally synchronized wall clock. A worker cannot be trusted to report its own elapsed time; the control plane evaluates all lease freshness using its own monotonic clock.

### 3.4 Advisory vs. Blocking Leases

VOOM distinguishes two lease modes:

| Mode | Effect |
|------|--------|
| **Blocking** | Prevents the commit safety gate from proceeding; destructive commits on the scoped resource are hard-rejected |
| **Advisory** | Reduces scheduler score for the affected resource; does not prevent work |

Playback leases are blocking by default; scheduler-scored metadata (e.g., "a large copy is active, prefer other storage") is advisory. This two-tier model lets the system remain operational during advisory holds while providing hard safety for user-visible playback sessions.

---

## 4. Idempotency and Exactly-Once Semantics

### 4.1 At-Least-Once vs. Exactly-Once

Message queues and job systems offer delivery guarantees on a spectrum:

- **At-most-once**: the message is delivered zero or one time; data loss on failure.
- **At-least-once**: the message is delivered one or more times; duplicates possible on retry.
- **Exactly-once**: the message is delivered and processed precisely once; requires coordination overhead.

In practice, durable queues provide **at-least-once** delivery semantics. Exactly-once processing is achieved not by the queue but by making the consumer idempotent — so that processing a duplicate message produces the same outcome as processing it once.

### 4.2 Idempotency Definition and Mechanism

**Idempotence** is the property of an operation that can be applied multiple times without changing the result beyond the initial application. In API and queue design, an idempotent consumer can safely receive and process a duplicated ticket lease without producing a duplicated side-effect (e.g., a second filesystem commit of the same transcode).

The canonical implementation: associate a **unique caller-provided request identifier** (idempotency key) with each operation, store the key atomically with the operation's result in the same ACID transaction, and on re-delivery, look up the key first and return the previously stored result rather than re-executing. The ACID atomicity requirement is critical: "the process that combines recording the idempotent token and all mutating operations related to servicing the request must meet the properties for an atomic, consistent, isolated, and durable (ACID) operation."

For VOOM, each ticket is already its own idempotency key: the `ticket_id` is the stable identifier that the worker includes in its result payload. The host commit transaction checks `WHERE ticket_id = :id AND status = 'leased' AND lease_id = :lease_id` before writing the committed artifact. If the ticket is already `succeeded`, the host returns the stored result. If the `lease_id` mismatches (because the lease expired and was re-granted), the commit is rejected as stale.

### 4.3 Idempotency Key Lifecycle

Keys should not be kept indefinitely. For VOOM tickets:

- **Active** tickets are live until terminal.
- **Succeeded** tickets are retained in the database for the audit trail and plan history (VOOM's spec mandates plan auditability).
- **Abandoned** tickets can be archived to a cold-storage table after a configurable retention window.

The spec already models the equivalent under idempotency keys: the `compliance_reports` and `execution_plans` tables contain the history needed to explain any past action.

---

## 5. Retry Strategies: Backoff, Jitter, and Dead-Letter Queues

### 5.1 Exponential Backoff with Jitter

Retrying failed tickets immediately and aggressively can amplify load on a partially failing downstream (e.g., a GPU node under memory pressure). The canonical pattern is **capped exponential backoff**: the wait between retry attempts grows exponentially up to a maximum cap.

```
wait(n) = min(cap, base * 2^n)
```

However, when all failed tickets share the same backoff schedule, they retry in lockstep and create a new load spike. **Jitter** adds a random component to spread retries:

```
wait(n) = random_between(0, min(cap, base * 2^n))
```

Amazon's systems guidance identifies jitter as essential for preventing thundering-herd retry storms in distributed systems.

For VOOM, the `attempt_count` field on a ticket drives the backoff formula. The scheduler should not re-queue a retrying ticket for immediate dispatch; instead it should compute `next_eligible_at = now + backoff(attempt_count)` and only surface the ticket to the scheduler after that time.

### 5.2 Classifying Failures: Retriable vs. Terminal

Not all failures should be retried. VOOM's error taxonomy maps naturally to retry policy:

| Error Class | Retriable? | Rationale |
|---|---|---|
| `worker_timeout` | Yes | Transient; lease expiry handles stale work |
| `worker_crash` | Yes | Lease expires; new worker can pick up |
| `artifact_checksum_mismatch` | Yes (limited) | May be transient corruption in transit |
| `no_eligible_worker` | Yes (with backoff) | Worker pool may recover |
| `malformed_worker_result` | No | Indicates a bug; retrying won't help |
| `approval_required` | No | Requires human action; retrying is noise |
| `policy_parse_error` | No | Structural error; policy must be fixed first |
| `stale_identity_evidence` | No | Evidence must be re-collected and re-accepted |
| `closure_resolution_incomplete` | No (without operator action) | Fail-closed is the safe default |

AWS guidance confirms this partition: "HTTP provides a clear distinction between client and server errors. Client errors should not be retried with the same request because they aren't going to succeed later, while server errors may succeed on subsequent tries."

### 5.3 Dead-Letter Handling

Tickets that exhaust their retry budget without succeeding must not silently disappear. Amazon SQS uses a **dead-letter queue (DLQ)** — a separate queue that receives messages that cannot be processed after a configured number of attempts. In VOOM's SQLite model, the equivalent is transitioning the ticket to `ABANDONED` and linking it to a durable `Issue` record with appropriate severity. The spec already describes this: "non-retriable failures stop the affected plan branch and surface actionable diagnostics." An `Issue` of kind `policy_noncompliant` or `health_failed` becomes the durable signal that a human or future policy run must resolve.

---

## 6. Priority Queues and Scheduling Fairness

### 6.1 Priority Queue Fundamentals

A **priority queue** is an abstract data type where each element has an associated priority and the queue always serves the highest-priority element first. For VOOM tickets, priority is composite: the spec lists ticket priority, issue severity and priority, dependency unlock order, artifact locality, resource cost, runtime use leases, and scheduling windows as inputs to the scheduler.

A heap-backed priority queue gives O(log n) insertion and O(log n) extraction. For a SQLite-backed scheduler, the "priority queue" is expressed as an indexed SQL query:

```sql
SELECT t.id
  FROM tickets t
  JOIN workers w ON w.id = :worker_id
 WHERE t.status = 'ready'
   AND t.required_capability = ANY(w.capabilities)
   AND t.next_eligible_at <= now()
 ORDER BY t.priority DESC, t.created_at ASC
 LIMIT 1
   FOR UPDATE SKIP LOCKED;
```

`SKIP LOCKED` is critical: it prevents multiple schedulers from contending on the same ticket row, allowing them to process different tickets in parallel without deadlock.

### 6.2 Bimodal Queue Behavior and Backlog Prevention

Amazon's queue engineering research identifies **bimodal behavior** as a fundamental property of queue-based systems: when queue depth is zero, the system is in "fast mode" with low latency; when arrivals exceed processing rate, the queue enters "slow mode" where end-to-end latency grows unboundedly and recovery requires greater-than-double-capacity processing to drain the backlog.

Practical mitigations for VOOM:
- **Separate queues by workload type**: transcode tickets, probe tickets, and commit tickets should not compete in a single FIFO queue.
- **LIFO preference for fresh data**: when a backlog builds, prefer newest tickets so live work is not blocked behind a historical backlog. The spec's `priority: newest_first` scheduling policy is the correct default.
- **Backpressure**: the daemon should apply rate limits to new ticket creation when the scheduler queue depth exceeds a threshold, preventing an unbounded backlog from forming.

### 6.3 Little's Law and Concurrency Limits

**Little's Law** states that the average number of items in a system (concurrency) equals the arrival rate multiplied by the average time each item spends in the system: `L = λW`. For VOOM's per-worker concurrency limits, this means: if a worker is processing 4 transcode tickets per hour and each takes 15 minutes on average, it will have `4 × 0.25 = 1` ticket in flight on average — well within a `max_parallel: 2` grant. If downstream dependencies increase the average service time to 60 minutes, the worker suddenly has 4 tickets in flight, potentially exhausting its concurrency budget and starving other operations.

The implication is that **concurrency limits should be derived from measured throughput**, not from static configuration alone. VOOM's observability spec captures "throughput by operation type" and "artifact transfer time and cost" — these are the inputs for dynamic concurrency budgeting.

---

## 7. Two-Phase Commit and the Host Commit Pattern

### 7.1 Two-Phase Commit Protocol

The **two-phase commit protocol (2PC)** coordinates a distributed atomic transaction across multiple participants: in the prepare phase, the coordinator asks all participants whether they can commit; if all vote yes, the commit phase writes the result durably to all participants. 2PC guarantees atomicity even across process failures in most scenarios but is not resilient to coordinator failure during the commit phase without additional recovery protocols.

VOOM's "host owns final commit" design is a pragmatic simplification of 2PC for a local-first architecture. Workers produce **staged artifacts** (the prepare phase); the host verifies checksums and issues a single commit transaction (the commit phase). Because there is only one commit authority (the host), the coordinator-failure problem of distributed 2PC is eliminated. The staged artifact on the filesystem or object store is the durable pre-commit state; if the host crashes before committing, the artifact is treated as abandoned on restart and the ticket is re-queued.

### 7.2 Saga Pattern for Long-Running Plans

For plans that span multiple tickets and may need partial rollback, the **saga pattern** (also called long-running transactions) offers an alternative to distributed 2PC. A saga decomposes a distributed transaction into a sequence of local transactions, each of which publishes an event or message to trigger the next step. If a step fails, **compensating transactions** execute in reverse order to undo already-completed steps.

VOOM's multi-phase plan maps directly to a saga: each ticket is a saga step, and the plan tracks which tickets have committed. If verification fails after a transcode succeeds, the compensating action is to delete the staged artifact and re-queue the transcode ticket. The spec's "bounded replanning" mechanism, which "updates downstream tickets while preserving the audit trail," is the saga's compensation mechanism.

### 7.3 Optimistic Concurrency Control

When the scheduler issues multiple leases in parallel, it must guard against the situation where two workers both attempt to commit results for overlapping scopes. **Optimistic concurrency control (OCC)** assumes conflicts are rare and defers conflict detection to commit time: each transaction reads a version number, performs its work, then attempts to commit only if the version has not changed since the read.

For VOOM, the natural implementation is a `version` or `row_version` column on the `tickets` and `file_versions` tables, incremented on every write. The host commit checks `WHERE id = :id AND version = :expected_version`; if the row has been updated by another transaction, the commit fails with a conflict and must be retried with fresh data. This prevents two concurrent workers from accidentally double-committing to the same file asset.

---

## 8. Concurrency Primitives: Semaphores and Database Locking

### 8.1 Semaphores

A **semaphore** is a variable used to control access to a shared resource by multiple concurrent processes, tracking how many units of a resource are currently available. A binary semaphore (0 or 1) implements a mutex; a counting semaphore allows up to N concurrent holders.

In VOOM, per-worker concurrency limits (`max_parallel: transcode_video: 2`) are counting semaphores. The scheduler decrements the semaphore when issuing a lease and increments it when the lease is released or expires. In SQLite, this is a counter column on the worker record; the scheduler uses `SELECT ... WHERE current_leases < max_parallel FOR UPDATE` to atomically claim capacity.

### 8.2 Two-Phase Locking

**Two-phase locking (2PL)** is a pessimistic concurrency control protocol that guarantees conflict-serializability: transactions acquire all needed locks before releasing any, ensuring no two transactions can conflict. SQLite's default transaction isolation provides serializable semantics for single-writer databases, which covers most VOOM scheduler operations.

However, for multi-row operations that must be atomic (e.g., transitioning a ticket to `LEASED` and simultaneously recording the lease record), it is important that both writes occur in the same transaction at the same isolation level. Mixed-isolation transactions where the ticket update is committed before the lease record is inserted create a window during which the ticket appears leased but no lease record exists — a state that crash recovery cannot correctly interpret.

### 8.3 SKIP LOCKED for Non-Blocking Queue Dequeue

SQLite 3.25+ supports `FOR UPDATE` semantics via `BEGIN IMMEDIATE` transactions. For PostgreSQL (a future deployment target noted in the spec), the `SELECT ... FOR UPDATE SKIP LOCKED` syntax allows multiple scheduler threads to dequeue tickets without blocking each other, each atomically locking a different `READY` ticket row. This is the standard technique for building scalable competing-consumers on a relational job queue and avoids lock-escalation behavior.

---

## 9. Heartbeating, Stale Lease Recovery, and Worker Health

### 9.1 Heartbeat Protocol

VOOM's spec requires that "long-running issuers renew with a heartbeat on a cadence shorter than the TTL." The heartbeat protocol is:

1. Worker acquires lease with `expires_at = now + TTL`.
2. Worker heartbeats at interval `T` where `T << TTL` (e.g., TTL = 30s, T = 10s).
3. Each heartbeat updates `last_heartbeat_at` and extends `expires_at`.
4. The control plane's reaper loop queries `WHERE status = 'leased' AND expires_at < now()` and transitions stale leases to `FAILED` (with `release_reason = issuer_lost`).

Amazon's distributed systems guidance explicitly warns against heartbeating in a background thread, because if the heartbeat thread dies independently, the main work thread holds a belief that it still owns the lease when it does not. VOOM workers should integrate the heartbeat into the main work loop — checking before each significant operation whether the lease is still valid from the control plane's perspective.

### 9.2 The Time-Boundary Problem

A critical edge case: a worker checks its lease at time T=0, finds it valid (5 seconds remaining), then enters a garbage-collection pause or OS scheduling stall for 6 seconds, and writes a commit at T=6 — by which time the lease has expired and may have been re-granted to another worker. The worker's in-process memory says the lease is valid; the database says it is not.

The only correct solution is the fencing token: the commit transaction must validate `WHERE lease_id = :my_lease_id AND expires_at > now()` immediately before the irreversible mutation. This is exactly what VOOM's "Commit Safety Gate" implements: "the check evaluates lease freshness against the control-plane clock immediately before any irreversible filesystem mutation."

### 9.3 Chaos Testing for Recovery Paths

VOOM's spec includes `chaos-worker` as a first-class synthetic provider that "crashes, stalls, corrupts output, misses heartbeats, returns malformed results, and exceeds deadlines." This maps directly to the industry practice of testing recovery paths under controlled failure injection.

The key scenarios to verify under chaos testing are:
- Worker crashes mid-artifact: staged artifact exists, ticket should transition to `FAILED` and artifact to `ABANDONED`.
- Worker completes but network drops before result delivery: lease expires, ticket is re-queued, new worker should not double-commit.
- Control plane crashes during commit: ticket may be in ambiguous state; recovery should determine outcome from filesystem artifact and checksum verification.
- Duplicate lease grant (race in scheduler): only one lease should be active per ticket at a time; second grant must be rejected.

---

## 10. Observability and State Debuggability

### 10.1 Structured Tracing Across Ticket Lifecycle

Propagating a **request/trace ID** through all queued messages correlates events across systems. VOOM's spec requires "trace IDs across plan, ticket, worker, artifact, and event records." The implementation should ensure that the `plan_id`, `ticket_id`, `lease_id`, and `artifact_id` form a traceable chain: every event in the append-only log references the ticket and plan that caused it, so any observed outcome can be traced back to its policy root.

### 10.2 Queue Depth as a Health Signal

Queue depth is the primary early-warning metric for queue-based systems. For VOOM:

- `ready_count` (tickets in `READY` state): length of pending work.
- `leased_count` (tickets in `LEASED` state): current in-flight operations.
- `failed_count` (tickets that failed in the last window): failure rate.
- `age_of_oldest_ready_ticket`: time the oldest ticket has been waiting; a rising value indicates scheduler or worker capacity problems.

These are all computable from `SELECT status, COUNT(*), MIN(created_at) FROM tickets GROUP BY status` and should be exported to the metrics endpoint on a short polling interval.

### 10.3 Explaining Why a Ticket Is Waiting

VOOM's web UI spec requires the ability to "inspect why a ticket is waiting, why a worker was selected, why an artifact placement was chosen." This requires that the scheduler record its decision rationale for each lease issuance as a structured event, including: which workers were evaluated, why each was accepted or rejected (capability mismatch, health degraded, concurrency limit, locality penalty, no eligible worker for path mapping), and what the winning score was. Without this, operators cannot distinguish between "ticket is waiting because all workers are busy" and "ticket is waiting because no worker has the required capability."

---

## 11. Mapping VOOM's Design to These Principles

### 11.1 What the Spec Gets Right

VOOM's architecture already reflects best practice on the most critical dimensions:

| Principle | VOOM Implementation |
|---|---|
| Durable tickets as FSM | `tickets` table with `status` column; append-only `events` for transitions |
| Transactionally staged queue | Tickets live in SQLite; scheduler reads committed `READY` rows |
| Fencing token on commit | Commit Safety Gate checks `lease_id` and `expires_at` inside the commit transaction |
| Idempotent host commit | Worker produces staged artifact; host verifies and commits; re-delivery of same lease is safe |
| Heartbeat-based lease expiry | TTL + `last_heartbeat_at`; control-plane clock is authoritative |
| At-least-once worker dispatch | Lease expiry re-queues ticket; workers produce staged artifacts, not in-place mutations |
| Fail-closed safety gate | Closure resolution abort on incomplete scope; operator force-path for overrides |
| Saga-style compensation | Bounded replanning on phase-boundary assumption failures |

### 11.2 Recommended Additions and Hardening

Based on the research above, the following implementation details merit explicit attention during sprints:

1. **Backoff with jitter on retry**: Store `next_eligible_at = now + capped_exponential_jitter(attempt_count)` in the `tickets` row; the scheduler query should filter `next_eligible_at <= now()`.

2. **Per-worker concurrency counted as atomic semaphore**: Use `SELECT ... FOR UPDATE` (or `BEGIN IMMEDIATE` in SQLite) when checking and incrementing the active-lease counter to prevent race-condition over-grants.

3. **Event emission in same transaction as state change**: The transition `LEASED → SUCCEEDED` and the corresponding `commit_completed` event must share a transaction boundary. Any crash between the two leaves an auditable inconsistency.

4. **`SKIP LOCKED` for competing scheduler instances**: If VOOM ever runs multiple daemon instances (e.g., in a future HA deployment), `SKIP LOCKED` prevents thundering-herd lock contention on the `tickets` table.

5. **Closure-recomputed scope on final gate check**: The spec already mandates recomputing the affected-scope closure immediately before the irreversible mutation. Ensure this is not short-circuited in any fast path during Sprint 5+ real-media work.

6. **Observability: `age_of_oldest_ready_ticket` metric**: Implement this as an early-warning signal analogous to Amazon IoT's `AgeOfFirstAttempt` metric, which separates expected steady-state latency from system-wide backlog indicators.

7. **Chaos test checklist as CI gate**: The `chaos-worker` must cover all of: crash before result, crash after result (network drop), heartbeat missed exactly at expiry boundary, and double-lease-grant race. These should be automated regression tests from Sprint 2 onward.
