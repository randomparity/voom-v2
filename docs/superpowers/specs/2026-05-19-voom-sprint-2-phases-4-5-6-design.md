---
name: voom-sprint-2-phases-4-5-6-design
description: Sprint 2 Phases 4, 5, 6 combined design + plan — chaos worker, benchmark worker, and conformance expansion. Each phase ships its placeholder binary or harness extension on `feat/sprint-2`. Full failure-mode implementation (Phase 4 crash/stall/malformed/missed-heartbeat scenarios), throughput measurement (Phase 5 baselines + thresholds), and the typed/raw-wire suite implementations (Phase 6 conformance fill-in) are deferred to follow-up commits.
status: proposed
date: 2026-05-19
sprint: 2
phases: [4, 5, 6]
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 4, §2 Phase 5, §2 Phase 6
scope: scaffolds only; deep implementation deferred per the overview budget
---

# Sprint 2 Phases 4 + 5 + 6 — Chaos, Benchmark, Conformance (combined)

## 1. Goal

Place the three remaining Sprint 2 deliverables on the branch:
chaos-worker and benchmark-worker as additional `voom-fakes` bins
(using `voom-worker-protocol::low_level` rather than the typed
encoder so they cannot mask wire-contract bugs), and the
`voom-conformance` harness extension placeholder.

## 2. Phase 4 — Chaos worker

Adds `chaos-worker` binary to `voom-fakes` (depends only on
`voom-worker-protocol`, NOT `voom-fake-support`). Phase 4 ships a
placeholder that compiles + advertises its scaffold status; the
five failure-mode scenarios (crash / stall / malformed result /
non-converging progress / deadline exceeded) are TODO comments
matched against the overview's exit criteria in
`voom-control-plane/tests/chaos/`.

## 3. Phase 5 — Benchmark worker

Adds `benchmark-worker` binary to `voom-fakes` (same independence:
depends only on `voom-worker-protocol`). Phase 5 placeholder ships
+ exits 0; throughput / latency measurement plus the CI threshold
test (`voom-control-plane/tests/benchmark.rs`) are TODO comments.

## 4. Phase 6 — Conformance expansion

Extends `voom-conformance::Harness::run_*` from the Phase 1 stubs
to the full §4.3 + §4.4 contract surface. Phase 6 placeholder
keeps the stubs from Phase 1 (returning empty `SuiteResult`s) and
adds:

- A `voom-fakes-manifest.toml` consumed by the harness to list
  which binaries to run conformance against;
- A `tests/conformance_all.rs` integration test stub that wires
  `Harness::run_all` against `echo-worker`.

The full §4.3 + §4.4 cases are TODO comments referencing the Phase 1
design.

## 5. Phase 4-6 commits

1. chaos-worker + benchmark-worker bins (single commit).
2. voom-fakes-manifest.toml + conformance integration-test stub.

Adversarial review: one round per phase end.
