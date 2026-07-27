# Issue #351 — Executable migration rollback health implementation plan

## Base and guardrails

- Base: `main` at `882eadd4569447e9ee568fbae8a71b23f241937c`
- Branch: `fix/migration-rollback-health`
- Focused check:
  `cargo test -p voom-cli --test health_envelope`
- Shell checks:
  `shellcheck` and `shfmt -d` for any added shell file; none is planned.
- Full check: `just ci`
- Merge gate: #352, #343, and #364 must merge first, followed by rebase and
  full reverification.

## Step 1 — Add the executable contract test

File:

- `crates/voom-cli/tests/health_envelope.rs`

Add helpers that invoke the compiled CLI and execute `jq`. Add a behavior test
that creates current, too-new, clean-partial, and dirty databases, checks the
real exit codes and envelopes, and requires the runbook to contain the exact
`jq` programs being executed. Build the partial database with a subset
`Migrator` containing every embedded migration except the latest, so the
fixture is a genuine forward-migratable older schema.

Expected red: the runbook lacks those predicates and still describes a
nonexistent field.

Verification:

- `cargo test -p voom-cli --test health_envelope
  migration_rollback_runbook_predicates_match_real_health_contract`

Commit boundary: executable health/runbook contract test.

## Step 2 — Correct the rollback procedure

File:

- `docs/runbooks/migration-rollback.md`

Replace `schema_state` with a shell block that captures stdout and the CLI exit
code separately, validates the actual success/error envelopes with the tested
`jq` programs, and rejects unexpected results. Add a decision table for all
three required failure codes. Correct backup direction so partial candidates
move to a newer snapshot or safely forward-migrate, never to an older backup.
Make dirty recovery require deliberate repair or a separately verified
consistent snapshot.

Expected green: the focused test executes the documented predicates against
all four real CLI states.

Verification:

- `cargo test -p voom-cli --test health_envelope`
- re-read every runbook command for copy/paste execution and destructive-action
  prerequisites.

Commit boundary: executable rollback health guidance.

## Step 3 — Review and ship

Review the branch against `main` for wrong exit handling, envelope-shape drift,
unsafe backup direction, partial-versus-corrupt ambiguity, shell quoting,
temporary-file cleanup, and accidental public-contract changes. Run
simplification review, focused tests, and `just ci`.

Push the branch and open a PR with `Closes #351`. Record the complete review
and trajectory annotations. Do not merge or mark awaiting merge before #352,
#343, and #364 are merged and this PR has been rebased and reverified.
