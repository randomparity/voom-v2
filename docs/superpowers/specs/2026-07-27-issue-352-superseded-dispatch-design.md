# Issue #352: Superseded lineage dispatch isolation design

## Goal

Prevent fresh and resumed phase-barrier workflows from routing work whose
planned file version was superseded before dispatch began. Prove the failure
through durable job, run-start, ticket, lease, summary, and event state, not
only through an error or observed worker request.

## Review charter

### Outcome and completion criteria

This design is complete when:

- every planned file branch is checked against its current active version at
  the same atomic boundary that creates the phase's root tickets;
- a promotion before that boundary produces `STALE_IDENTITY_EVIDENCE`;
- no stale root ticket, lease, worker request, phase/file summary, or artifact
  effect occurs;
- fresh and resumed runs use the same path;
- prior-job tickets remain historical and cannot bypass the resume guard;
- tests force promotion after planning for both entry points and inspect the
  complete relevant durable state and lifecycle events; and
- existing published DSL and compiled-policy wire shapes remain unchanged.

### Permitted surface

- `voom-store` identity repository: the in-transaction active-version
  predicate and focused repository tests.
- `voom-control-plane` ticket use-case composition: crate-private in-transaction
  create/ready helpers that preserve existing event semantics.
- `voom-control-plane` workflow executor ticket creation: guarded atomic root
  batch and dispatch linearization.
- `voom-control-plane` coordinator: planned-lineage projection, shared
  fresh/resume wiring, and a prepared-resume seam for deterministic tests.
- Sibling unit tests and this ADR/design/plan.

### Dependencies and exclusions

- ADR 0001 governs tickets as the routing authority and therefore the dispatch
  linearization point.
- ADRs 0007–0009 govern phase planning, refreshed facts, one-job execution, and
  replacement-job resume.
- Issue #353, which precedes #352 in the campaign, owns transactional
  policy-input snapshot-link provenance. #352 must rebase after it and may not
  weaken that validation.
- Issue #343 owns deny-wins worker eligibility and atomic worker lease
  acquisition. #352 does not alter candidate eligibility or lease predicates;
  it must ensure no lease path is reachable before the lineage guard commits.
- Issue #334 owns `verify artifact` execution. The guarded root-ticket path is
  operation-agnostic and must not special-case verification.
- Issue #378 owns exact job/ticket correlation for an unrelated promotion that
  occurs after dispatch has linearized. #352 must prevent its pre-dispatch
  rejection from entering partial finalization and must not broaden the
  existing post-dispatch active-tip inference.
- Issues #338 and #339 remain later campaigns. No status or corpus work belongs
  here.
- No migration, use-lease lifecycle, automatic replanning, DSL form,
  compiled-policy field, worker-protocol field, or compatibility shim is in
  scope.

These exclusions are valid only if the guarded phase cannot dispatch stale
work without them.

## Existing flow and race

`prepare_phase_barrier_run_inputs` resolves each stored file member to:

- its selected version's `FileAsset`;
- the asset's greatest live `FileVersion`; and
- that exact version's latest `MediaSnapshot`.

`PhaseFile.version_id` retains that active version through synchronous phase
planning. Fresh and resumed paths then converge in `PhaseLoop::run`:

```text
resolve active version/snapshot
  → open job
  → plan phase from retained PhaseFile facts
  → bridge plan
  → create root ticket(s)
  → mark ready
  → acquire worker lease
  → send request
```

Today a promotion can commit anywhere after resolution. A query inserted before
the bridge or executor still leaves a race before the first ticket write.

## Dispatch linearization contract

For a phase with at least one `Disposition::Planned`, build a lineage
expectation for each planned branch:

```text
(PhaseFile.asset_id, PhaseFile.version_id)
```

Blocked, skipped, compliant, and gate-excluded branches are omitted because
the phase routes no work for them. Input-set validation already rejects two
members from the same asset, so expectations are unique.

The executor renders every root ticket specification first. Rendering may read
locations and configuration but writes no ticket, event, lease, or artifact.
It then opens `BEGIN IMMEDIATE`, which either waits for an earlier promoter or
holds SQLite's reserved writer lock ahead of a later promoter.

The coordinator cannot select an unguarded production overload. A crate-private
`PlannedLineageGuard` constructor takes the planned-disposition count and the
expectations derived from the same zipped `(entering file, disposition)` walk.
It requires a non-zero count, exact count/expectation cardinality, and unique
assets. The same count is passed to `WorkflowExecutionShape`. The sole
production in-job executor entry point accepts that validated type, not a slice
or `Option`; the unguarded entry used by executor-only tests is compiled only
under `cfg(test)`. Deleting the guard is a compile error, while dropping only
one planned branch makes guard construction fail before the bridge or job
dispatch.

Inside that one transaction:

1. The identity repository queries the greatest live `file_versions.id` for
   each expected asset. It intentionally does not join `media_snapshots`: a
   newly promoted but not-yet-probed version still supersedes the planned tip
   and must reject.
2. Any absent or mismatched tip returns `STALE_IDENTITY_EVIDENCE`. Transaction
   drop rolls back all root rows and ticket events.
3. If all expectations match, the executor creates all root tickets and their
   `ticket.created` events.
4. It marks all roots ready and appends their `ticket.ready` events.
5. One commit makes the guarded root batch visible.

The commit is the phase dispatch linearization point because ADR 0001 says the
ready ticket is the durable routing mechanism. No worker candidate lookup,
lease acquisition, task spawn, or client call occurs before it.

A promotion ordered after this commit may precede the eventual network write,
but it is not a promotion *before dispatch*: durable dispatch already began.
Closing the post-dispatch interval would require a long-lived claim and is not
part of #352.

The distinct, pre-existing risk that phase finalization may attribute such a
later external tip to the running job is tracked by #378. #352 neither solves
nor worsens that post-dispatch ownership inference. Its non-regression boundary
is exact: the new pre-dispatch stale error carries `dispatch_started = false`
and therefore never reaches that inference.

## Executor failure classification

`WorkflowRunError` gains a `dispatch_started` marker defined solely by the
guarded root-batch commit:

- `false` for plan validation, root payload rendering, lineage predicate,
  ticket/event creation, readiness, or commit failure; and
- `true` for every failure after the guarded commit.

The coordinator sets `PhaseDispatchFailure.run_summary` only when
`dispatch_started` is true. Existing `finalize_failed_phase` logic uses the
presence of that summary as proof that inline worker commits may have landed.
This distinction is load-bearing: after a stale rejection the database's active
tip is the external promotion, not output from the failed job. Passing an empty
executor summary as `Some` would cause the partial finalizer to claim that tip
as a committed file-phase result.

## Failure behavior

The predicate returns the existing public error variant:

```text
STALE_IDENTITY_EVIDENCE:
phase dispatch rejected for file asset A: planned file version V is no longer
active; current active file version is N; replan or resume from current inputs
```

If no live version exists, the message says so instead of inventing an ID.
Database errors retain their database code and source context.

The guarded transaction is all-or-nothing. If one planned branch is stale:

- the owned job is finalized `failed`;
- `job.opened` and `job.failed` exist, with the stale reason in the latter;
- fresh/resume file-run-start provenance written with job open remains;
- resume history/seeds written before phase dispatch remain valid provenance;
- no ticket row or ticket event for the rejected phase exists;
- no lease row or lease event exists;
- no workflow, phase, or new file-phase summary is written by the rejected
  phase; and
- no worker client is called and no artifact state changes.

The earlier failed job and its tickets are untouched during resume.
The new job has no partial `CoordinatorOutcome`; the external promoted version
appears only in identity tables, never as its produced file-phase provenance.

## Resume composition

ADR 0009 opens a new job for every resume. `prepare_resume` reconciles prior
file-phase rows into replacement-job run starts/history/seeds; it never
reactivates or copies prior tickets. The replacement phase produces a new
workflow ID and new roots.

The public resume path will delegate to a crate-private prepared-resume helper,
parallel to the existing prepared-fresh helper. This is a deterministic test
seam, not a second execution path: both helpers enter `drive_phase_loop`, and
the phase loop supplies planned lineage expectations to the same guarded
executor method.

Within one replacement job, every resumed/later phase uses a distinct
`phase-{ordinal}` workflow ID. Earlier phase tickets cannot satisfy the new
phase's ready-ticket query, and every new root batch repeats the guard using
that phase's refreshed `PhaseFile.version_id`.

## Transaction and event composition

`ControlPlane::create_ticket` and `mark_ready_if_unblocked` currently own their
transactions. Add crate-private `_in_tx` forms, following `open_job_in_tx`, that
compose repository writes with the same events. Existing public methods become
thin transaction wrappers over them.

The guarded executor method calls only these `_in_tx` forms while holding its
`BEGIN IMMEDIATE` transaction. There is no direct ticket insert without its
event and no ready transition without `ticket.ready`.

Test-only unguarded workflow executor entry points preserve their current
per-root transaction behavior. Production in-job phase invocation always
carries a non-empty validated guard and uses the guarded batch.

## Compatibility and rollout

There is no migration and no durable payload change. New binaries can execute
existing policies, input sets, jobs, and summaries. Old binaries can read every
row produced by new binaries.

Rollback restores the old race but encounters no unknown state. Operators
should avoid mixed-version concurrent execution during rollback; no data
conversion is required.

The branch begins before campaign issues #346, #358, and #353 merge. Before
merge, rebase serially onto their merged `main`, resolve any input-resolution
overlap without weakening provenance, and rerun focused tests plus `just ci`.

## Test strategy

### Store predicate

- expected live tip succeeds inside a caller-owned transaction;
- a newer live version fails with `STALE_IDENTITY_EVIDENCE` and identifies both
  IDs;
- a newer live version without a snapshot still fails; and
- an asset with no live version fails visibly.

### Atomic ticket boundary

- a current guard creates every root plus `ticket.created`/`ticket.ready` in one
  transaction;
- an injected stale expectation rolls back the complete multi-root batch and
  both event kinds;
- the `PlannedLineageGuard` constructor rejects empty and duplicate-asset
  expectations and rejects any mismatch between planned-disposition count and
  expectation count;
- a separate connection begins an immediate transaction and inserts an
  uncommitted newer version, then the test calls the production guarded
  in-job executor method. The executor task must remain pending while the
  promoter owns the writer lock; after the promoter commits, the executor must
  return `STALE_IDENTITY_EVIDENCE` with no root or event. Replacing the
  production opener with deferred `BEGIN` must make this test fail; and
- pre-root executor errors carry `dispatch_started = false`, while a failure
  after a successfully committed root batch carries `true`; and
- existing unguarded executor tests remain green.

The concurrency test uses real time and a real `SqlitePool`; it never pauses
Tokio time.

### Fresh workflow

Prepare a one-file policy run against version V1, append and snapshot V2 after
preparation, then run the prepared fresh path with an eligible in-process
worker whose client counts calls. Assert:

- `STALE_IDENTITY_EVIDENCE` names the asset, V1, and V2;
- the replacement job is `failed`;
- its run start records V1;
- job events are exactly opened then failed with the stale reason;
- new-job ticket, lease, workflow-summary, phase-summary, and file-phase counts
  are zero;
- the coordinator error has no partial outcome, and V2 is absent from every
  produced-version field for the new job;
- ticket/lease lifecycle event counts for that job are zero;
- the dispatch client count is zero; and
- V2 remains the active tip with no artifact-produced V3.

A second fresh case uses two planned files and promotes only the second. It
must reject the whole phase with no roots for either file. This catches
first-only or truncated expectation construction.

### Resumed workflow

Seed a prior failed job with its V1 run start and a historical failed ticket.
Prepare resume inputs at V1, append/snapshot V2, then invoke the prepared-resume
path. Assert the same new-job failure state plus:

- the prior job and its ticket/event state are byte-for-byte/field-for-field
  unchanged;
- the replacement job has no ticket or lease;
- no prior ticket is leased or sent;
- resume history/seeds have no fabricated completion for V2; and
- the dispatch client count is zero.

### Sensitivity

Temporarily remove the active-version comparison and run both stale workflow
tests. Each must fail because a ticket/lease/client call occurs or the returned
error is no longer stale. Temporarily replace the production
`BEGIN IMMEDIATE` with deferred `BEGIN`; the uncommitted-promoter test must fail.
Restore both mutations and rerun green.

## Verification

- `cargo test -p voom-store active_file_version`
- `cargo test -p voom-control-plane superseded`
- `cargo test -p voom-control-plane workflow::coordinator`
- `just fmt-check`
- `just lint`
- `just ci`
