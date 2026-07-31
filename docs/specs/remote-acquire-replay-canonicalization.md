# Remote acquire replay canonicalization

Status: draft
Date: 2026-07-31
Base: `desloppify/code-health` at `2dc31959078a`

## Context

Remote acquire outcomes gained `scheduler_decision_id` after
`remote_idempotency_keys` was introduced. Completed acquire rows written before
that field existed remain durable, so the current replay decoder rewrites their
JSON in memory on every read by inserting a zero decision id. That compatibility
branch sits in the current hot path and makes the application accept two durable
response shapes indefinitely.

The database schema is versioned, and `ControlPlane::open` does not run against
an older schema. This gives the migration boundary sole responsibility for
bringing persisted replay JSON to the shape required by the current binary.

No ADR is added. This is a one-time durable-data normalization within the
existing migration and idempotency contracts, not a new architecture decision.

## Decision

Add migration 0033 to canonicalize completed remote acquire success responses
whose known outcome is `idle`, `no_candidate`, or `leased` and whose
`scheduler_decision_id` field is absent. The migration inserts
`scheduler_decision_id: 0`, preserving the existing public replay result for
pre-scheduler rows that cannot be linked to a historical decision.

The SQL update is narrowly guarded by all of the following:

- `route_key = 'POST /v1/execution/lease/acquire'`;
- `status = 'completed'`;
- replay status is `ok`;
- `data` is a JSON object;
- outcome is `idle`, `no_candidate`, or `leased`; and
- `data.scheduler_decision_id` is absent.

Rows already carrying a decision id remain byte-for-byte unchanged. Error
replays, other routes, unknown outcome variants, malformed shapes, and
in-progress rows remain unchanged. Unknown or malformed completed acquire
responses continue through the existing poison-repoint behavior when read.

After migration, `decode_acquire_replay` delegates directly to the ordinary
typed `decode_replay` helper. Delete the in-memory legacy JSON transformer and
the control-plane tests that preserve the obsolete shape. There is no runtime
fallback, deprecated format, or dual decoder.

## Invariants and boundaries

- A completed idempotency row is never deleted: the mutation it represents may
  already have executed, so allowing the key to reserve again would be unsafe.
- Migration 0033 changes only known legacy acquire success JSON and does not
  create scheduler decision rows without historical evidence.
- The zero decision id is a historical sentinel only. All newly completed
  acquire outcomes continue storing the real positive scheduler decision id.
- `connect()` remains read-only and never migrates. Operators must apply the
  normal `init()` upgrade path before the new binary opens the database.
- The remote acquire wire shape, error taxonomy, lease state, scheduler log,
  transaction ownership, and replay poison handling do not change.

## Failure behavior

The data update is atomic under the existing sqlx migrator transaction. If the
JSON update fails, its data changes roll back, but sqlx may record migration
0033 with `success = 0`. The schema probe then reports `Dirty`, and the existing
repair-or-restore guidance applies before migration can be retried. A completed
acquire row outside the known legacy shape is not guessed into validity; the
current replay decoder rejects it and the existing recovery boundary repoints
it to a terminal error.

## Compatibility and rollback

This is a one-way pre-release schema upgrade. Migration 0033 replaces the old
durable response shape rather than teaching the application to support it.
There is no down migration. Rolling back the binary after applying 0033 is safe
for this column because the older decoder already accepts the added field, but
schema-version checks still require the repository's normal coordinated
rollback procedure.

## Security and observability

The migration does not touch tokens, request hashes, worker ownership, lease
authorization, or error messages. Existing warning logs for genuinely
unreadable replay rows remain. Removing the compatibility branch makes accepted
durable input explicit at the schema boundary.

## Test strategy and acceptance criteria

- A migration integration test starts at schema 0032, seeds completed acquire
  success rows for all three legacy outcomes, and proves migration 0033 adds a
  zero decision id to each.
- The same test proves a current response with a real decision id is unchanged.
- The same test proves error replays, another route, an unknown acquire outcome,
  non-object `data`, and present decision ids containing JSON null or the wrong
  type are byte-for-byte unchanged. This distinguishes field absence from JSON
  null and prevents a broad `json_extract(...) IS NULL` predicate.
- Existing new-format acquire replay coverage continues to pass. A table-driven
  poison-repoint test proves missing, null, wrong-typed, non-object, and unknown
  outcome responses are rejected by the strict runtime decoder and durably
  repointed to terminal errors.
- The two control-plane tests that directly seed and accept missing-decision-id
  outcomes are removed; upgrade behavior belongs to the migration test.
- `acquire_replay_with_legacy_decision_id` has no declaration or reference, and
  `decode_acquire_replay` contains only typed decode delegation.
- Migration inventory and expected schema-count assertions advance from 32 to
  33.
- `just ci` passes with zero failures and zero warnings. Existing separately
  gated environment tests remain unchanged, and this change adds no ignored
  test.

## Dependencies and exclusions

In scope: migration 0033, the hand-built migrator inventory, migration tests,
schema-count assertions, the acquire replay decoder, and obsolete legacy-shape
tests.

Excluded: synthesizing historical scheduler decisions, deleting idempotency
rows, changing public response types, changing poison-repoint behavior, or
generalizing durable JSON migrations beyond this known response transition.
