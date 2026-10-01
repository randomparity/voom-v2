# Refuse retiring a storage root another root names as a default

Issue: #626. Scope: q626-3e47932d. Lane: full-spec, S (100 lines).
Plan: `docs/workflow/plans/2026-10-01-retire-referenced-root.md`.

## Problem

`retire_library_root_in_tx` (`crates/voom-store/src/repo/library/library_roots.rs`)
checks only that the target is not already retired. A root that another root names as
`default_output_root_id`, `default_staging_root_id`, or `default_backup_root_id` retires
silently. Under ADR 0097 the staging default is the containment root of a pre-promotion
`.committed` address, so its retirement strands committed-but-unpromoted artifacts. ADR 0097
records this as an accepted, unowned residual.

## Decision

1. **Retire refuses a referenced root.** Inside the retire transaction, after the existing
   already-retired check and before the `UPDATE`, one query selects every root that is
   *not the target* and *not retired* and holds the target's id in any of the three default
   columns, ordered by id. A non-empty result returns `VoomError::Conflict`:
   `storage root <id> cannot retire while other roots name it as a default: root <a>
   (default_staging_root_id), root <b> (default_output_root_id, default_backup_root_id);
   repoint those defaults or retire the referencing roots first`. Nothing is written.
2. **Excluded referencers.** A root's reference to itself does not block (retiring it retires
   the referencer too). A retired referencer does not block: `update_library_root` refuses
   retired rows, so the operator could never clear that reference. Mutual references (a
   staging root whose output default is its referencer) resolve by repointing one side,
   possibly to itself.
3. **`active -> unavailable` stays unguarded.** ADR 0055 makes `unavailable` the record of an
   observed validation loss, reversible by reactivation under the same owner. Refusing it
   would keep a lost root persisted as `active`, which is worse than recording the loss. The
   reference survives the outage, so retirement stays guarded afterwards.
4. **ADR 0097** gains an appended `## Later decision: retirement refuses a referenced
   default` section (the convention ADRs 0019/0025/0027/0034/0055/0069 follow); its body is
   not edited.

Rejected: auto-clearing referencing defaults (operator exclusion).

## Failure model

- **Actors and deployments:** a local operator running `voom library root retire`; control-
  plane callers of `retire_library_root` / `mark_library_root_unavailable`.
- **Invariants at stake:** a non-retired root's default columns never name a retired root
  through the retire path; retire stays atomic with its `StorageRootRetired` event.
- **Accepted:** a concurrent create/update adding a reference during retire. Retire runs
  under `BEGIN IMMEDIATE`; create also does, and update's deferred transaction cannot upgrade
  a pre-retire WAL snapshot to a write, so it fails instead of writing a stale reference.
  Repointing a staging default between commit and promotion, then retiring, still strands
  an unpromoted artifact; it is not guarded (not in the
  charter; reported as a follow-up candidate). Disabling a referenced root is not guarded
  (not in the charter). The error lists every referencer; library root counts are small.
- **Covered elsewhere:** pairing validation at configuration time (#616); resolver
  containment (#625); a CLI flag to clear (rather than repoint) a default (follow-up
  candidate — the CLI cannot clear today, so the message offers only repoint or retire).

## Success

1. Retiring a root named as a default by a non-retired other root returns `Conflict` naming
   each such root, its column(s), and the fix; the target row is unchanged.
2. After the referencing defaults are repointed or cleared, the same retire succeeds.
3. Self-references and retired referencers do not block retirement.
4. Marking a referenced active root unavailable succeeds.
5. `just ci` green.

## Validation

- Criteria 1–3 — `focused-test`: `library_roots_test.rs`
  `retire_refuses_a_root_other_live_roots_name_as_a_default`. Red on main: retire succeeds.
  Green: `cargo test -p voom-store --lib retire_refuses_a_root`.
- Criterion 4 — `focused-test`: `library_roots_test.rs`
  `mark_unavailable_is_not_refused_for_a_referenced_default`. Passes on main (records the
  decided policy); red if the guard is added to `transition_state`. Green:
  `cargo test -p voom-store --lib mark_unavailable_is_not_refused`.
- ADR append — `task-test-not-applicable`: prose; `just check-adr-index` covers the index.
- Regression: `cargo test -p voom-store -p voom-control-plane -p voom-cli`, then `just ci`.
