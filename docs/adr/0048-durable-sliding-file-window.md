# ADR 0048: Coordinate policy execution through a durable sliding file window

Status: Accepted

Date: 2026-07-28

Issue: #401

## Context

The phase-barrier coordinator plans and executes one phase across every active
input before any file enters the next phase. That preserves per-file phase
order, but it couples unrelated files and retains every committed intermediate
until the complete input finishes. The issue #339 canary retained 26 GiB in
staging while producing 6.3 GiB of final output.

The executor already owns operation capacity through durable tickets, leases,
and worker grants. The coordinator needs a separate bound on files whose
intermediate lineage may occupy staging. That bound must survive interruption
and must not become a second worker scheduler.

ADR 0007 deliberately chose one `plan_phase` call per phase across the complete
active set. A sliding window cannot retain that choice: a fast file must plan
its next phase from newly probed facts while a slow sibling is still executing
the preceding phase.

## Decision

### One job, independent ordered file pipelines

One coordinator job continues to own the complete execution and its job,
phase, and file-phase reporting. Each admitted file runs its policy phases
sequentially:

1. evaluate the file's gate from its durable/inherited phase history;
2. plan only that file and phase from its current chain-tip snapshot;
3. dispatch the resulting invocation in the shared job;
4. persist the file-phase outcome;
5. refresh the file's version and probe snapshot before planning its next
   phase.

Different admitted files may be in different phases. Each file uses a unique,
deterministic workflow invocation identity derived from the job, input ordinal,
and phase ordinal. Concurrent executor calls therefore cannot claim or inspect
another file's tickets. Tickets and leases remain the only operation scheduler.
When another executor already holds a ticket for the same invocation, the
caller polls durable state until that lease completes, expires through normal
lease recovery, or the job is cancelled. The worker-capacity retry timeout does
not apply to healthy work that has already been leased.

This supersedes ADR 0007 only where it requires one whole-input `plan_phase`
call and a barrier between phases. ADR 0007's single-job ownership, executor
reuse, chain-tip authority, and append-only failure consequences remain.

### Durable admission and cursor state

Migration 0028 adds three job-owned tables:

- `workflow_file_windows`, one row per job, records the positive configured
  maximum and creation time; and
- `workflow_file_progress`, one row for every `(job_id, branch_id)`, with:

  - stable input ordinal;
  - admission tier (`interrupted` before untouched `pending`);
  - state `pending`, `active`, `terminalizing`, or `terminal`;
  - next phase ordinal;
  - admitted and terminal timestamps; and
- `workflow_file_phase_entries`, one row written before dispatch for every file
  that enters a phase, records the exact input snapshot and gate decision used
  for planning and reporting.

The row has a composite foreign key to `workflow_file_run_starts`. Job opening
inserts run starts, inherited history, reconciliation seeds, and progress rows
in one transaction. A transaction may move the next admission-tier/ordinal
`pending` row to `active` only when the active count is below the job's durably
recorded file-window limit. The primary key prevents duplicate admission.
Admission tier prioritizes interrupted work, while ordinal gives each tier a
deterministic refill order. Reads reject a missing, zero, or inconsistent
job-level window record instead of substituting the current process's option.

A file-phase row and the progress cursor advance in the same transaction.
First-write-wins file-phase persistence plus the expected current cursor makes
replayed completion idempotent. Resume still opens a new job per ADR 0009 and
ADR 0037, but it requires the prior window and one progress row per run start,
validates each cursor against the contiguous file-phase tail, and rejects an
option that differs from the durable prior capacity. It admits interrupted
branches before untouched pending branches. Completed operations are seeded or
inherited, not dispatched again.

Migration 0028 also extends inherited run-history outcomes to include `blocked`
and projects legacy phase-barrier run starts into an interrupted admission tier.
Resume carries that terminal outcome transitively so terminalization replay
cannot make a withheld branch promotable again. A new resume job also projects
one progress row for every prior run start: surviving branches retain their
durable input ordinal and a previously terminal branch is inserted atomically
as terminal at the completed cursor. Exact branch-set validation therefore
continues to reject missing active progress while mixed terminal/survivor jobs
remain resumable across repeated failures.

The configured `max_in_flight_files` is a positive execution option and CLI
argument. It defaults to four. It bounds admitted file pipelines, not tickets,
leases, or operations.

### Terminalization precedes refill

A successful file does not release its slot when its final phase row lands. It
first:

1. promotes every terminal main/sidecar artifact scoped to that file using the
   existing add-only promotion path;
2. identifies earlier same-lineage produced locations under coordinator-owned
   working directories, including earlier commit results from the same phase;
3. removes only those retired intermediate files, then retires their durable
   locations while retaining versions, snapshots, commit records, verification
   records, tickets, and file-phase rows as evidence; and
4. marks the progress row `terminal`.

The progress row moves from `active` to `terminalizing` before step 1. Both
states consume a window slot. A crash anywhere in steps 1–3 therefore leaves a
durable terminalization-only branch: resume skips completed policy phases,
replays promotion and cleanup, and only then marks the new row terminal. The
new job carries every prior branch row, including its original ticket ids and
produced references, so terminalization has the same provenance and
intermediate set after repeated interruption.

Missing cleanup files are treated as an interrupted cleanup and completed
idempotently. Source locations are never candidates because cleanup requires a
ticket id carried by a `Committed` file-phase row and validates it before any
promotion or reclamation. Validation binds the ticket's job, workflow phase,
file-scoped input ordinal through its durable progress branch, result
job/ticket/lease, released lease, verification, committed record, source/result
asset, location and snapshot ownership, on-chain source version, and the row's
exact produced result. Only then may a matching location
under a configured committed working directory be reclaimed; the active
chain-tip location is also excluded. This exact carried ticket authority
remains transitive across repeated resume jobs. Verified source locations are
excluded from both promotion and cleanup. Promotion or cleanup failure leaves
the row terminalizing, fails the run, and prevents slot refill.

Promotion uses the same evidence gate. A ticket result with ordered `outputs`
is expanded and each member is validated independently; promotion never
accepts a location directly from unvalidated result JSON. A committed row with
nullable or incomplete artifact evidence cannot bypass validation.
Non-mutating and verification-only results contribute no promotion location.
Primary and sidecar outputs both inherit the owning branch's path relative to
the job-wide source root, so immediate per-branch publication preserves
duplicate basenames from different source subtrees. Cross-filesystem copies use
one deterministic, location-owned hidden partial path; resume removes that
owned partial before retry instead of accumulating full-size orphan copies.

Blocked planning and continued per-file ticket failure terminalize without
promotion. A prior `terminal` branch is already proof that promotion/cleanup
either completed or was intentionally withheld, so a zero-survivor resume does
not promote its carried rows again. An abort-strategy failure stops further
admission synchronously when the failure is selected, before recovery awaits,
and an unwind guard closes admission if a pipeline task panics. Already admitted
work drains. External cancellation likewise stops admission; the job remains
cancelled and its committed file-phase rows remain resumable.

Resume rejects every phase-complete branch whose active chain tip differs from
its recorded row tail. For an incomplete branch, a changed tip advances the
cursor only when a succeeded ticket from the expected prior-job phase proves
the exact committed record, verification, result location, and reprobe
snapshot. A newer same-lineage version without that provenance fails closed.

### Coherent summaries without barriers

Per-file rows remain incremental and authoritative. Phase-entry rows make entry
durable even when dispatch fails before a file-phase completion row. When the run drains—
successfully, after cancellation, or after failure—the coordinator reconstructs
each available phase solely from durable phase entries, file-phase rows,
produced snapshots, and job-scoped tickets. Completed files contribute their
refreshed snapshot; entrants without a completion row contribute the exact
entry snapshot. It folds those facts by phase ordinal into the existing phase
summaries and reports. A phase report therefore contains every file that
actually entered that phase, regardless of completion order, and a resumed job
can rebuild the same inputs without an in-memory completion log from the
interrupted process. Final job counters come from job-scoped durable tickets,
elapsed time uses the coordinator's wall-clock interval, and peak concurrency
is reconstructed from all job lease intervals.

The phase-outcome fold joins completion rows to entries by exact
`(phase_ordinal, branch_id)`. A carried resume row without a phase-entry row is
not an entrant in the new job and cannot numerically replace an entered branch
that failed before writing its completion row.

No CLI JSON envelope or compliance report type changes.

## Consequences

- A fast file may execute, verify, promote, and reclaim intermediates while a
  slow sibling remains in an earlier phase.
- Staging residency is bounded by the configured admitted-file window rather
  than total input count.
- Planning performs more small calls, one per file phase, instead of one large
  call per phase. This is the necessary cost of refreshed per-file progression.
- The progress table duplicates the next ordinal derivable from rows, but makes
  admission and cursor transitions atomic and inspectable. Resume validates
  both representations and fails closed on disagreement.
- A second, job-level row persists the capacity used for every admission
  transition; it is intentionally separate from worker-operation capacity.
- Phase reports become drain-time folds rather than barrier-time writes.
  File-phase rows continue to expose partial progress while execution runs.

## Considered and rejected

### Fixed cohorts

Rejected because a fast file cannot release its cohort's slot until the slowest
cohort member finishes. Staging remains coupled to stragglers.

### In-memory semaphore without durable progress

Rejected because restart could neither prove which files occupied the window
nor distinguish first admission from replay.

### One job per file

Rejected because it fragments the existing job/reporting contract and makes
whole-run cancellation and inspection incoherent.

### Keep whole-input planning and overlap only dispatch

Rejected because a later phase must use each file's newly probed chain-tip
facts. Waiting to form a whole-input plan recreates the barrier.

### Delete every retired file-version location

Rejected because it could delete sources or artifacts outside this run and
would erase recovery inputs. Cleanup is limited to this run's earlier produced
locations under coordinator-owned working directories.

### Keep the existing barriers and increase staging capacity

Rejected because retained intermediates scale with total input size and defer
promotion until the slowest files complete. More storage postpones the failure
point but does not bound it or unblock per-file progress.
