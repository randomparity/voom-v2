# ADR 0037: Persist per-file run starts for resume reconciliation

Status: Proposed

## Context

ADR 0009 resumes a file from the highest phase row in the prior job. When no
committed row exists, it compares the current chain tip with the input-set
starting `FileVersion` to detect a commit whose summary row was lost.

That fallback is not sound once ADR 0036 permits an input set to select a
historical version while planning from the lineage's current active tip. If an
input selects `v0` and `v1` was already active when the prior job began, a
zero-row resume sees `v1 != v0` and falsely backfills phase zero.

The phase ordinal is also unavailable when a resumed job writes no phase rows.
A run may legitimately begin at phase two, then crash before its first row.
Treating the empty prior job as starting at phase zero would repeat two phases.

Resume therefore needs durable facts about where each job began. Neither the
immutable policy input set nor the current lineage can reconstruct those facts
after a crash.

## Decision

Each phase-barrier job records one `workflow_file_run_starts` row per input
file:

```text
job_id
branch_id
starting_file_version_id
starting_phase_ordinal
```

The primary key is `(job_id, branch_id)`. `job_id` is a non-null foreign key to
`jobs(id)` with `ON DELETE CASCADE`, matching the job-owned workflow-summary
tables. `starting_file_version_id` is a non-null foreign key to
`file_versions(id)` with `ON DELETE RESTRICT`. The ordinal is non-negative. The
branch identifier retains ADR 0009's existing file-to-row matching contract.
The table contains no policy facts, snapshots, or mutable cursor: it records
only the immutable starting point of one run.

Fresh preparation records the authoritative active version returned by
ADR 0036 and phase ordinal zero.

Resume preparation is read-only until it has reconciled every current file
against the prior job:

1. Load the prior job's run-start rows and phase rows.
2. Require exactly one run-start row for every current branch and no unmatched
   prior branch. Resolve each starting version and require its file-asset id to
   equal the current branch's selected lineage. Every committed prior phase row
   used for that branch must produce a version from the same lineage. Missing,
   duplicate, or mismatched state fails closed before a new job opens.
3. Validate the prior rows against the starting ordinal `s` and phase count.
   Require `s <= phase_count`; a larger cursor is invalid. Every row ordinal is
   below the phase count. When `s > 0`, rows may contain one committed
   reconciliation seed with empty ticket ids at `s - 1`, followed by a
   contiguous ordinary tail beginning at `s`; either part may be absent. No
   other earlier row or gap is valid, and a blocked row must end the tail.
4. Use the highest prior phase row plus one as the next ordinal. With no prior
   row, use `starting_phase_ordinal`. The validated seed-only case also yields
   the starting ordinal.
5. Use the produced version of the highest committed prior row as the recorded
   tip. With no committed row, use `starting_file_version_id`.
6. Determine terminality before considering backfill. A prior blocked row or a
   next ordinal equal to the phase count is terminal. Its current tip must equal
   the recorded tip; a difference is mismatched state, not evidence for a
   nonexistent later phase. A valid terminal branch records phase count as the
   new job's starting ordinal.
7. Only a non-terminal branch whose current tip differs from the recorded tip
   backfills one committed row at the next ordinal and advances the ordinal.

The new job's run-start rows record the post-reconciliation active version and
next ordinal. Job creation, `job.opened`, all run-start rows, and any
reconciliation backfill rows commit in one transaction. The phase loop begins
only after that transaction succeeds. A crash therefore exposes either no new
job or a complete starting cursor and its seed rows.

The coordinator constructs first-phase `PhaseFile` values from ADR 0036's
single authority result. `version_id` is the current active version and
`resume_ordinal` is the reconciled starting ordinal. The old
`start_version_id` field is removed; reconciliation reads the prior job's
durable run-start record instead.

Jobs created before this schema exists have no trustworthy starting cursor.
When the current request contains at least one file branch, resume rejects such
a job with `POLICY_EXECUTION_ERROR` and the stable prefix
`resume state is incomplete` before opening another job. It does not guess from
the input-set selection or current tip. A zero-file run needs no per-file
cursor and retains its existing zero-work behavior; absence of rows is complete
state for that case.

This decision supersedes only these ADR 0009 details:

- the fallback from a missing committed row to the input-set starting version;
- the fallback from no phase rows to ordinal zero; and
- the conclusion that resume needs no new durable table.

ADR 0009's new-job ownership, explicit `prior_job_id`, terminal blocked files,
single-writer assumption, one-row crash backfill, and heterogeneous phase loop
remain unchanged.

It also supersedes ADR 0007's rejection of a new phase-cursor table and its
conclusion that no cursor is needed, but only for this immutable per-run
starting cursor. ADR 0007's per-file phase rows remain the sole durable record
that a phase completed; the new table is not updated as phases advance.

## Consequences

- Historical input selections cannot be mistaken for a lost phase commit.
- A chain of resumed jobs remains safe when any run crashes before its first
  phase row.
- Resume state becomes explicit and inspectable instead of reconstructed from
  facts that may predate the run.
- One narrow durable table and transactional job-opening path are added.
- Pre-migration jobs with file branches cannot be resumed safely and fail with
  an actionable error instead of risking duplicate or skipped mutation.

## Considered and rejected alternatives

### Continue using the selected input version

Rejected because it may be older than the authoritative tip at job start.

### Use the current tip when resume begins

Rejected because a real commit whose row was lost is already the current tip;
using it as the baseline would repeat the committed phase.

### Infer the baseline from timestamps or identifiers

Rejected because clocks and identifiers do not prove which version was active
when a particular job began.

### Store only the starting version

Rejected because a resumed job can start after phase zero and crash before
writing a phase row. Chained resume also needs the starting ordinal.
