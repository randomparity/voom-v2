---
name: voom-sprint-9-closeout
description: Sprint 9 closeout evidence for scheduler scoring, durable decisions, remote acquire integration, and CLI inspection.
status: complete
date: 2026-05-24
sprint: 9
references:
  - docs/superpowers/specs/2026-05-24-voom-sprint-9-design.md
---

# VOOM Sprint 9 Closeout

## Acceptance Matrix

| Requirement | Evidence |
|---|---|
| Reusable scheduler scoring core | `cargo test -p voom-scheduler` |
| Hard eligibility gates | `cargo test -p voom-scheduler scorer_rejects_hard_gate_failures_with_reason_codes` |
| Fixed weights and scoring version | `crates/voom-scheduler/src/lib.rs`, `cargo test -p voom-scheduler` |
| Deterministic tie-breaking | `cargo test -p voom-control-plane remote_acquire_uses_scored_priority_then_tie_breaker` |
| Worker-level concurrency enforcement | `cargo test -p voom-control-plane remote` |
| Node-level concurrency enforcement | `cargo test -p voom-control-plane node_default_limit_blocks_second_concurrent_remote_acquire` |
| Durable scheduler decision persistence | `cargo test -p voom-store scheduler_decisions` |
| Idle/no-candidate suppression | `cargo test -p voom-store suppression_key_keeps_selected_rows_separate_from_idle_rows` |
| Selected and rejected explanations | `cargo test -p voom-scheduler` and `cargo test -p voom-control-plane remote_acquire_no_candidate_is_success_with_decision` |
| Remote acquire scorer integration | `cargo test -p voom-control-plane remote_acquire` |
| Idempotent replay without rescoring | `cargo test -p voom-control-plane remote_acquire_replay_returns_original_scheduler_decision_without_rescoring` |
| Artifact access scoring vocabulary | `cargo test -p voom-scheduler` and `cargo test -p voom-control-plane remote` |
| CLI list/show output | `cargo test -p voom-cli --test scheduler_envelope` |
| Full local CI | `just ci` |

## Deferred Work

Sprint 9 intentionally leaves daemon scheduling loops, scheduling windows,
dynamic throttles, UI controls, production metrics, real media transfer cost
modeling, and policy-configurable scoring weights to the later roadmap phases
named in `docs/specs/voom-control-plane-design.md`.
