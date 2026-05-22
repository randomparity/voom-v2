---
name: voom-sprint-2-phase-2-design
description: Sprint 2 Phase 2 combined design + plan — local worker supervisor. Adds LocalWorkerSupervisor in voom-control-plane (consumes the Phase 1 protocol), the WorkerSelector trait in voom-scheduler, and migration 0005 with worker_incarnations + lease_dispatch_intents for outbox-safe dispatch. Sprint 2 Phase 2 scaffold ships the typed surface + minimal lifecycle so Phase 3 fakes can be wired against it; full crash-recovery tests, watchdog state machine, and identity-verified pgid reap are deferred to follow-up sprints.
status: proposed
date: 2026-05-19
sprint: 2
phase: 2
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 2, §3 (voom-control-plane + voom-scheduler + voom-store rows), §4.8 incarnation outbox, §4.9 watchdog
scope: Historical Phase 2 architectural surface + minimal scaffold; final Sprint 2 closeout uses Phase 7 WorkflowExecutor while the standalone supervisor/outbox/incarnation design remains deferred
---

# Sprint 2 Phase 2 — Local Worker Supervisor (combined design + plan)

> Supersession note: this is a historical scaffold design. Sprint 2
> closeout does not require the standalone `LocalWorkerSupervisor`,
> `worker_incarnations`, or `lease_dispatch_intents` surface described
> here. The implemented Sprint 2 scheduler exit surface is the Phase 7
> `WorkflowExecutor`; the standalone supervisor/outbox/incarnation work
> remains later-sprint design context.

## 1. Goal

Add the control-plane component that registers, supervises, and
dispatches to a single local worker process. Build on the Phase 1
wire protocol; introduce the `WorkerSelector` trait boundary that
Sprint 4 will extend; persist the `worker_incarnations` and
`lease_dispatch_intents` rows the overview §4.8 outbox requires.

## 2. Scope

Crates touched:

| Crate | Phase 2 additions |
|---|---|
| `voom-store` | Migration `0005_worker_incarnations.sql` (worker_incarnations + lease_dispatch_intents). Minimal `WorkerIncarnationRepo` + `LeaseDispatchIntentRepo` with insert + retire + state-transition methods. |
| `voom-scheduler` | `WorkerSelector` trait + `SingleWorkerPerKindSelector` default impl. |
| `voom-control-plane` | `LocalWorkerSupervisor::start / dispatch / shutdown`. Composes Sprint 1 lease lifecycle with the new repos + the Phase 1 `ClientHandle`. |
| `voom-core` | `FailureClass::ProgressTimeout` + `FailureClass::AmbiguousWorkerSelection` (the supervisor introduces both). |

Out of scope for this phase (deferred to follow-up sessions):

- Full crash-recovery test matrix (eight scenarios in overview §4.8). Phase 2 ships
  the schema + the reconciliation code path with **one happy-path
  test**; the seven other crash points are TODO comments in the
  test file with explicit issue references.
- Identity-verified pgid reap (`libproc` / `/proc` probes). Phase 2
  reaps via `Child::kill` only; identity verification is a TODO
  comment marked for the Sprint 5 real-worker hardening sprint.
- Full watchdog state machine (overview §4.9). Phase 2 ships the
  single-arbiter mpsc skeleton + exit/heartbeat/progress events
  but only one paired race test. Five additional race-precedence
  tests are deferred TODOs.

Historical acceptance for this scaffold was limited to landing the
`WorkerSelector` boundary and keeping `just ci` green. Sprint 2 release
acceptance is now owned by the Phase 6 conformance closeout and Phase 7
durable workflow docs.

## 3. Migration 0005

```sql
CREATE TABLE worker_incarnations (
    incarnation_id    INTEGER PRIMARY KEY,
    worker_id         INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    epoch             INTEGER NOT NULL,
    state             TEXT NOT NULL,            -- 'spawning' | 'live' | 'retired'
    pid               INTEGER NOT NULL,
    pgid              INTEGER NOT NULL,
    endpoint          TEXT,
    secret_hash       TEXT NOT NULL,
    binary_path       TEXT NOT NULL,
    process_birth_id  TEXT NOT NULL,
    started_at        TEXT NOT NULL,
    retired_at        TEXT,
    retire_reason     TEXT,
    UNIQUE(worker_id, epoch),
    CHECK (state IN ('spawning', 'live', 'retired')),
    CHECK ((state = 'live') = (endpoint IS NOT NULL)),
    CHECK ((state = 'retired') = (retired_at IS NOT NULL))
) STRICT;

CREATE TABLE lease_dispatch_intents (
    intent_id        INTEGER PRIMARY KEY,
    lease_id         INTEGER NOT NULL REFERENCES leases(id) ON DELETE RESTRICT,
    incarnation_id   INTEGER NOT NULL REFERENCES worker_incarnations(incarnation_id),
    idempotency_key  TEXT NOT NULL,
    state            TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    dispatched_at    TEXT,
    completed_at     TEXT,
    UNIQUE(lease_id, incarnation_id),
    UNIQUE(idempotency_key),
    CHECK (state IN ('pending', 'dispatched', 'completed', 'failed', 'abandoned'))
) STRICT;
```

## 4. Public API

```rust
pub trait WorkerSelector: Send + Sync + std::fmt::Debug {
    fn select(
        &self,
        operation: OperationKind,
        candidates: &[WorkerView],
    ) -> Result<WorkerId, VoomError>;
}

pub struct LocalWorkerSupervisor {
    /* private */
}

impl LocalWorkerSupervisor {
    pub async fn start(...) -> Result<Self, VoomError>;
    pub async fn dispatch(&self, lease_id: LeaseId) -> Result<(), VoomError>;
    pub async fn shutdown(self, grace: Duration) -> Result<(), VoomError>;
}
```

## 5. Phase 2 commits

1. Migration 0005 + `WorkerIncarnationRepo` / `LeaseDispatchIntentRepo`.
2. `FailureClass::ProgressTimeout` + `AmbiguousWorkerSelection` in
   voom-core; exhaustive match adjustments in voom-cli + voom-api.
3. `WorkerSelector` trait + `SingleWorkerPerKindSelector` in voom-scheduler.
4. `LocalWorkerSupervisor` skeleton in voom-control-plane.

Every commit ends `just ci` green. Adversarial review: one round
after the four commits land (per the goal's "no more than three"
budget, used efficiently — one round per phase is allowed).
