---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0017 — `verify artifact` compiles and plans, execution wiring deferred

## Context

`verify artifact` is one of the nine V1 media operations in the DSL grammar
(`docs/specs/voom-control-plane-design.md`). Until now the compiler recognised
the `verify` keyword but always raised `DeferredExecutionOperation`, sharing an
arm with `synthesize`. The V1 operation vocabulary cannot be declared closed
(the Sprint 12–17 charter) while a spec operation is a hard compile error, so
#273 makes `verify artifact` a first-class operation through the policy →
plan pipeline.

Two facts constrain how far #273 reaches:

- **The verify worker exists but is not policy-dispatchable yet.**
  `voom-verify-artifact-worker` and `OperationKind::VerifyArtifact` are already
  in the taxonomy, but there is no `LocalWorkerKind::VerifyArtifact`, so
  `voom worker run-local` cannot start one. Verification today runs *implicitly*
  inside the staged-commit flow, not as a policy-driven dispatched ticket.
- **`verify artifact` takes no arguments.** The spec production is the fixed
  token pair with no filter, target selector, or settings.

## Decision

Split the deferred arm: `verify` is validated and lowered; `synthesize` stays
deferred to #276.

- **Fieldless compiled variant.** `CompiledOperation::VerifyArtifact` carries no
  data (serialises to `{"type":"verify_artifact"}`). The artifact to verify is
  identified by the plan node's target and snapshot, not by operation
  parameters, so there is nothing to model.
- **Validation** accepts exactly `verify artifact`; any other shape is
  `UnknownPhaseStatementOrOperation` (a shape error, not a deferral).
- **Planner** emits an always-`Planned` `PlanOperationKind::VerifyArtifact` node
  with the `verify_artifact` capability hint. Verification targets the artifact
  a prior phase produces, not the source snapshot's streams, so it never
  degrades to a snapshot-shape no-op or block. The payload pins the operation
  and the source snapshot id when the caller already knows it.
- **Plan-level routing only.** The policy execution bridge maps
  `PlanOperationKind::VerifyArtifact → OperationKind::VerifyArtifact` so a verify
  node becomes a dispatchable ticket. Full policy-driven execution (a
  `run-local` verify kind, or folding the node into the implicit staged-commit
  verify, and the ticket-payload binding the worker consumes) is deferred to
  T19 (#288). `policy_worker_requirement` therefore returns `None` for verify:
  there is no `run-local`-startable worker to name in the dead-endpoint hint.

This closes the *vocabulary* (validate + lower + plan) — the #273 goal — without
prematurely committing an execution contract.

## Consequences

- A policy with a `verify artifact` operation compiles, and the golden fixture
  `verify-artifact.voom` pins the compiled shape.
- A verify node routes to `OperationKind::VerifyArtifact`. If such a node is
  ever executed before T19 wires a worker, the coordinator's existing
  per-ticket "no eligible worker" path handles it; no current test drives that.
- Adding the `PlanOperationKind` variant forced completing the exhaustive
  `policy_worker_requirement` match in the control plane. That edit is additive
  and outside the artifact/commit modules.
- `synthesize` remains a hard `DeferredExecutionOperation`, unchanged.

## Considered & rejected

- **Model verify with fields (e.g. an expected-facts struct).** Rejected: the
  expected facts (size, hash, path) are only known after the artifact is
  produced and committed; they do not exist at compile or plan time. A fieldless
  variant is honest about what the DSL actually carries.
- **Wire full policy-driven verify dispatch now (run-local kind + ticket
  binding).** Rejected: out of #273 scope, it belongs with the real-media
  execution work in T19, and there is no `run-local` verify worker to target.
  Fabricating one would be a phantom feature.
- **Map `verify` to a coordinator-only no-op like metadata edits.** Rejected:
  verify genuinely has an executing worker; grouping it with true no-ops would
  misrepresent it. It routes to the worker taxonomy; only the *startup* hint is
  `None`.
