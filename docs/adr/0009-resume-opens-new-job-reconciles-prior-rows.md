# ADR 0009 — Resume opens a new job and reconciles against the prior job's per-(file, phase) rows

- Status: Accepted
- Date: 2026-05-30
- Issue: #165 (Sprint 16 §6/§8)
- Related: ADR-0007 (phase-barrier coordinator owns one job), ADR-0006 (workflow-summary schema), ADR-0008 (per-phase report regenerated against refreshed facts)

## Context

The phase-barrier coordinator (`crates/voom-control-plane/src/workflow/coordinator.rs`,
ADR-0007) drives the workflow executor one phase at a time across a multi-file
input set, committing each file's phase artifact inline and persisting a durable
per-`(file, phase)` summary row as it goes. The barrier is **not** transactional:
a whole-job failure mid-barrier leaves some files advanced past phase *k* and
others not (spec §3, §6). `finalize_failed_phase` (ADR-0007, shipped in #162)
already backfills a `Committed` row for every file that committed inline before
the failure, so the durable summary records which files advanced even on a
terminally failed job.

Issue #165 adds the **resume** half of that contract (spec §8): after a crash or
a failed job, re-running the policy against the same input set must

1. never re-mutate a file already advanced past a phase (skip fully-recorded
   `(file, phase)` pairs), and
2. re-enter, for each file, the first phase whose artifact is not yet committed
   for that file.

Three decisions the spec and ADR-0007 leave open:

1. **Job ownership on resume.** ADR-0007 keys all three summary grains to a
   single `job_id` and `jobs` is a strict `Open → {Succeeded, Failed, Cancelled}`
   state machine (`crates/voom-store/src/repo/jobs.rs`, `transition_open_to`):
   terminal states are terminal, with no `Failed → Open` transition. So a resumed
   run cannot continue *inside* the failed job. Does resume reopen the prior job,
   reuse its id, or open a fresh job?
2. **How a resumed run learns which `(file, phase)` pairs are done.** ADR-0007
   notes that re-planning a container-canonicalizing transform against an
   already-produced artifact re-runs it rather than seeing a no-op (raw probe
   container names are compared verbatim). So replanning alone is **not** a safe
   idempotency signal — a recorded-phase check is required.
3. **Unsupported `on_error`.** Non-default `CompiledPhase.on_error`
   (`continue`/`skip`) is out of scope this sprint (spec §6, §11); it must not
   silently regress into partial honoring.

## Decision

**Resume opens a new job and treats the prior job's per-`(file, phase)` rows as a
read-only reconciliation source addressed by an explicit `prior_job_id`.**

- A new crate-public entry point
  `ControlPlane::resume_phase_barrier(prior_job_id, policy_version_id, input_set_id, options)`
  (plus a `_with_runtimes` variant for tests, mirroring `run_phase_barrier`)
  opens a fresh `synthetic.workflow` job and drives the same phase loop. The
  prior job stays `Failed` as the durable record of the failed attempt; the new
  job owns the rows the resumed run writes. The caller already holds
  `prior_job_id` from the failed run's `CoordinatorError.partial.job_id` /
  `CoordinatorOutcome.job_id`, so no job lookup table is introduced (consistent
  with ADR-0007's "no new tables").

- **Reconciliation reads `file_phases_for_job(prior_job_id)`, grouped by
  `branch_id`** (the same path-stem identity the fresh run derives, so prior rows
  match current files). For each active file:
  - A prior row with outcome `Blocked` makes the file **terminal**: the
    abort-for-file already fired (spec §8), so resume excludes the file entirely
    — it is neither re-planned nor revived.
  - `resume_ordinal` is the smallest phase ordinal in `[0, phase_count)` with no
    prior row. Phases below it are recorded (`Committed`/`Skipped`) and are
    **never re-run**; the file's committed artifact (chain tip) is the active
    version the next phase plans against.
  - A file with a row for every phase has nothing to resume and is dropped.

- **Backfill on resume.** Before a file re-enters at `resume_ordinal`, compare
  its current chain tip (`active_version_with_snapshot`) against the produced
  version of its highest `Committed` prior row (or its starting version if none).
  If the tip advanced past that recorded version, a phase committed inline
  without a row (a crash between the inline commit and the row write). The
  coordinator backfills a `Committed` row at `resume_ordinal` by re-probing the
  already-committed tip and advances `resume_ordinal` by one — **no re-mutation,
  no dispatch**. Inline commit and row write are adjacent, so at most the last
  row can be lost; the missing phase is therefore exactly `resume_ordinal` and is
  unambiguous. The backfilled row carries empty `ticket_ids`: the crashed phase's
  ticket linkage is not reconstructed, and the committed artifact plus its
  reprobe snapshot are the durable evidence.

- **Heterogeneous per-file start.** The phase loop carries a per-file
  `resume_ordinal` (`0` for a fresh run, so `run_phase_barrier` is byte-for-byte
  unchanged). At phase `p`, only files with `resume_ordinal <= p` enter the
  draft, are planned, dispatched, and finalized; files not yet at their resume
  phase pass through untouched (no row, no plan) and rejoin at their own
  `resume_ordinal`. The loop still walks `phase_order` once, top to bottom, and
  stops when no files remain.

- **`on_error` rejected at resolve time.** After the policy compiles and **before
  the job opens**, both `run_phase_barrier` and `resume_phase_barrier` reject any
  phase in `phase_order` whose `CompiledPhase.on_error` is `Some(Continue)` or
  `Some(Skip)` with `VoomError::PolicyValidationError` (code
  `POLICY_VALIDATION_ERROR`) naming the phase and strategy. `None` and
  `Some(Abort)` are the default (whole-job abort on in-phase ticket failure,
  unchanged from Sprints 12–15) and are accepted.

## Consequences

- A resumed run's durable summary (the new job) records only the phases it
  actually runs; the prior job retains the earlier phases. The full per-`(file,
  phase)` history therefore spans two jobs after a resume. This is the cost of
  the terminal `jobs` state machine; it keeps job lifecycle and ADR-0007's
  single-job-per-run invariant intact (each *run* still owns exactly one job).
- Idempotency rests on two facts, not on replanning: (a) fully-recorded `(file,
  phase)` pairs are skipped by the recorded-row check, and (b) committed
  artifacts are append-only and are the active version the next phase plans
  against. A container-canonicalizing transform that ADR-0007 would re-run on
  replanning is therefore **not** re-run on resume for any recorded phase.
- The backfill path reuses the same re-probe (`active_version_with_snapshot` +
  `ProducedRefs::resolve`) the failure path uses, so resume adds no second probe
  or commit path (spec §2/§6).
- Rejecting `on_error` at resolve time means a policy that declares
  `continue`/`skip` fails fast with an inspectable diagnostic and never opens a
  job — the limitation cannot silently regress into partial honoring.

## Considered & rejected

- **Reopen the failed job (`Failed → Open`) and continue under its id.** Rejected:
  it requires a new `jobs` state transition that violates the terminal-state
  invariant, and a successfully-resumed run would have to leave the job in
  `Succeeded` after it was `Failed`, erasing the record of the original failure.
  ADR-0007 explicitly rejected adding job-lifecycle concepts.
- **Auto-discover the resumable job from `(policy_version, input_set)`.** Rejected:
  `jobs` rows do not store the policy/input identity, and ADR-0006/0007 rejected
  adding tables or columns for it. An explicit `prior_job_id` (which the caller
  already has) needs no new schema and is agent-native.
- **Rely on replanning seeing a no-op for already-advanced files.** Rejected: ADR-0007
  documents that a container transform re-runs on replanning against the produced
  artifact, so this would re-mutate recorded phases — exactly what spec §8 forbids.
- **Re-derive the backfilled row's `ticket_ids` by replanning the missing phase
  to recover its node id and querying the prior job's tickets.** Rejected: it
  couples a new job's row to the prior job's tickets and adds a replan whose only
  purpose is label recovery; the committed artifact and reprobe snapshot are the
  durable evidence, and empty `ticket_ids` on a backfilled row is an honest record.
- **Treat non-default `on_error` as a documented no-op instead of rejecting.**
  Rejected: a no-op is indistinguishable at runtime from "handled", so a future
  regression that half-implements `continue` would pass silently. A hard reject
  at resolve time is the testable guard the spec asks for.
