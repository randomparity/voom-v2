# ADR 0008 — Per-phase compliance report is regenerated against post-commit refreshed facts

- Status: Accepted
- Date: 2026-05-30
- Issue: #164 (Sprint 16 §4/§7)
- Related: ADR-0006 (workflow-summary schema), ADR-0007 (phase-barrier coordinator)

## Context

The phase-barrier coordinator (`crates/voom-control-plane/src/workflow/coordinator.rs`,
ADR-0007) records a compliance report in each per-phase workflow-summary row keyed
`(job_id, phase_ordinal)`. The spec is explicit about which facts that report must cover:

- §4 "Compliance report storage": each phase's `report_id` and report JSON are "a
  policy-level artifact covering the input set's **refreshed facts at that phase**".
- §7 "Events And Reporting": "Per-phase compliance reports are **regenerated** and
  recorded in the per-phase workflow-summary rows."
- §10 Acceptance: "The compliance report **reflects produced artifacts per phase** with
  lineage."

The coordinator shipped for #162 records the **pre-dispatch** report — the report
generated from the plan that *drove* the phase, computed against each file's chain-tip
snapshot **entering** the phase. By ADR-0007 that entering snapshot is the *prior*
phase's produced artifact. So the recorded report is off by one phase: phase *k* records
the facts of V_k (phase *k-1*'s output), not phase *k*'s own produced artifact V_{k+1}.
The last phase's produced artifact is therefore never recorded in any row.

Two interface decisions are left open and are settled here:

1. **Which facts the recorded report covers, and over which file set**, given that a
   phase advances some files (committed), keeps others unchanged (skipped/no-op), and
   drops others (blocked, abort-for-file per ADR-0007).
2. **How regenerating the report reconciles with the bounded-replan invariant** (spec
   §9: "exactly one plan pass per declared phase, no phase added beyond `phase_order`").

## Decision

**After a phase's branches commit and re-probe and `finalize_phase` advances the
surviving files' chain tips, the coordinator regenerates the compliance report against
the refreshed survivor working set and records that report in the per-phase summary
row.** The pre-dispatch plan and report continue to drive dispatch, unchanged.

- Regeneration re-plans the *same* phase against the refreshed snapshots:
  `phase_draft(&base_draft, &survivors)` → `voom_plan::plan_phase(request, phase_name)`
  → `voom_plan::generate_compliance_report(&plan)`. The resulting `report_id` + JSON
  are written to `NewPhaseSummary.report`. This is a read-only pass: it submits no
  tickets, advances no active version, and adds no phase.
- **File set = the post-`finalize_phase` survivors** (committed-advanced + skipped-kept),
  re-projected at their refreshed chain tips. Blocked files are excluded: abort-for-file
  has removed them from the run, and their non-compliance is already durable in their
  per-`(file, phase)` `Blocked` row.
- **All-blocked phase (no survivors):** the phase carried no compliance artifact forward,
  so the row records `report: None`. The phase outcome (`blocked`) and the per-file
  `Blocked` rows already capture it.
- **Failed phase (in-phase ticket failure):** `finalize_failed_phase` writes no
  phase-grain row, so there is no report to regenerate — unchanged.
- **Deterministic identity preserved:** the `report_id` algorithm
  (`voom-plan/src/compliance_report.rs:30-52`) is unchanged. The regenerated `report_id`
  is a deterministic function of the refreshed preimage; regenerating the same refreshed
  facts twice yields the same id.

### Reconciliation with the bounded-replan invariant

The §9 invariant — "exactly one plan pass per declared phase" — governs the
**dispatch-driving** plan: one pass per phase decides what tickets to submit, and no
phase is added beyond `phase_order`. The report regeneration is a **second, read-only
plan pass** that drives no execution. It does not re-encode, re-dispatch, or add a
phase; the loop is still bounded by `phase_order`. Regenerating (re-planning against the
refreshed snapshot) rather than patching the dispatch plan's observed states is the same
"plan-per-phase, not patch-the-plan" rule the spec applies to the dispatch path (§3),
applied to reporting.

## Consequences

- Each phase's recorded report reflects that phase's produced artifact; the final
  phase's output is now recorded (it previously was not).
- A completed phase runs two plan passes: one to dispatch, one to regenerate the report.
  Both are pure functions over snapshots; the second runs only after commits land and
  only over the survivor set (≤ the files that entered the phase).
- For a phase with no commits (all no-op/skip), the refreshed facts equal the entering
  facts, so the regenerated report is byte-identical (same `report_id`) to the
  pre-dispatch one. The behavior change is observable only for phases that committed.
- The existing chain test's report-target assertions shift: phase *k*'s report now
  targets/observes phase *k*'s produced version, not the prior phase's. The `produced_from`
  lineage assertions (#163, direct `file_versions` reads) are unaffected.

## Considered & rejected

- **Record the pre-dispatch report (status quo from #162).** Rejected: it records the
  facts *entering* the phase (the prior phase's artifact), never the produced artifact,
  and the final phase's output is never recorded — contradicting §4/§7/§10.
- **Patch the dispatch plan's observed states with the re-probe results instead of
  re-planning.** Rejected by spec §3 ("plan-per-phase, not patch-the-plan"): a patched
  plan diverges from what a fresh plan would compute — it would miss a node that becomes
  a no-op, or a selector that newly matches nothing — reintroducing the stale-plan bug
  class the per-phase planner exists to prevent.
- **Regenerate over the full phase-entry set (include blocked files at unchanged tips).**
  Rejected: blocked files have left the run (abort-for-file, ADR-0007); folding them into
  the phase report conflates "compliance after phase *k*" with "files that could not
  enter phase *k*". Their `Blocked` state is already durable per `(file, phase)`, and
  later phases' reports (which no longer see the file) would be inconsistent with it.
- **A `supersedes_report_id` pointer or a reports table to chain pre/post reports.**
  Rejected by spec §4 — there is no reports table and no `supersedes_report_id`; lineage
  is the ordered per-phase rows.
