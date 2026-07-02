# Spec: Wire the commit safety gate into the production commit path (#270)

Status: Draft
Issue: [#270](https://github.com/randomparity/voom-v2/issues/270) (part of epic #269, Workstream A)
Related ADR: `docs/adr/0017-commit-gate-lineage-commit-check.md`

## Problem

The commit safety gate (`crates/voom-store/src/repo/media/commit_safety_gate/`)
is fully implemented and unit-tested but has **zero production callers**. The
real artifact commit path (`crates/voom-control-plane/src/artifact/commit/`)
never consults it. End-to-end, a blocking use lease does **not** block a commit
today: the spec's central safety promise (`Runtime Use Lease Model` →
`Commit Safety Gate`, `docs/specs/voom-control-plane-design.md` §1185–1248) is
dead code on this path.

## Goal

A blocking use lease that is live at commit time on the asset being committed
actually fails the commit, before any irreversible filesystem mutation, and the
leases the gate considered are recorded in the commit's event record for audit.

## Background: the commit path shape

`ControlPlane::commit_artifact` (entry `commit_artifact_with_hooks` in
`artifact/commit/mod.rs`) runs three stages:

1. **prepare** (`prepare.rs`, one DB transaction): reads source/staging facts,
   creates the durable `pending` commit record, emits `ArtifactCommitStarted`.
   Commits the tx.
2. **promote** (`promote.rs`, no DB tx): the **irreversible filesystem
   mutation** — hard-links the verified temp copy to the new target path
   (`install_temp_no_replace`, add-only; fails if the target already exists).
3. **finalize** (`finalize.rs`, one DB transaction): creates the result
   `FileVersion`/`FileLocation`, retires the staging location, marks the record
   `committed`, emits `ArtifactCommitCompleted`. Commits the tx.

This commit is a **lineage / additive** operation: it produces a new
`FileVersion` on the existing source `FileAsset` from the prior
`source_file_version_id` (design §793–795 — transcode/remux/restore). It is
**not** one of the gate's destructive `CommitTarget` shapes (delete / replace /
move a `FileLocation`).

Remux, audio-transcode, and video-transcode commits all route through this same
`commit_artifact` entry, so gating it covers all of them.

## Design

### What scope a blocking lease must cover

Per design §1191–1202 the affected-scope closure for a commit is the full
closure of identifiers it touches. For this lineage commit the closure is
anchored on the source version being committed:

- `file_assets`   = { source `FileAsset` }
- `file_versions` = { `source_file_version_id` }
- `file_locations`= every live `FileLocation` of `source_file_version_id`
- `bundles`       = every `AssetBundle` the source `FileAsset` belongs to

A live **blocking** lease overlapping any member fails the commit. Advisory
leases never fail it (design §1243).

### Freshness (expiry) semantics

Design §1235–1241: a lease in any terminal state (non-null `release_reason`)
does not block; a TTL-bound lease whose `expires_at` has passed without a
renewing heartbeat is treated as expired and does not block, **even before
cleanup has run**. Manual locks (not TTL-bound) block until terminal.

The check therefore excludes a lease when any of:

- `release_reason IS NOT NULL` (terminal), or
- `ttl_bound = 1 AND expires_at < now` (TTL expired against the control-plane
  clock).

> Note: the existing destructive-gate query
> (`scope.rs::blocking_lease_rows_in_tx`) filters only `release_reason IS NULL`
> and does not apply the TTL-expiry rule. That is a pre-existing conformance gap
> on the destructive gate, out of scope here; flagged as a follow-up. This spec
> adds a **new, clock-aware** check for the lineage-commit path and does not
> change the destructive gate.

### Where the check runs

The check runs **inside the prepare transaction**, after source/staging facts
are read and before the durable `pending` record is written — i.e. before the
irreversible promote step. This is the only host transaction that precedes the
filesystem mutation, satisfying "inside the host-side transaction … immediately
before any irreversible filesystem mutation" (design §1187–1190). See the ADR
for why prepare (not finalize) and why not the full gate intent lifecycle.

On a blocking lease the commit fails as a **pre-mutation** error
(`VoomError::BlockedByUseLease`, code `BLOCKED_BY_USE_LEASE`), reusing the
existing `ArtifactCommitFailedPreMutation` event and failure machinery. No
durable `pending` record is written, no filesystem mutation happens, no orphan
target file is left. The blocking lease id and its scope are named in that
event's `message` (free text); a blocked commit produces no completed-commit
event, so the structured `gate_evaluated_lease_ids` audit field below is a
success-path record only — see "Recording in the commit event".

**Fail-closed.** `check_lineage_commit_leases_in_tx` runs on the host prepare
transaction and can return `VoomError` (DB failure, bundle-membership query,
location listing). Any such error aborts the commit as a pre-mutation failure —
the commit never proceeds on an unresolved gate check (design §1226–1234). It is
not swallowed or reinterpreted as "no blocking lease".

**Point-in-time, not serialized.** The check reads leases once at prepare, in a
deferred host transaction. Lease acquisition (`use_leases.rs::acquire_in_tx`) is
serialized only against `commit_intents` rows; the artifact commit path writes
`artifact_commit_records`, not `commit_intents`, so acquisition is **not**
blocked by an in-flight artifact commit. A blocking lease acquired after the
prepare check therefore does not fail the in-flight commit. This issue blocks a
lease that is **live at commit (prepare) time**; closing the prepare→promote
window (recompute-under-isolation) is out of scope (see below).

### New store-layer primitive

Add one public function to the `commit_safety_gate` module (voom-store owns
`asset_use_leases`; the control plane must not embed lease SQL — architecture
Rule 1 / crate layering):

```rust
pub struct LineageCommitLeaseCheck {
    pub closure: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,      // live, non-expired, overlapping (audit)
    pub blocking: Option<(UseLeaseId, LeaseScope)>, // first blocking overlap, if any
}

pub async fn check_lineage_commit_leases_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    file_asset_id: FileAssetId,
    file_version_id: FileVersionId,
    now: OffsetDateTime,
) -> Result<LineageCommitLeaseCheck, VoomError>;
```

It runs entirely on the caller's transaction (host prepare tx), reuses
`IdentityRepo::list_live_file_locations_by_version_in_tx` for the location
closure and an `asset_bundle_members` query for bundles, and runs a single
clock-aware overlap query for the leases. It does **not** open a gate
`BEGIN IMMEDIATE` transaction (that would nest against the host tx).
`evaluated_lease_ids` is every live, non-expired lease (blocking or advisory)
overlapping the closure; `blocking` is the lowest-id blocking overlap.

### Recording in the commit event

`ArtifactCommitCompletedPayload` gains one additive field:

```rust
#[serde(default)]
pub gate_evaluated_lease_ids: Vec<u64>,
```

Populated at finalize from the leases the gate considered at prepare time
(threaded through `PreparedCommit`). On a successful commit `blocking` was
`None`, so this records the advisory/overlapping leases that were evaluated and
did not block — the audit trail (design §1245–1248). The field is
`#[serde(default)]` per the durable-payload evolution contract (ADR 0013); the
struct keeps `deny_unknown_fields`.

The audio **sidecar** extract commit (`audio/commit.rs::finalize_sidecar_commit`)
shares this payload; it is a separate commit path not in this issue's scope. It
populates the new field as empty and does not yet run the gate — documented
limitation, follow-up filed.

## Acceptance criteria

1. A live blocking use lease on the source asset/version/location fails
   `commit_artifact` end-to-end with `ErrorCode::BlockedByUseLease`, before the
   target file is written, and the `pending` record is not left behind. The
   blocking lease id is named in the `ArtifactCommitFailedPreMutation` event
   message.
2. A **terminal** lease (released) does **not** block the commit.
3. A **TTL-expired** lease (`expires_at` in the past, not yet swept) does
   **not** block the commit.
4. An **advisory** lease does **not** block the commit, and its id appears in
   `gate_evaluated_lease_ids` on the completed event.
5. On a normal commit with no leases, `gate_evaluated_lease_ids` is empty and
   behavior is unchanged.
6. A manual lock (`ttl_bound = 0`, no `expires_at`) **does** block the commit
   (not excluded by the TTL-expiry rule).
7. Guardrails green: `just ci` (fmt, clippy `-D warnings`, test-layout,
   test, doc, deny, audit, payload-deny-unknown).

## Out of scope / follow-ups

- The destructive-gate TTL-expiry gap in `blocking_lease_rows_in_tx`.
- Wiring the gate into the audio sidecar extract commit path.
- Re-evaluating the gate on the `recover_commit` re-drive path.
- The spec's advanced recompute-under-isolation semantics (serialize against
  lease acquisition, recompute closure immediately before mutation, evidence
  revalidation) — those belong with the destructive gate, not this additive
  path.
