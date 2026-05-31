# ADR 0010 — `compliance report` gains a read-only `--job-id` post-run mode

- Status: Accepted
- Date: 2026-05-30
- Issue: #166 (Sprint 16 §6/§7/§2)
- Related: ADR-0006 (workflow-summary schema), ADR-0007 (phase-barrier coordinator owns one job), ADR-0008 (per-phase report regenerated against refreshed facts)

## Context

Sprint 16 (`docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md`, §7) says
"the CLI report surface returns the latest phase's report and can expose the
per-phase chain by reading the ordered summary rows." The coordinator already
folds each phase's regenerated compliance report into a durable
`workflow_phase_summaries` row keyed `(job_id, phase_ordinal)` (ADR-0008), and the
store exposes `get_summary` / `phases_for_job` / `file_phases_for_job` for reading
those rows back (#169). What does not yet exist is the **CLI surface** that reads
them: `compliance report` today only *regenerates* a fresh report from
`--policy-version-id` / `--input-set-id` (the pre-run preview), with no way to read
a completed run's durable per-phase chain.

Three interface decisions are left open and are settled here:

1. **Where the post-run read lives** — a new mode on `compliance report`, or a new
   subcommand.
2. **What the read returns** — only the latest phase's report, or the full ordered
   per-phase chain.
3. **Whether the read regenerates or reads** the durable rows.

## Decision

**`compliance report` gains a `--job-id <id>` argument that switches it into a
read-only post-run mode. The mode reads the durable `workflow_summaries` rows for
that job and returns the job-grain summary, the ordered per-phase chain (each
phase carrying its folded `report_id` + report JSON), the per-`(file, phase)`
rows, and a `latest_phase` pointer (the highest `phase_ordinal`). It regenerates
nothing.**

- `--job-id` is **mutually exclusive** with the preview pair
  (`--policy-version-id` + `--input-set-id`). The contract is exactly one of:
  - both preview args together → preview (regenerate, unchanged), or
  - `--job-id` alone → post-run read.
  Any other combination (all three, only one preview arg, or none) is `BAD_ARGS`
  (exit 1), enforced by a clap `ArgGroup` so the parser rejects it before the
  handler runs and stdout stays a single parseable envelope.
- The control-plane method `read_compliance_run_report(job_id)` reads
  `get_summary` (NotFound → `NOT_FOUND` envelope, exit 2), then `phases_for_job`
  and `file_phases_for_job`, preserving the repo's `phase_ordinal` / `branch_id`
  ordering. `latest_phase` is the max-`phase_ordinal` row, or `None` for a job
  that opened with zero phase rows (a successful read, not an error).
- The mode is **read-only**: no transaction, no ticket submission, no report
  regeneration. The reports returned are byte-for-byte the ones the run folded
  into the rows (ADR-0008), so post-run identity equals what `execute` returned.

## Consequences

- "The report you preview" and "the report that ran" stay under one verb
  (`compliance report`); the preview/post-run distinction is carried by the
  argument, matching the issue's framing of a single report surface.
- The read view reuses the `WorkflowSummaryView` / `PhaseSummaryView` /
  `FilePhaseSummaryView` DTOs already defined for `compliance execute`
  (`cases::compliance`), so `execute` output and `report --job-id` output share a
  wire shape — an agent parses one schema for both the run and its later
  inspection.
- Because the read never regenerates, it cannot drift from the recorded run even
  if the file's active version has since advanced or its inputs changed. The
  durable rows are the single source of truth for "what this job reported,"
  consistent with ADR-0008's "lineage is the ordered per-phase rows."
- A job that opened but recorded zero phase rows (e.g. an input set with no file
  targets) reads back as `ok` with empty `phases` and `latest_phase: null`, not an
  error — the run *did* happen and is faithfully empty.
- `compliance report` now has two argument shapes a reader must distinguish; the
  clap `ArgGroup` and a `BAD_ARGS` snapshot keep the boundary explicit and
  regression-guarded.

## Considered & rejected

- **A new `compliance run-report` (or `compliance show`) subcommand.** Rejected:
  Sprint 16 §7 frames the post-run surface as *the report surface returning the
  latest phase's report*; keeping it on `report` keeps preview and post-run under
  one verb and makes the distinction an argument, not a separate command. A new
  subcommand would duplicate the report vocabulary and split it across two verbs.
- **Returning only the latest phase's report**, dropping the per-phase chain.
  Rejected by Sprint 16 §7 ("expose the per-phase chain by reading the ordered
  summary rows") and §4 (lineage *is* the ordered rows): collapsing to the latest
  report discards the lineage the durable rows exist to carry. `latest_phase` is
  provided as a convenience *in addition to* the full chain, not instead of it.
- **Regenerating the report at read time** from the policy/input rows. Rejected:
  the run already folded each phase's report against its refreshed facts
  (ADR-0008). Regenerating at read time could diverge from what the run recorded
  (the active version may have advanced; inputs may have changed) and would
  reintroduce exactly the recompute-vs-recorded drift ADR-0008 closed. The read
  must reflect what ran, not what would run now.
- **A `--job-id` mode on `plan` and `execute` too.** Rejected as out of scope:
  `plan` is a pure pre-run preview with no durable run to read, and `execute`
  drives a run rather than inspecting one. Only `report` has a post-run artifact to
  return.
- **Failing the read when `latest_phase` is absent** (treating a zero-phase job as
  an error). Rejected: a job with no file targets is a legitimate successful run
  (the shipped coordinator test pins it); reporting it as an error would conflate
  "nothing to do" with "job not found."
