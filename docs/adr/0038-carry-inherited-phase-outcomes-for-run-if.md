# ADR 0038: Carry inherited phase outcomes for per-file run gates

Status: Accepted

## Context

The published grammar defines:

```text
run_if completed <phase>
run_if modified <phase>
```

These gates are per file. `completed` is true after a successful committed or
skipped predecessor row; `modified` is true only after a committed predecessor
row. A batch-level phase outcome cannot answer either question.

Fresh phase-barrier runs can evaluate a gate from the per-file rows accumulated
earlier in the same job. Resume is harder. ADR 0037 deliberately allows a
resumed job to begin at a non-zero ordinal and retain only its starting cursor,
an optional reconciliation seed, and the rows produced by that job. A later
resume can therefore need a predecessor outcome that is no longer present in
the immediate prior job. The starting version and ordinal cannot distinguish a
committed predecessor from a skipped predecessor.

## Decision

Compiled phases use a typed `CompiledRunIf` containing a `RunIfTrigger`
(`Completed` or `Modified`) and referenced phase name. Its serialized form
remains the existing published compiled shape:

```json
{"type":"predicate","name":"completed inspect"}
```

This keeps existing valid compiled policy versions readable without publishing
a second compiled form.

Each phase-barrier file carries an in-memory map from phase ordinal to
`FilePhaseOutcome`. After a phase is finalized, its new per-file rows update
that map before the next phase is planned.

Migration 0023 adds immutable inherited history rows:

```text
workflow_file_run_history
  job_id
  branch_id
  phase_ordinal
  outcome
```

The key is `(job_id, branch_id, phase_ordinal)`, with a composite foreign key to
`workflow_file_run_starts`. The allowed outcomes are `committed` and `skipped`.
`blocked` is terminal and never inherited by a surviving file.

Fresh jobs insert no inherited history. Resume preparation combines the prior
job's inherited history with its ordinary phase rows and any valid
reconciliation seed. It rejects duplicate, conflicting, out-of-range, missing,
or blocked history for a surviving branch. Job open atomically copies the
combined history beside the new run starts and seeds. A resumed job is therefore
self-contained even if it crashes before writing its first ordinary phase row.

Before planning a gated phase, the coordinator resolves the referenced name to
an earlier phase ordinal and evaluates each entering file:

- `completed`: `Committed` or `Skipped`;
- `modified`: `Committed`;
- missing or inconsistent history: fail with `POLICY_EXECUTION_ERROR` before
  that phase dispatches or mutates a file.

Files whose gate is false are omitted from that phase's planning input. The
existing missing-node classification records them as `Skipped`, so later
`completed` gates observe successful non-mutation. The coordinator clears the
already-resolved gate only in its per-phase policy clone; the stored compiled
policy remains unchanged.

## Consequences

- One modified file cannot admit its siblings into a gated phase.
- Fresh and repeated-resume decisions use the same per-file outcome map.
- Existing phase rows remain the authority for outcomes produced by their job.
  Inherited rows are an immutable carry-forward projection used only when the
  source rows live in an earlier job.
- Preview planning without durable phase history may continue to report a
  typed run gate as unresolved. Execution planning must resolve it in the
  coordinator.
- The new table adds bounded rows proportional to completed phases per resumed
  file. It avoids an unbounded or unavailable traversal of earlier job ids.

## Considered alternatives

### Infer modification from file-version lineage

Rejected. A changed version proves that something modified the file, not which
policy phase did so, and cannot represent a successfully skipped phase.

### Read only the immediate prior job's phase rows

Rejected. Repeated resume permits the immediate prior job to start after the
referenced phase.

### Link every resumed job to its predecessor and traverse the chain

Rejected. It makes every gate evaluation depend on an unbounded cross-job walk
and leaves a new job non-self-contained.
