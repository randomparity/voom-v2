# Issue #330: Per-file `run_if` design

## Goal

Execute the two published phase gates from durable per-file predecessor
outcomes:

- `run_if completed <phase>`
- `run_if modified <phase>`

The behavior must be identical for fresh runs and repeated resumes.

## Boundaries

This issue changes only `run_if`. It does not implement `on_error: continue`
(#335), remux execution gaps (#331/#332), or new condition syntax. Existing
compiled predicate JSON remains readable.

## Typed compiled contract

`CompiledPhase.run_if` becomes `Option<CompiledRunIf>`.

```text
CompiledRunIf {
  trigger: Completed | Modified,
  phase: String
}
```

The source compiler accepts exactly three tokens: `run_if`, a published
trigger, and one existing predecessor phase name. Lowering produces the typed
value. Custom serde preserves the existing `type: predicate` plus canonical
`name` string representation. Malformed stored names fail during compiled
policy deserialization.

Source validation rejects malformed, self, and unknown references. Lowering
checks the reference against the computed topological `phase_order` and rejects
a phase that is not earlier than the gated phase. Both checks happen before a
compiled policy is accepted. No alias or alternative syntax is added.

## Outcome semantics

For one file and referenced phase:

| Durable predecessor outcome | `completed` | `modified` |
|---|---:|---:|
| `Committed` | true | true |
| `Skipped` | true | false |
| `Blocked` | invalid for a surviving file | invalid |
| missing or conflicting | error | error |

Gate-false files receive a `Skipped` row for the gated phase. They remain active
for later phases.

## Fresh execution

Each `PhaseFile` owns a phase-outcome map. It starts empty. When phase `p`
finalizes, each surviving file records the row outcome at `p`. Before phase
`p+1` planning:

1. resolve the typed gate's referenced ordinal;
2. evaluate it separately for every entering file;
3. build the planning draft only from admitted files;
4. plan with the already-resolved `run_if` cleared;
5. classify omitted files as `Skipped`.

No batch or phase-grain outcome participates in the decision.

## Resume execution

`workflow_file_run_history` stores only outcomes inherited from jobs before the
current one. Resume preparation:

1. validates the immediate prior run using ADR 0037;
2. loads its inherited history once;
3. overlays its phase rows in ordinal order;
4. incorporates a valid lost-commit reconciliation seed;
5. rejects conflicts, blocked surviving branches, outcomes at or after the
   next ordinal, and phase ordinals outside the current policy;
6. copies the resulting map atomically into the new job; and
7. attaches the same map to the resumed `PhaseFile`.

The next resume repeats those steps from one self-contained prior job. It never
guesses from the selected version, active tip, or batch summary.

## Failure behavior

- Invalid source syntax produces existing validation diagnostics.
- Invalid typed stored JSON fails policy loading before job creation.
- A gate referencing a later phase fails policy eligibility before job creation.
- Missing or inconsistent inherited history fails resume preparation before
  the new job opens.
- A fresh-run internal history gap fails before the gated phase dispatches.
- No error is converted into a false gate.

## Compatibility and rollback

The compiled JSON wire shape does not change. Migration 0023 is additive and
does not alter existing phase rows or run starts. Jobs created before migration
have no inherited rows; a fresh job is valid, and a resume is valid when all
required predecessor outcomes are present in its immediate phase rows. When
they are not, resume fails closed.

Rolling back the binary after migration requires restoring a pre-migration
database backup; older binaries reject the newer schema version. A newer
resumed job must not be resumed by an older binary because that binary does not
implement published run gates.

## Test strategy

- compiler: exact typed lowering for both triggers; reject missing, extra,
  unknown, self, and non-predecessor forms;
- compatibility: deserialize and reserialize the existing compiled predicate
  shape;
- store: strict schema, allowed outcomes, atomic batch insert, ordered read;
- coordinator unit: committed/skipped truth table, mixed-file isolation,
  missing-history failure, gate-false skipped row;
- resume: inherited history copy, immediate-row overlay, conflict rejection,
  crash-before-first-row and repeated-resume equivalence;
- corpus: S08 and S09 retain their published source and compiled wire forms;
- guardrails: focused tests, Clippy, and `just ci`.
