# ADR 0118: Automatic lease recovery and scan terminality

## Status

Accepted (2026-10-28)

## Context

A standalone owner scan can lose its lease heartbeat without losing the node's
heartbeat. Local fencing cancels its pump, but the API process did not invoke
remote recovery. Explicit recovery could fail the ticket while its scan remained
running until the independent idle deadline. Issue#783 requires bounded automatic
recovery and truthful lifecycle state without weakening owner fencing.

## Decision

The bundled API process explicitly composes a periodic recovery future with its
listener/termination lifecycle. The control-plane library and router constructors
remain passive. Reuse existing lease expiry and scan state/event owners; close
an exhausted scan-routing ticket's active session in the expiry transaction and
reconcile existing orphan pairs after existing recovery phases. Validate persisted
linkage using typed IDs and stored root/owner identity. Store owns SQL.

A local lease fence is a local Fenced outcome, not proof of a remote terminal
acknowledgement. Cancel byte work and report that durable recovery remains pending.
Retain existing explicit recovery and workflow-expiry callers for embedding and
workflow contracts. No new wire fields, operation names or error codes are added.

## Consequences

Standalone scans converge under a running API service even when their node remains
healthy. Recovery is repeatable across concurrency and restart; corruption fails
closed. Pass deadlines/cadence bound background work, while database outage delays
recovery and produces a safe diagnostic. Inspecting/opening the library has no new
mutation side effect. Experiment polling/TTL tuning remains separately owned.

## Considered & rejected

- Client-only cleanup. judgment: cannot provide recovery after client disappearance
  or justify mutations after lease fencing.
- Constructor-spawned recovery. judgment: would add surprising mutation/lifetime
  behavior to inspection, CLI and library embedding.
- Longer lease TTL as the fix. verified: the incident snapshot replay expired the
  lease/failed its ticket and still observed Running scan under a healthy owner;
  diagnostics on Linux x86_64 at fc93ad22, recorded in issue #783.
- General daemon framework or new association schema. judgment: disproportionate
  to this repair while canonical routing payloads already identify scan scope.
