---
name: voom-sprint-2-closeout-acceptance-plan
description: Sprint 2 closeout acceptance matrix tying the architectural exit criteria to Phase 6/7 documentation, verification commands, and canonical provider inventory.
status: proposed
date: 2026-05-22
sprint: 2
branch: feat/sprint-2
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-6-fake-providers-conformance-closeout-design.md
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-7-durable-simulated-workflow-design.md
---

# Sprint 2 Closeout Acceptance Plan

## 1. Purpose

This document is the Sprint 2 release-readiness checklist. It does not add
runtime scope; it records the acceptance evidence required to close the three
architectural Sprint 2 exit criteria and resolves the documentation handoff
between early scaffold phases and the final Phase 6/7 closeout docs.

## 2. Acceptance Matrix

| Architectural exit criterion | Owning closeout phase | Required evidence | Verification command | Sprint 2 status |
|---|---|---|---|---|
| A synthetic end-to-end plan runs through the real scheduler. | Phase 7 | The default durable workflow runs through `WorkflowExecutor`: durable job/ticket creation, `SingleWorkerPerKindSelector`, lease acquire/release/fail, process-backed worker protocol dispatch, dependency promotion, terminal job state, branch summaries, and per-operation dispatch counts. | `cargo test -p voom-control-plane --test durable_workflow --all-features` and `just ci` | Required for closeout. |
| Chaos tests cover worker crash, timeout, malformed result, and missed heartbeat. | Phase 7 | Chaos workflow tests use the same executor path and assert stable failure classes for worker crash, dispatch timeout, watchdog-observed missed heartbeat, malformed result, and progress timeout. Missed heartbeat must be watchdog-owned, not an `expire_due` shortcut. | `cargo test -p voom-control-plane --test durable_workflow --all-features` and `just ci` | Required for closeout. |
| Benchmark worker reports scheduler throughput. | Phase 7 | The durable workflow benchmark case reports non-zero scheduler throughput from the implemented `WorkflowExecutor` path. Sprint 2 requires reporting and sanity validation; calibrated hard regression thresholds are deferred until the full supervisor benchmark has enough baseline data. | `cargo test -p voom-control-plane --test benchmark --all-features`, `cargo test -p voom-control-plane --test durable_workflow --all-features`, and `just ci` | Required for closeout. |
| Every Sprint 2 worker binary passes the public protocol contract. | Phase 6 | `echo-worker`, `chaos-worker`, `benchmark-worker`, and the eleven fake providers are active manifest entries; every fixed `OperationKind` has active operation coverage from the manifest, and every `FailureClass` has one named conformance fixture/assertion in the failure-taxonomy registry. | `cargo test -p voom-fakes --all-features`, `cargo test -p voom-conformance --all-features`, and `just ci` | Required prerequisite for Phase 7 closeout. |

## 3. Canonical Sprint 2 Provider Inventory

Sprint 2 owns these eleven fake providers:

- `fake-scanner`
- `fake-prober`
- `fake-transcoder`
- `fake-remuxer`
- `fake-backup-store`
- `fake-health-checker`
- `fake-identity-provider`
- `fake-external-system`
- `fake-quality-scorer`
- `fake-issue-provider`
- `fake-use-lease-provider`

Sprint 2 also owns `chaos-worker`, `benchmark-worker`, `echo-worker`, and
the protocol/conformance support needed to test them. `fake-object-store`
and `fake-transcriber` remain architectural candidates for later fake-provider
expansion; they are not Sprint 2 deliverables.

## 4. Documentation Closeout Checklist

- The Sprint 2 overview points to this closeout matrix and names Phase 7 as
  the implemented scheduler exit surface.
- Phase 7 has explicit exit criteria, not only a test-strategy inventory.
- Phase 2 is marked as a historical scaffold for the deferred standalone
  supervisor/outbox/incarnation design.
- Phase 3 is marked as a historical scaffold superseded by the Phase 6 active
  fake-provider implementation.
- README describes the current Sprint 1/2 workspace instead of the Sprint 0
  skeleton only.

Final Sprint 2 verification is `just ci`.
