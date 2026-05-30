---
status: accepted
date: 2026-05-29
deciders: [VOOM core]
---

# 0005 — Per-phase planner entry point `plan_phase`

## Context

Sprint 16 (`docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md`, §5)
drives a multi-phase real-media policy one phase at a time: each phase is
planned against the artifact the prior phase produced, then run and committed
before the next phase is planned. The existing entry point
`generate_plan(PlanningRequest)` expands **every** phase in `phase_order` up
front against the original snapshot, so it cannot serve a coordinator that needs
to re-plan a single phase against a refreshed snapshot.

The spec settles the behaviour (§5, §8); what it leaves open is the Rust
interface for the new entry point and its failure contract. Two decisions need
pinning before the coordinator (#162) is built against this surface:

1. **Signature.** What does `plan_phase` take, and how does it reuse the
   existing single planning code path rather than forking it?
2. **Failure contract.** A phase can fail to plan for two distinct reasons —
   an operation that no longer matches the refreshed artifact (a track selector
   matching nothing) versus a caller asking for a phase that is not declared in
   `phase_order`. These must be distinguishable so the coordinator can turn the
   first into an inspectable blocked issue and treat the second as a bug.

## Decision

Add `plan_phase(request: PlanningRequest, phase_name: &str) -> Result<ExecutionPlan, PlanGenerationError>`
alongside `generate_plan`, sharing the same `PlanBuilder`:

- `plan_phase` validates the input set, builds the same `PlanBuilder`, expands
  **only** the named phase, and calls the same `finish()`. `generate_plan`'s
  per-phase loop and `plan_phase` both route through one private
  `expand_named_phase` helper, so there is a single planning code path. Each
  phase's plan is deterministic from `(compiled policy, phase, snapshot)`.
- The caller supplies the projected snapshot inside `request.input`
  (`PolicyInputSetDraft.media_snapshots`). Projecting the current snapshot into
  that request is the coordinator's job (#162), not the planner's. Taking the
  full `PlanningRequest` keeps the policy/context identities that `finish()`
  needs for `plan_id`/`plan_hash` and matches `generate_plan`'s by-value
  convention.
- `run_if`/`skip_if` are re-evaluated against the supplied snapshot through the
  existing `expand_phase` path: a skipped phase produces **zero** nodes (the
  coordinator reads "zero nodes" as skipped), distinct from a compliant phase
  whose operations all evaluate to `NoOp` nodes.
- **Unplannable operation** (e.g. a selector matching nothing) is **not** an
  `Err`: it produces a `Blocked` node plus a `PlanningDiagnostic`, exactly as
  `generate_plan` does today. The coordinator turns that diagnostic into a
  blocked issue.
- **Phase not in `phase_order`** is a hard `Err(PlanGenerationError)` with an
  `InvalidPlanningRequest` diagnostic. The bound is the declared phase count;
  a phase outside it can never be planned, and asking for one is a coordinator
  bug that must fail loud rather than silently return an empty plan.
- **Phase in `phase_order` but absent from `phases`** (an internally
  inconsistent compiled policy) is the *symmetric* structural error and is also
  a hard `Err`. `generate_plan` tolerates this with a non-fatal diagnostic so it
  can keep planning sibling phases, but `plan_phase` must not: a node-less `Ok`
  plan here is indistinguishable from a legitimately skipped phase, exactly the
  ambiguity the `phase_order` guard above exists to prevent. The two structural
  errors therefore share one failure shape (`Err`), distinct from skip
  (`Ok`, zero nodes, zero diagnostics).

A single-phase plan carries no inter-phase edges: `build_phase_edges` only emits
an edge when both endpoints' nodes are present, and only the target phase's
nodes exist. Inter-phase ordering is the coordinator's barrier (§3), not encoded
in a per-phase plan.

## Consequences

- The coordinator (#162) gets a deterministic, idempotent per-phase planning
  call it can invoke once per `(file, phase)` against a refreshed snapshot.
- `plan_phase` ships before its first caller (#162); it is a spec-advertised
  surface, not dead code.
- Replanning is structurally bounded by `phase_order`: there is no API by which
  a phase outside the declared order can be planned.
- The two failure modes are typed apart — `Ok` plan with a `Blocked` node and
  diagnostic (unplannable, → blocked issue) vs `Err` (phase not declared, → bug)
  — so the coordinator never conflates a real-media dead end with a wiring
  error.

## Alternatives Considered

- **`plan_phase(&CompiledPolicy, &CompiledPhase, &MediaSnapshotInput)`** — the
  literal three-argument shape from the issue title. Rejected: it bypasses
  `PlanningContext` (needed for `plan_id`/`plan_hash` and the policy/input
  identities) and `validate_input`, and would force a second plan-assembly path
  diverging from `generate_plan`. The "(compiled, phase, snapshot)" framing is
  the conceptual contract; the snapshot rides inside `PlanningRequest.input`.
- **Phase-not-in-`phase_order` returns `Ok` with an empty plan.** Rejected:
  silently returning a no-op plan for a misnamed phase hides a coordinator bug
  (AGENTS.md Rule 12, fail loud) and is indistinguishable from a legitimately
  skipped phase.
- **Unplannable operation returns `Err`.** Rejected: it conflates a real-media
  dead end (which the spec requires be recorded as an inspectable blocked issue,
  §8) with a structural request error, and would discard the partial plan and
  diagnostics the coordinator needs to report.
- **A new `Planner` struct / second planning module.** Rejected: violates the
  spec's single-planning-code-path requirement (§5) and AGENTS.md Rule 3
  (simplicity); the existing `PlanBuilder` already expands per phase.
- **Patch a whole-policy plan down to one phase after the fact.** Rejected
  explicitly by the spec ("plan-per-phase, not patch-the-plan", §3): filtering
  nodes out of a full plan would not re-evaluate `run_if`/`skip_if` against the
  refreshed snapshot.
