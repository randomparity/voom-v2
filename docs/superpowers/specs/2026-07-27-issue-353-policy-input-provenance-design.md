# Policy Input Snapshot Provenance Design

**Issue:** #353
**Status:** Draft
**Base:** `main` at `4fc6bf74e00dba51b253d084c7cb7654cf3df47c`

## Goal

Make the generic policy-input writer accept a linked media snapshot only when
the link proves the member's exact file-version provenance. Reject an invalid
member before any input-set row is inserted and preserve the existing
scan-import and historical-input contracts.

## Review charter

- **Outcome:** every member passed to
  `ControlPlane::create_policy_input_set` with an
  `existing_media_snapshot_id` is checked against the exact
  `TargetRef::FileVersion` in the same write transaction.
- **Permitted surface:** the control-plane policy-input case handler and
  sibling tests; one bounded set-based `IdentityRepo` method and its store
  tests; and this issue's design and plan.
- **Direct dependencies:** a new
  `IdentityRepo::get_media_snapshot_file_versions_in_tx` bulk read,
  `SqlitePolicyInputRepo::create_input_set_in_tx`, `begin_immediate_tx`,
  SQLite `json_each`, the Sprint 3 aggregate transaction, and ADR 0036's
  selected-version provenance rule.
- **Compatibility:** no public type, JSON, SQL, error-code, event, or policy
  grammar changes.

Explicit exclusions:

- Read-time rejection and active-snapshot projection remain governed by ADR
  0036 and #329. This change does not repair old invalid rows.
- Plan-through-dispatch lineage isolation belongs to #352. This change ends
  when the immutable input-set selection is committed.
- No schema trigger or migration is added. The polymorphic target columns
  cannot express this cross-table equality, and the control plane remains the
  owner of production write use cases.
- The repository's direct `create_input_set` helper remains a persistence
  primitive for repository tests. Production callers must use the control
  plane, as established by the Sprint 3 architecture.
- Generic writes do not adopt scan import's live-version requirement.
  Historical file-version/snapshot pairs remain valid durable selections.

An excluded concern is blocking if this change depends on it or makes it
worse.

## Current behavior

`ControlPlane::create_policy_input_set` begins a transaction and passes the
draft directly to `SqlitePolicyInputRepo`. The database foreign key proves
only that `existing_media_snapshot_id` names some snapshot. It does not prove
that the member targets `FileVersion`, or that the snapshot belongs to the
targeted version.

`create_policy_input_set_from_scan` already loads both rows inside its
transaction, rejects a missing snapshot with `NOT_FOUND`, and rejects a
file-version mismatch with `CONFLICT`. ADR 0036 applies the same relationship
when stored inputs are read for planning, but malformed generic writes should
not be admitted in the first place.

## Decision

### Bounded identity read

`IdentityRepo` adds this transaction-scoped read:

```text
get_media_snapshot_file_versions_in_tx(
    transaction,
    snapshot_ids: &[MediaSnapshotId],
) -> [(MediaSnapshotId, FileVersionId)]
```

The implementation returns an empty vector without issuing SQL when the input
is empty. Otherwise it converts the IDs to the repository's SQLite `i64`
representation, serializes that vector once, and binds it to one set-based
query:

```sql
SELECT id, file_version_id
FROM media_snapshots
WHERE id IN (SELECT value FROM json_each(?))
ORDER BY id
```

This established repository pattern uses one bind regardless of aggregate
size, so it does not hit SQLite's variable limit. `IN` makes duplicate input
IDs produce one result row. The control plane still deduplicates linked IDs
before calling the repository so the request and returned map have one entry
per identity.

The method returns every existing requested `(snapshot, version)` pair and
omits missing IDs. The control plane owns the public `NOT_FOUND` decision and
member context; the repository remains a deterministic identity read rather
than policy validation.

### Transaction boundary

Before acquiring a database lock, the generic writer:

1. runs `voom_policy::validate_input_set`, preserving the existing
   model-validation error precedence; and
2. collects all linked snapshot IDs into a `BTreeSet`.

The repository repeats model validation before insertion as defense in depth.

The generic writer then uses `begin_immediate_tx`. It is a read-then-write
transaction, so taking the SQLite write lock before the one bulk identity read
avoids a deferred lock-upgrade failure and keeps provenance validation and
insertion in one serializable writer boundary. The lock-held validation work is
one set-based query plus an in-memory pass; it no longer grows by one database
round trip per member.

Inside the transaction, the handler:

1. bulk-loads all distinct linked snapshot/version pairs;
2. indexes the returned pairs by snapshot ID;
3. inspects every linked media-snapshot member in draft order;
4. validates snapshot existence, member target, and exact version;
5. calls `create_input_set_in_tx`; and
6. commits.

No root, label, synthetic-target, or child insert starts until the complete
linked member list passes.

### Link contract

For each `MediaSnapshotInput` with
`existing_media_snapshot_id = Some(snapshot_id)`:

- load `snapshot_id`; absence returns `VoomError::NotFound` with
  `media snapshot <id> not found`;
- require `TargetRef::FileVersion`; every other target returns
  `VoomError::Conflict` with an actionable message naming the snapshot and
  member ordinal; and
- require `snapshot.file_version_id == target.id`; mismatch returns
  `VoomError::Conflict` with the existing scan-path wording,
  `media snapshot <id> does not belong to file version <id>`.

Snapshot existence is checked before target shape, so a missing linked
snapshot always has the documented `NOT_FOUND` contract. Members without a
link are unchanged.

The generic path deliberately does not require the selected file version to be
live. ADR 0036 treats an input set as a durable selection of file lineage and
permits a historical selected version while planning resolves its active tip.
The snapshot's foreign key proves that a matching version exists. The
single-file scan-import path keeps its stronger explicit existence and
retirement checks because it represents current scan state.

### Atomic failure and events

Any error drops the uncommitted transaction. Tests query every table owned by
the policy-input aggregate, not only the root list, to prove that a valid
earlier member followed by an invalid later member leaves no partial state.

Policy-input creation intentionally has no event kind under the Sprint 3
design. Tests capture durable event rows before the attempted write and prove
that provenance rejection appends none. Setup identity and snapshot events are
allowed and remain unchanged.

## Compatibility and rollback

There is no migration or persisted-shape change. Existing unlinked fixture and
manual drafts are unaffected. Exact historical links remain writable. Existing
invalid rows remain readable and continue to fail the ADR 0036 stored-planning
adapter.

Rollback is a code revert. Rows accepted by the new version were already valid
under the old reader, so rollback requires no data repair.

## Security and operations

The link is caller-controlled durable provenance. Validating it at the
control-plane trust boundary prevents a caller from attaching trustworthy
snapshot identity to facts for another target. No authentication, filesystem,
worker, network, or secret boundary changes.

Failures use existing public error codes and context. No new event or log is
introduced.

## Test strategy

- Exact valid link round-trips through the generic writer.
- The bulk identity read returns immediately for an empty slice, collapses
  duplicate IDs, orders results deterministically, reports existing pairs
  while omitting missing IDs, and accepts more IDs than SQLite's ordinary bind
  limit because it uses one JSON bind.
- A missing snapshot returns `NOT_FOUND`.
- A linked non-file target returns `CONFLICT`.
- A later mismatched member returns `CONFLICT` after an earlier valid member
  was inspected.
- Every failure asserts all eight policy-input aggregate tables remain empty
  and the durable event count is unchanged.
- A matching snapshot on a retired historical version remains writable.
- Existing scan-import missing, mismatch, and retired tests remain green.
- Mutate the version comparison locally, confirm the mismatch test fails, then
  restore it.
- Run the focused store identity and control-plane tests, formatting, linting,
  and `just ci`.

## Campaign sequencing

This branch is based on main `4fc6bf7`. Campaign issues #344, #346, and #358
must merge first. Before #353 can merge, rebase onto that resulting main,
rerun the focused policy-input suite and `just ci`, then require green PR CI.
