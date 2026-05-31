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
   remux phase). The multi-phase *behavior* is already proven end-to-end at the
   control-plane level by `crates/voom-control-plane/tests/phase_barrier_flow.rs`
   (a two-phase transcode chain against the real `voom-ffmpeg-worker` + real
   ffprobe, both phases commit, durable rows re-read), but with direct assertions
   rather than `insta`, and through the control-plane API rather than the CLI
   process. This issue adds a multi-phase policy fixture and golden-output
   snapshots for the full `scan → plan → execute → report` flow at the **CLI**
   layer. The CLI worker/fixture path is **not yet proven** and is resolved by a
   proof-of-commit gate in the plan (§5) before any golden is captured.

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
  rejects at resolve time and so cannot drive a run). The two phases and the
  worker pairing are chosen by the §5 proof-of-commit gate; the leading candidate
  is a `transcode` phase (`transcode video to hevc`) and a dependent `remux` phase
  (`container mkv`, `depends_on: [transcode]`), with the worker pairing decided by
  the gate, so phase 2 plans and runs against the artifact phase 1 produced.
- **CLI golden-output (`insta`) snapshots** for the full flow against that
  fixture: `plan show` (or `compliance report`) pre-run preview, `compliance
  execute` multi-phase run with summary + per-phase chain, and `compliance report
  --job-id` post-run read. The new multi-phase coverage lives in
  `compliance_envelope.rs` and drives the same scanned-input seeding path the
  existing remux test uses; the pre-run `plan show` snapshot against the new
  fixture is added so the goldens cover all four `scan → plan → execute → report`
  stages against one policy, not only the three `compliance`-family stages.

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
  `BAD_ARGS` (exit 1), routed through `envelope::emit_err` so stdout stays a
  single parseable envelope.

**Mechanism.** A clap `ArgGroup` expresses *at-most-one* / *at-least-one*
membership; it **cannot** require that the two preview args co-occur, so it alone
cannot reject `--policy-version-id` supplied without `--input-set-id`. The
contract is therefore enforced in two layers:

- All three (`policy_version_id`, `input_set_id`, `job_id`) are `Option<u64>`.
- `job_id` carries clap `conflicts_with_all = ["policy_version_id",
  "input_set_id"]`, and the two preview args carry `requires` on each other, so
  clap rejects "job-id + either preview arg" and "only one preview arg" at parse
  time.
- The handler then validates the remaining cases that clap's attribute model does
  not cover cleanly — exactly one of {both preview args present} xor {job_id
  present}, and not none — returning `BAD_ARGS` via `emit_err` for the leftover
  combinations.

This keeps the repo's "even clap parse failures route through `envelope::emit_err`"
contract (AGENTS.md, `main.rs`): whether the rejection comes from clap or the
handler, stdout is one envelope. The exact attribute placement and the handler
validation arm are pinned in the plan; a parsing test asserts every rejected
combination yields `BAD_ARGS`.

## 4. Control-plane read method

```rust
// cases::compliance
pub struct ComplianceRunReportData {
    pub summary: WorkflowSummaryView,
    pub phases: Vec<PhaseSummaryView>,
    pub file_phases: Vec<FilePhaseSummaryView>,
    // Index into `phases` of the highest-ordinal phase (the "latest phase's
    // report" per Sprint 16 §7). `None` when `phases` is empty. An index rather
    // than a duplicated `PhaseSummaryView` so the latest report has a single
    // wire representation that cannot drift from `phases`.
    pub latest_phase_index: Option<usize>,
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
- Reads `phases_for_job(job_id)` and `file_phases_for_job(job_id)`. The repo
  methods order rows by `phase_ordinal ASC` (phases) and `phase_ordinal ASC,
  branch_id ASC` (file-phases) — verified in
  `voom-store/src/repo/workflow_summaries.rs`; the method preserves that order and
  does not re-sort.
- `latest_phase_index` points at the last element of `phases` (which is the
  maximum `phase_ordinal`, since the repo returns them ascending), giving the
  "latest phase's report" per Sprint 16 §7 without copying the row. A job that
  opened but recorded zero phase rows (e.g. an input set with no file targets, per
  the shipped `…no_file_targets…` test) yields `latest_phase_index: None` and
  empty `phases` — a successful read, not an error.
- The method is **read-only**: it opens no transaction, submits no tickets, and
  regenerates nothing. The reports it returns are the ones the run already folded
  into the rows (ADR-0008), so post-run identity is exactly what `execute`
  returned — no second generation, no drift.

The CLI `report` handler dispatches on the parsed arguments: preview pair →
existing `generate_compliance_report`; job id → `read_compliance_run_report`. Both
emit under `command: "compliance"`.

## 5. Multi-phase fixture and golden flow

### Proof-of-commit gate (must pass before any golden is captured)

The headline deliverable — a CLI `insta` golden of a two-phase run with **two
committed phases** — depends on each phase actually committing through the CLI
process. That CLI worker/fixture path is **not yet proven**, and two failure modes
are real:

- The existing CLI transcode test
  (`compliance_envelope.rs::execute_audio_uses_cli_staging_and_output_overrides`)
  tolerates `Some(0 | 2)` — i.e. the fake-worker CLI commit path can legitimately
  exit 2 (job failure). The multi-phase golden needs a *deterministic* exit 0 with
  two `Committed` rows.
- If phase 1 fails to commit + re-probe + advance, the whole job fails (Sprint 16
  §6/§8: an in-phase ticket failure fails the job), there is no phase 2, and the
  post-run-read golden has nothing to read.

Therefore, **before** writing the fixture or any snapshot, the plan's first step
proves a concrete (phase-pair, worker, prober) combination commits **both** phases
to exit 0 through the CLI, using a plain `assert!`-style test (not `insta`). Only
once that test is green is the golden captured over the same setup. Candidate
combinations, in preference order:

1. **`transcode video to hevc` → `remux` to `mkv`**, driven by the real
   `voom-ffmpeg-worker` + real ffprobe — the exact stack
   `phase_barrier_flow.rs` already proves commits two transcode phases. Reusing
   the proven worker stack at the CLI layer is the lowest-risk path; the cost is
   that the golden becomes a real-ffmpeg test (gated like `phase_barrier_flow.rs`,
   `cargo test --workspace` only).
2. **A fake-worker pairing** (`fake-transcoder`/`fake-remuxer` + the fake ffprobe
   stub) — cheaper and hermetic, but only viable if the gate proves the fake
   transcode CLI path commits deterministically (exit 0), which today's tolerant
   test does not establish.

The gate's result — which combination commits — **decides the fixture's phases and
the test's worker launch**. If neither single pairing commits twice, the fallback
is a `remux → transcode` order or a two-`transcode`-phase policy (the proven shape
from `phase_barrier_flow.rs`'s chain test), accepting that "two distinct
operations" yields to "two committed phases." A `remux → remux` chain is rejected
outright: the second remux would collide on the `<stem>.remux.mkv` target path
(the existing `…existing_target_outputs_failure_envelope` test proves that
collision fails the commit).

### Fixture

`crates/voom-policy/fixtures/policies/<name>.voom` — name and body fixed by the
gate. The leading-candidate shape:

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
Both operations are planner-supported and map to workers, so the run dispatches a
ticket per phase rather than blocking.

### Golden flow test (`compliance_envelope.rs`)

A new test seeds one scanned source file (the existing `seed_scanned_remux`
seeding path, generalized to accept a policy source + a probed snapshot matching
the fixture's expectations), then:

1. **plan (preview):** `plan show --policy-version-id --input-set-id` → snapshot
   the plan envelope, so the goldens cover the `plan` stage against this policy.
2. **report (preview):** `compliance report --policy-version-id --input-set-id`
   → snapshot the regenerated report envelope (status `ok`, plan present).
3. **execute:** launch the worker(s) the gate selected, run `compliance execute …
   --staging-root --output-dir` with the appropriate prober → snapshot the run
   envelope. Asserts: two phases recorded in order (`phase_ordinal` 0 then 1), the
   outcomes the gate established (both `completed` for the leading candidate), two
   committed per-`(file, phase)` rows, and that phase 1's committed
   `produced_file_version_id` is the chain parent phase 2 ran against (the
   produced-version linkage, per ADR-0008 — **not** a compliant verdict). The job
   id is captured from `data.summary.job_id` before redaction.
4. **report (post-run):** `compliance report --job-id <captured>` → snapshot the
   durable read envelope. Asserts the per-phase chain length is 2,
   `latest_phase_index` points at the last (ordinal-1) phase, and each phase
   carries its folded `report_id`.

Volatile ids (job id, produced version/location, reprobe snapshot, ticket ids, and
report ids/hashes that depend on autoincrement target ids) are redacted with the
existing `redact_local` / `redact_execute_ids` helpers, extended as needed so the
goldens are stable across runs. Per the project test-layout rule (AGENTS.md), this
multi-phase run launches a prober on staged output and is therefore only exercised
by `cargo test --workspace`; the fixture media is written by the harness. If the
gate selects the real-ffmpeg stack, the test is additionally gated/serialized the
way `phase_barrier_flow.rs` is (hide the fake-ffprobe sibling, build the worker
crates).

### Determinism

Whichever stack the gate selects, the recorded goldens must be deterministic
across runs. For the real-ffmpeg path this is already established by
`phase_barrier_flow.rs` (fixed `testsrc` input, content-addressed report ids). The
golden asserts the produced-version linkage and the ordered two-phase chain, which
are deterministic; it does **not** assert a compliant *verdict* for a produced
artifact (ADR-0008 consequence: a freshly produced artifact may still read
non-compliant because the planner compares the raw probe `format_name` against the
policy's canonical container). Report-id values that are stable functions of the
inputs may be snapshotted directly; any id that varies with autoincrement target
ids is redacted.

## 6. Error handling

- `report --job-id` for an unknown / never-run job → `NOT_FOUND` (exit 2), single
  envelope. Covered by a CLI test.
- `report` with a missing/extra argument combination → `BAD_ARGS` (exit 1), single
  envelope, asserted by a snapshot. Covered by the clap attributes + handler
  validation arm (§3).
- A job whose summary exists but has zero phase rows → `ok` with empty `phases`
  and `latest_phase_index: null`. Covered by a handler-level test (no worker
  launch needed) that runs the no-file-targets coordinator path then reads it
  back.
- `execute` failure envelopes (existing partial-data path) are unchanged; the
  multi-phase fixture does not alter them.

## 7. Testing

- **Proof-of-commit gate** (§5): a plain-assertion CLI test proving the selected
  (phase-pair, worker, prober) combination commits both phases to exit 0 — run and
  green *before* any golden is captured.
- **Handler unit tests** (`cases::compliance_test.rs`): `read_compliance_run_report`
  returns the ordered chain and correct `latest_phase_index` for a multi-phase
  run; returns `NotFound` for an unknown job; returns `ok` + empty chain for a
  zero-phase job. These call the control-plane method directly (no MCP, no CLI
  process), per the handler-is-the-unit rule.
- **CLI golden tests** (`compliance_envelope.rs`): the full `plan(preview) →
  report(preview) → execute → report(--job-id)` multi-phase flow above, plus
  `report --job-id` unknown-job `NOT_FOUND` and the `BAD_ARGS`
  argument-combination snapshot.
- **Argument-parsing test**: the clap attributes + handler validation reject
  all-three / only-one-preview-arg / none combinations (a `bad_args_envelope`-style
  assertion), each yielding `BAD_ARGS`.
- `just ci` passes (fmt-check, clippy `-D warnings`, check-test-layout, test, doc,
  deny, audit).

## 8. Acceptance criteria

- `compliance report --job-id <j>` returns the durable summary, the ordered
  per-phase chain (each phase carrying its folded report), and the latest phase's
  report, for a completed run — read-only, with no regeneration.
- `compliance report --job-id` for an unknown job is `NOT_FOUND`; an invalid
  argument combination is `BAD_ARGS`; both emit a single JSON envelope.
- A real two-phase policy (phases + workers fixed by the §5 gate) executes through
  `compliance execute` and is inspectable through the CLI: two phases recorded in
  order, two committed per-file rows, and phase 2's run rooted at phase 1's
  produced version.
- `insta` goldens cover the full `plan(preview) → report(preview) → execute →
  report(--job-id)` multi-phase flow and are stable across runs.
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
- **A `remux → remux` two-phase chain.** Rejected: the second remux collides on
  the `<stem>.remux.mkv` target path (the existing
  `…existing_target_outputs_failure_envelope` test proves the collision fails the
  commit), so the second phase cannot commit.
- **Fixing the fixture's phases/workers in the spec before proving they commit
  through the CLI.** Rejected: the CLI fake-worker commit path is unproven and one
  existing CLI transcode test tolerates exit 2; pinning the fixture before the
  proof-of-commit gate (§5) risks specifying a chain that cannot reach two
  committed phases. The gate decides the fixture.
