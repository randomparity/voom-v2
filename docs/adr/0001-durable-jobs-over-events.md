---
status: accepted
date: 2026-05-15
deciders: [VOOM core]
---

# 0001 — Durable jobs route work; events record facts

## Context

Both the legacy VOOM and several reference systems (Unmanic, FileFlows) rely
on an event bus to drive worker scheduling. This conflates two concerns:
recording that something happened (an immutable fact) and committing to do
something next (durable work). Event-bus claiming makes recovery, audit, and
idempotency harder because the system has no single source of truth for "what
must still happen."

## Decision

Sprint 0 of voom-v2 separates these concerns at the schema level:

- **Tickets and leases** are durable, transactional rows that route work.
  Workers claim tickets via the scheduler; leases expire on heartbeat
  timeout; the host commits final mutations.
- **Events** are append-only facts that record what occurred. They feed UI,
  audit, metrics, and optional reactive plugins. Events do not claim,
  lease, or schedule work.

Both surfaces exist; the architectural promise is that *only durable jobs
route work*.

## Consequences

- Recovery is simple: any node can resume by re-reading ticket/lease state.
- Reasoning about "what will the system do next" is local to the tickets
  table, not a distributed bus.
- Reactive behavior (triggering work on an event) is layered on top of
  durable job creation rather than being the primary mechanism, which costs
  a small amount of indirection but eliminates a class of double-execution
  bugs.
- Event-bus features (transient pub/sub) are not provided in v1; consumers
  read the append-only event log.

## Alternatives Considered

- **Event-bus claiming.** Rejected: history of double-execution and recovery
  pain in similar systems.
- **In-memory job queues.** Rejected: home deployments need crash-safe
  durable state by default.
