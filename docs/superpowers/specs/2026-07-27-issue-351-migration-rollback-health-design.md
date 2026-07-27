# Issue #351 — Executable migration rollback health diagnosis

## Status

Approved after adversarial review.

## Context

The migration rollback runbook branches on a `schema_state` field that the CLI
does not emit. `voom health` has two real wire paths:

- exit `0`, with schema state at `.data.db.status`; and
- exit `2`, with schema state expressed as a stable `.error.code`.

The runbook also tells an operator who restored a too-old backup and received
`DB_PARTIAL_SCHEMA` to choose an even older backup. That moves farther away
from the downgraded binary's required schema.

The CLI contract itself is already correct. This change makes the operator
procedure consume that contract and adds a test that prevents documentation
and binary behavior from drifting apart.

## Goals

- Diagnose a rollback candidate using the actual `voom health` exit code and
  JSON envelope.
- Provide executable `jq` predicates for the healthy and error envelopes.
- Cover `DB_SCHEMA_TOO_NEW`, `DB_PARTIAL_SCHEMA`, and
  `DB_DIRTY_MIGRATION`.
- Direct backup selection toward the selected binary's compatible schema.
- Prevent an operator from responding to `DB_PARTIAL_SCHEMA` by choosing a
  progressively older backup.
- Keep the existing health envelope, exit codes, error codes, and migration
  behavior unchanged.

## Non-goals

- Adding down migrations or changing the up-only migration model.
- Changing `voom health`, `voom init`, or any public CLI contract.
- Automating database restore, deletion, migration-row repair, or backup
  selection.
- Changing #338, #339, #367, #368, or #369.
- Merging ahead of #352, #343, or #364.

## Decision

### Capture the envelope and exit code separately

The runbook will capture stdout in a `mktemp` file and preserve the `voom
health` exit code before running `jq`. This avoids losing the CLI status in a
pipeline:

- exit `0` is accepted only when the envelope is a successful `health`
  envelope whose `.data.db.status` is `current`;
- exit `2` is accepted only when the envelope is an error `health` envelope;
  its `.error.code` must be one of the three rollback states;
- any other exit code, malformed JSON, mismatched command, or unexpected error
  code stops the procedure.

The predicates also pin `schema_version == "0"`, the `health` command name,
the success/error discriminator, and the expected null side of the envelope.
The temporary file is removed with a shell trap.

### Treat backup selection as a bounded search

The selected binary defines the target schema:

- `current`: the candidate is compatible.
- `DB_SCHEMA_TOO_NEW`: the candidate is ahead of the selected binary. An
  earlier pre-upgrade snapshot may be tried, or the operator may restore the
  newer binary. Moving earlier is permitted only while the diagnosis remains
  too-new.
- `DB_PARTIAL_SCHEMA`: the candidate is behind or its migration metadata is
  damaged. Do not choose an older snapshot. Prefer a newer pre-upgrade
  snapshot. When the error hint explicitly says the schema is a clean,
  migratable partial state, first retain an untouched copy, then run `voom
  init` against a working copy with the selected binary to apply the missing
  migrations forward, and diagnose again. When the hint reports corruption,
  restore a known-consistent newer snapshot or perform deliberate metadata
  repair.
- `DB_DIRTY_MIGRATION`: age does not identify a safe replacement. Reject the
  candidate until the failed migration is deliberately repaired, or select a
  different known-consistent snapshot and diagnose it from the beginning.

Every candidate must pass both SQLite integrity checking and the health
diagnosis. A result other than `current` never proceeds to normal operation.

### Make the documentation executable in tests

`crates/voom-cli/tests/health_envelope.rs` will:

1. create real current, too-new, clean-partial, and dirty SQLite states (the
   partial database is built by running the embedded migrator through the
   penultimate migration, not by deleting migration metadata from a current
   schema);
2. invoke the compiled `voom health` binary;
3. assert exit `0` for current and exit `2` for each failure;
4. read the runbook at compile time;
5. require the documented `jq` programs to match the test's executed programs;
   and
6. execute those programs with the system `jq` against the real envelopes.

The existing corrupt-`schema_meta` binary test remains. Together, it
distinguishes a migratable partial snapshot from corrupt partial metadata
without changing their shared public error code.

`jq` is an existing repository/operator prerequisite: the `just smoke` and
release workflows already use it.

## Failure behavior

- A successful exit with a non-current or malformed success envelope stops.
- Exit `2` with malformed JSON, a success envelope, or an unexpected code
  stops.
- Exit `1` (`BAD_ARGS`) and operating-system termination statuses stop.
- A failed `jq` predicate stops because the procedure runs with
  `set -euo pipefail`.
- No restore candidate is promoted to operation until health reports current.

## Compatibility and rollback

There is no schema, payload, or production-code change. Existing binaries and
databases are untouched. Reverting the documentation/test commit restores the
old guidance but requires no data conversion.

## Security and operational safety

The procedure remains read-only until the operator deliberately restores a
snapshot, runs `voom init`, or performs dirty-migration repair. It never
derives SQL from envelope contents. The health output is written to a
uniquely-created temporary file and removed on exit.

## Test strategy

- Expected red: the runbook does not contain the executable predicates and
  still names `schema_state`; the new contract test fails.
- Current database: exit `0`, success predicate passes.
- Synthetic future migration: exit `2`,
  `DB_SCHEMA_TOO_NEW`, error predicate passes.
- Database migrated through the penultimate embedded migration: exit `2`,
  `DB_PARTIAL_SCHEMA`, error predicate passes.
- Known failed migration record: exit `2`,
  `DB_DIRTY_MIGRATION`, error predicate passes.
- Existing corrupt `schema_meta`: exit `2`,
  `DB_PARTIAL_SCHEMA`, recovery/repair hint remains covered.
- Focused verification:
  `cargo test -p voom-cli --test health_envelope`.
- Full verification: `just ci`.

## Success criteria

An operator can copy the runbook commands, observe the actual CLI exit and
envelope, and deterministically choose the safe next action. The executable
test fails if either the documented predicates or the CLI contract changes.
