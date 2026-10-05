# Atomic commit-recovery abort and re-prepare (#665)

## Problem

`abort_and_reprepare_report` (`artifact/commit/recovery.rs`) commits the old attempt's
abort in one transaction: intent `aborted`, record `failed`, staging `file_locations` row
retired. It then calls `prepare_commit` in a second transaction. Prepare resolves the
successor's containment root from the source root's current staging default
(`operation_source::resolve_pre_promotion_target`, ADR 0097). If the operator cleared or
changed that default after the original prepare, the second transaction fails after the
first committed. No non-terminal record remains, so a later `recover_commit` returns
`Conflict` ("no non-terminal commit to recover") and the work is stranded. The same
ordering strands work on any other prepare failure, such as an occupied target.

Reproduced at `d674cebd` by
`failed_reprepare_keeps_the_old_attempt_recoverable_until_the_default_returns`: after the
failed re-prepare the old intent reads `aborted`, where `authorized` was expected.

## Design

**One transaction.** The recovery transaction (`begin_read_then_write`) keeps the epoch
compare-and-set and the three abort writes, then runs `prepare_commit_in_tx` on the same
handle. It commits only when prepare returns the successor's record and intent. On any
error the transaction drops and rolls back, so the old record, intent, and staging
location stay exactly as classified, and the same `recover_commit` can run again.

**Prepare must see the transaction's own writes.** Prepare pins the source version's
single live rooted location. The abort just retired the old staging location, which is a
second live rooted location of the same version. A pool read cannot see that uncommitted
retirement, so it would report "multiple live rooted source locations". Prepare therefore
selects the source location with a new `operation_source::select_location_in_tx`. It uses
the same zero/one/many rule as `select_location` through one shared private helper. The
ordinary commit path gets the same transaction-consistent read.

**Staging default reads stay on the pool.** `resolve_pre_promotion_target` reads
`library_roots` through the pool. The transaction does not write roots, and it holds
SQLite's write lock from `BEGIN IMMEDIATE`, so no default change can commit between that
read and our commit. The default that prepare resolves is therefore the one in force when
the successor commits. This closes the window instead of narrowing it. ADR 0097's
fail-closed behavior is unchanged.

**Checked ID conversion.** `list_live_file_locations_by_version_in_tx` already reaches
prepare through the commit safety gate, and source selection now calls it too. It maps a
negative stored id to `VoomError::Internal`, so it moves to the shared `u64_from_i64`. A
corrupt row is then a database error, as AGENTS.md requires.

**Errors.** Every re-prepare failure keeps today's public result,
`VoomError::CommitFailure(<prepare message>)`, so no error code changes.

Ownership is unchanged. Recovery stays in control-plane orchestration, and SQL and
checked conversions stay in `voom-store`. There is no migration, schema change, or new
public contract.

Rejected alternatives:

- **Read-only pre-check before the abort.** It is check-then-act: a default change can
  still commit between the check and the abort. It also misses every other prepare
  failure, for example the occupied target in
  `recover_commit_requires_operator_when_target_already_exists`.
- **Accept the window as a residual (ADR 0103).** The reproduction shows the work is
  stranded, not recoverable, and the criteria require recoverability.
- **Record `ArtifactCommitFailedPreMutation` through a savepoint.** It would add savepoint
  handling only to record an audit event for a recovery that changed nothing durable. The
  caller already receives the error.

## Failure model

1. Actors and deployments
   - Local operators and automation call `recover_commit` (`voom artifact recover-commit`,
     control-plane API) against the control-plane SQLite database.
   - Storage-owner node agents concurrently authorize the commit or report receipts on it.
   - Operators concurrently update root defaults (`voom library root update`).
2. Invariants and assets at stake
   - An old attempt's abort, record failure, and staging retirement commit only together
     with a successor pending record and pending intent in the same transaction.
   - ADR 0074 fencing still applies. If a receipt lands after classification, the epoch
     compare-and-set fails with `Conflict` and nothing commits. A node fenced to the
     aborted intent cannot apply or complete it after the successor exists.
   - ADR 0074 still takes the successor's expected facts from the successful verification.
   - Each artifact handle has at most one non-terminal commit record (repository
     constraints).
3. Accepted failure classes
   - The recovery path records no `ArtifactCommitFailedPreMutation` event when re-prepare
     fails. Nothing durable changed, and the caller gets `CommitFailure`.
   - Recovery holds the write lock while prepare canonicalizes paths. Ordinary prepare
     already does this, and the cost is bounded by one prepare.
   - This change narrows ADR 0074's lease release on abort ("one dead node cannot freeze
     a lease scope indefinitely") to recoveries that can prepare a successor. ADR 0074 does
     not already accept this cost. The operator accepted it on 2026-10-05 as a direct
     consequence of the frozen outcome, with no ADR. While prepare keeps failing, the old
     attempt stays pending or `recovery_required` and keeps the handle's commit slot, the
     intent's lease refusal on its pinned scope, and the node's open-intent listing. On
     success the release was already transient, because a successful re-prepare pins a new
     intent on the same scope for the same owner. Before this change, the failure case got
     the release only by stranding the work.
   - Transient causes hold until an operator fixes them. These are a cleared staging
     default (restore it), a repointed default (revert it, because `record.target_path`
     lies inside the old staging root), and an occupied target (clear it).
   - Permanent causes are a retired source version, or a dead node's retired or inactive
     staging root. These currently leave the intent stuck with no abort path, so its lease
     scope stays frozen. This is a known gap with a follow-up for the orchestrator to file.
4. Covered elsewhere
   - Durable placement state: #677.
   - Root-retirement guard: #678.
   - Never-promoted tip observability: #679.
   - How staging defaults are configured or cleared: #662 (out of scope).

## Validation

All tests are in `crates/voom-control-plane/src/artifact/commit/mod_test.rs` unless named.

- `failed_reprepare_keeps_the_old_attempt_recoverable_until_the_default_returns`
  (failure, then retry). Clear the staging default after authorize and run recovery. It
  returns `CommitFailure`, and the old intent stays `authorized`, its record `pending`,
  its staging location live, with one record in total. Restore the default and run
  recovery again. It returns a pending successor whose intent's expected facts equal the
  verification's size and checksum. The old intent is `aborted`, its record `failed`, its
  staging location retired. Red at `d674cebd`.
- `recovery_fences_the_aborted_intent_after_reprepare` (fencing). After a successful
  re-prepare, `applying` and `complete` reports on the old intent are rejected, and the
  successor intent stays `pending` with no receipt.
- Extend `recovery_abort_fails_closed_when_a_receipt_lands_after_classification`. After
  `Conflict`, assert that one record remains and that it was not failed.
- Extend `recover_commit_requires_operator_when_target_already_exists`. Assert that the old
  intent stays `authorized` and that recovery succeeds once the occupying file is removed.
- `crates/voom-store/src/repo/media/identity_test.rs`
  `list_live_file_locations_by_version_in_tx_reports_negative_id_as_database_error`. A row
  rewritten to a negative id yields `VoomError::Database`. Red at `d674cebd`, where it
  yields `Internal`.
- Existing commit lifecycle tests (`cargo test -p voom-control-plane artifact::commit`,
  `--test recover_commit_gate`, `--test staged_artifact_flow`) stay green. Finally run
  `just ci`.
