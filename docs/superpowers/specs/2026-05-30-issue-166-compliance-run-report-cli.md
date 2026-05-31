---
name: issue-166-compliance-run-report-cli
description: Issue #166 design — grow the `compliance` CLI family into the Sprint 16 scan → plan → execute (run) → report surface. Adds a durable post-run read mode to `compliance report` that returns a completed job's latest-phase report and ordered per-phase chain, and a multi-phase golden-output flow that exercises a real transcode → remux policy end-to-end.
status: draft
date: 2026-05-30
issue: 166
sprint: 16
references:
  - docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md
  - docs/adr/0006-workflow-summary-schema.md
  - docs/adr/0007-phase-barrier-coordinator.md
  - docs/adr/0008-per-phase-report-regenerated-against-refreshed-facts.md
  - docs/adr/0009-resume-opens-new-job-reconciles-prior-rows.md
  - docs/adr/0010-compliance-report-job-read-mode.md
---

# Issue #166 — Multi-phase `compliance execute` run + report CLI surface

## 1. Goal

Sprint 16 §6/§8/§2 grows `compliance execute` into the multi-phase run + report
surface and reconciles the legacy spec §8 transcode-report framing into the
existing `compliance` command family (subsumes #149; **no** separate `voom
transcode` command is introduced). The coordinator, durable summary, per-phase
report regeneration, and resume already shipped (#162/#164/#169/#165). This issue
closes the two remaining gaps in the **CLI surface**:

1. **Post-run report read.** `compliance report` today only *regenerates* a fresh
   report from `--policy-version-id`/`--input-set-id` — the pre-run preview. There
   is no way to read a **completed run's** durable per-phase chain back. This issue
   adds a read mode that, given a job id, returns the latest phase's compliance
   report and the ordered per-phase chain from the durable `workflow_summaries`
   rows (the lineage carrier per Sprint 16 §4/§7).

2. **Multi-phase golden flow.** No `insta` snapshot exercises a genuinely
   multi-phase `compliance execute` (the existing CLI snapshots cover a single
   remux phase). This issue adds a real two-phase policy fixture and golden-output
   snapshots for the full `scan → plan → execute → report` flow.

## 2. Scope

This issue delivers, and is bounded to:

- **A durable post-run read mode for `compliance report`.** A new
  `--job-id <id>` argument (mutually exclusive with the existing
  `--policy-version-id`/`--input-set-id` preview pair) makes `compliance report`
  read the durable summary for a completed job and return the latest phase's
  report plus the ordered per-phase chain. The interface choice (extend `report`
  vs. new subcommand) and the failure contract are pinned by
  `docs/adr/0010-compliance-report-job-read-mode.md`.
- **A control-plane read method** `read_compliance_run_report(job_id)` that reads
  `get_summary` / `phases_for_job` / `file_phases_for_job` (the read API #169
  already exposes) and assembles a `ComplianceRunReportData` view: the job-grain
  summary, the ordered per-phase rows (each carrying its folded `report_id` +
  report JSON), the per-`(file, phase)` rows, and a convenience pointer to the
  latest phase's report. The view reuses the existing `WorkflowSummaryView` /
  `PhaseSummaryView` / `FilePhaseSummaryView` DTOs from `cases::compliance`.
- **A multi-phase mutation policy fixture** with default `on_error` (so it is
  accepted at resolve time — the existing two-phase
  `production-normalize-reduced.voom` declares `on_error: continue`, which ADR-0009
  rejects at resolve time and so cannot drive a run). The fixture declares a
  `transcode` phase (`transcode video to hevc`) and a dependent `remux` phase
  (`container mkv`, `depends_on: [transcode]`), so phase 2 plans and runs against
  the artifact phase 1 produced.
- **CLI golden-output (`insta`) snapshots** for the full flow against that
  fixture: `compliance report` pre-run preview, `compliance execute` multi-phase
  run with summary + per-phase chain, and `compliance report --job-id` post-run
  read. `scan` and `plan` are already covered by their own envelope suites; the
  new multi-phase coverage lives in `compliance_envelope.rs` and drives the same
  scanned-input seeding path the existing remux test uses.

Out of scope (unchanged from the Sprint 16 spec §2/§11, or already shipped):

- The coordinator, per-phase report regeneration, durable summary schema/repo, and
  resume — already merged (#162/#164/#169/#165). This issue adds **no** new durable
  schema and **no** new coordinator behavior.
- A `voom transcode` command (explicitly not introduced; #149 is closed by folding
  the transcode-report framing into this surface).
- Per-file failure isolation, phase re-entry, rollback, `on_error` strategies —
  deferred per the Sprint 16 spec §11.
- A `--job-id` mode for `plan` or `execute`; only `report` gains the read mode.

## 3. CLI surface

The `compliance` command family after this issue:

```text
voom scan --path <file>                      # FileVersion + MediaSnapshot (existing)
voom plan show --policy-version-id --input-set-id      # pre-run plan preview (existing)
voom compliance report --policy-version-id --input-set-id   # pre-run report preview (existing)
voom compliance execute --policy-version-id --input-set-id [--staging-root] [--output-dir]
                                             # multi-phase run + summary (existing handler,
                                             # newly exercised by a multi-phase fixture)
voom compliance report --job-id <id>         # NEW: post-run durable read
```

`scan → plan → execute → report(--job-id)` is the full lifecycle. `plan` /
`compliance report` (preview form) are the pre-run preview; `compliance report
--job-id` is the post-run inspection of what actually ran.

### `compliance report` argument contract

`report` takes **either** the preview pair **or** the job id, never both and never
neither:

- `--policy-version-id <v> --input-set-id <i>` → preview (regenerate, unchanged).
- `--job-id <j>` → post-run read (new).
- Any other combination (all three, only one of the preview pair, or none) →
  `BAD_ARGS` (exit 1) via a clap argument group, routed through `envelope::emit_err`
  so stdout stays a single parseable envelope.

The arguments are modeled as a clap `ArgGroup` (`required = true`,
`multiple = false`) over `job_id` and a second group requiring the preview pair
together, so the parser rejects the bad combinations before the handler runs. The
precise grouping and the "missing one of the preview pair" edge are pinned in the
plan.

## 4. Control-plane read method

```rust
// cases::compliance
pub struct ComplianceRunReportData {
    pub summary: WorkflowSummaryView,
    pub phases: Vec<PhaseSummaryView>,
    pub file_phases: Vec<FilePhaseSummaryView>,
    pub latest_phase: Option<PhaseSummaryView>,   // highest phase_ordinal, None if no phases
}

impl ControlPlane {
    pub async fn read_compliance_run_report(
        &self,
        job_id: JobId,
    ) -> Result<ComplianceRunReportData, VoomError>;
}
```

Behavior:

- Reads `get_summary(job_id)`; a missing job summary returns
  `VoomError::NotFound("workflow summary for job {job_id} not found")` →
  `NOT_FOUND` envelope (exit 2). This is the same not-found contract the preview
  form uses for a missing input set.
- Reads `phases_for_job(job_id)` and `file_phases_for_job(job_id)`. Both repo
  methods already return rows ordered by `phase_ordinal` (then `branch_id`); the
  method preserves that order and does not re-sort.
- `latest_phase` is the row with the maximum `phase_ordinal` (Sprint 16 §7: "the
  CLI report surface returns the latest phase's report"). A job that opened but
  recorded zero phase rows (e.g. an input set with no file targets, per the
  shipped `…no_file_targets…` test) yields `latest_phase: None` and empty `phases`
  — a successful read, not an error.
- The method is **read-only**: it opens no transaction, submits no tickets, and
  regenerates nothing. The reports it returns are the ones the run already folded
  into the rows (ADR-0008), so post-run identity is exactly what `execute`
  returned — no second generation, no drift.

The CLI `report` handler dispatches on the parsed arguments: preview pair →
existing `generate_compliance_report`; job id → `read_compliance_run_report`. Both
emit under `command: "compliance"`.

## 5. Multi-phase fixture and golden flow

### Fixture

`crates/voom-policy/fixtures/policies/transcode-then-remux.voom`:

```text
policy "transcode-then-remux" {
  phase transcode {
    transcode video to hevc
  }
  phase remux {
    depends_on: [transcode]
    container mkv
  }
}
```

Default `on_error` (abort) so the policy is accepted at resolve time (ADR-0009).
Both operations are planner-supported and map to real fake workers
(`fake-transcoder` → `transcode_video`, `fake-remuxer` → `remux`), so the run
dispatches a ticket per phase rather than blocking.

### Golden flow test (`compliance_envelope.rs`)

A new `#[tokio::test]` seeds one scanned `mp4` file (the existing
`seed_scanned_remux` seeding path, generalized to accept a policy source), then:

1. **report (preview):** `compliance report --policy-version-id --input-set-id`
   → snapshot the regenerated report envelope (status `ok`, plan present).
2. **execute:** launch **both** `fake-transcoder` and `fake-remuxer`, run
   `compliance execute … --staging-root --output-dir` with the fake ffprobe
   (`VOOM_FFPROBE_BIN`) → snapshot the run envelope. Asserts: two `completed`
   phases (`transcode` ordinal 0, `remux` ordinal 1), two committed
   per-`(file, phase)` rows, and that phase 1's committed `produced_file_version_id`
   is the chain parent phase 2 ran against (the produced-version linkage, per
   ADR-0008 — not a compliant verdict). The job id is captured from
   `data.summary.job_id` before redaction.
3. **report (post-run):** `compliance report --job-id <captured>` → snapshot the
   durable read envelope. Asserts the per-phase chain length is 2, `latest_phase`
   is the `remux` phase, and each phase carries its folded `report_id`.

Volatile ids (job id, produced version/location, reprobe snapshot, ticket ids,
report ids/hashes that depend on autoincrement target ids) are redacted with the
existing `redact_local` / `redact_execute_ids` helpers, extended as needed so the
goldens are stable across runs. Per the project test-layout rule (AGENTS.md), this
multi-phase run launches the bundled ffprobe on staged output and is therefore
only exercised by `cargo test --workspace`; the fixture media is written by the
harness.

### Why a transcode → remux chain is deterministic

The fake ffprobe returns a fixed `basic-mp4.json` for every probe, so phase 1's
re-probe reports `mp4` facts against the produced version; phase 2 then plans a
`remux` to `mkv` (noncompliant → planned) against those facts and runs. The
produced-version linkage and the ordered two-phase chain are deterministic; the
compliance *verdict* of the produced artifact is not asserted (ADR-0008
consequence: a freshly produced artifact may still read non-compliant because the
planner compares raw probe `format_name` against the policy's canonical
container).

## 6. Error handling

- `report --job-id` for an unknown / never-run job → `NOT_FOUND` (exit 2), single
  envelope. Covered by a CLI test.
- `report` with a missing/extra argument combination → `BAD_ARGS` (exit 1), single
  envelope, asserted by a snapshot. Covered by the clap `ArgGroup`.
- A job whose summary exists but has zero phase rows → `ok` with empty `phases`
  and `latest_phase: null`. Covered by a handler-level test (no worker launch
  needed) that runs the no-file-targets coordinator path then reads it back.
- `execute` failure envelopes (existing partial-data path) are unchanged; the
  multi-phase fixture does not alter them.

## 7. Testing

- **Handler unit tests** (`cases::compliance_test.rs`): `read_compliance_run_report`
  returns the ordered chain and correct `latest_phase` for a multi-phase run;
  returns `NotFound` for an unknown job; returns `ok` + empty chain for a
  zero-phase job. These call the control-plane method directly (no MCP, no CLI
  process), per the handler-is-the-unit rule.
- **CLI golden tests** (`compliance_envelope.rs`): the full `report(preview) →
  execute → report(--job-id)` multi-phase flow above, plus `report --job-id`
  unknown-job `NOT_FOUND` and the `BAD_ARGS` argument-combination snapshot.
- **Argument-parsing test**: the clap `ArgGroup` rejects all-three / only-one /
  none combinations (a `bad_args_envelope`-style assertion).
- `just ci` passes (fmt-check, clippy `-D warnings`, check-test-layout, test, doc,
  deny, audit).

## 8. Acceptance criteria

- `compliance report --job-id <j>` returns the durable summary, the ordered
  per-phase chain (each phase carrying its folded report), and the latest phase's
  report, for a completed run — read-only, with no regeneration.
- `compliance report --job-id` for an unknown job is `NOT_FOUND`; an invalid
  argument combination is `BAD_ARGS`; both emit a single JSON envelope.
- A real two-phase `transcode → remux` policy executes through `compliance
  execute` and is inspectable through the CLI: two `completed` phases, two
  committed per-file rows, and phase 2's run rooted at phase 1's produced version.
- `insta` goldens cover the full `report(preview) → execute → report(--job-id)`
  multi-phase flow and are stable across runs.
- No new durable schema, no new coordinator behavior, no `voom transcode` command.
- `just ci` passes.

## 9. Considered & rejected

These are recorded in full, with consequences, in
`docs/adr/0010-compliance-report-job-read-mode.md`; summarized here so the spec is
self-contained:

- **A new `compliance run-report` / `compliance show` subcommand** instead of a
  `--job-id` mode on `report`. Rejected: the issue title and Sprint 16 §7 frame
  the post-run surface as *the report surface returning the latest phase's
  report*; a `report` job-id mode keeps "the report you preview" and "the report
  that ran" under one verb, and the preview/post-run distinction is the argument,
  not the command.
- **Returning only the latest phase's report** (dropping the per-phase chain from
  the read view). Rejected by Sprint 16 §7: the surface must "expose the per-phase
  chain by reading the ordered summary rows", which is exactly the lineage carrier
  (§4) — collapsing to the latest report discards the lineage the durable rows
  exist to provide.
- **Regenerating the report at read time** from the policy/input rows. Rejected:
  the run already folded each phase's report against its refreshed facts
  (ADR-0008); regenerating at read time could drift from what the run recorded
  (the active version may have advanced, inputs may have changed) and would
  contradict "lineage is the ordered per-phase rows, not a recomputation".
- **Reusing the two-phase `production-normalize-reduced.voom` fixture** for the
  golden. Rejected: it declares `on_error: continue`, which ADR-0009 rejects at
  resolve time, so it cannot drive an `execute` run; a default-`on_error`
  mutation fixture is required.
```
