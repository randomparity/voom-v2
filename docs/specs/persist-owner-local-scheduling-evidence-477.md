# Spec: Persist owner-local scheduling evidence (#477)

Governing ADR: `docs/adr/0071-persist-owner-local-scheduling-evidence.md`.
Predecessors: ADR 0069 (declaration vocabulary), ADR 0070 (resolution + gate),
ADR 0068 (serialized migrations, untrusted SQLite), ADR 0013 (payload contract).

## 1. Durable evidence vocabulary (`voom-core::taxonomy::owner_access_evidence`)

All types `Debug, Clone, PartialEq, Eq, Serialize` with manual `Deserialize`
impls that route through the same validation; `#[serde(deny_unknown_fields)]`
on every named-field struct.

```rust
pub struct OwnerAccessEvidence {
    pub declaration: ArtifactAccessDeclaration,   // canonical, validated on decode
    pub root_epochs: Vec<RootEpoch>,              // strictly ascending, unique ids
}
pub struct RootEpoch {
    pub storage_root_id: StorageRootId,           // non-zero u64 newtype
    pub root_epoch: u64,                          // negative persisted epoch = corrupt
}

#[serde(tag = "evidence", rename_all = "snake_case")]
pub enum DecisionAccessEvidence {
    Owner(OwnerAccessEvidence),
    Rejected(AccessRejectionEvidence),
}

pub struct AccessRejectionEvidence {
    pub references: Vec<AccessReferenceRejection>, // the failing reference(s)
}
pub struct AccessReferenceRejection {
    pub target: ArtifactAccessTarget,              // as declared
    pub reason: AccessReferenceReason,
}
#[serde(rename_all = "snake_case")]
pub enum AccessReferenceReason {
    StorageRootNotFound, FileLocationNotFound, LocationRootInvalid,
    InvalidRootState, InvalidRootEpoch, InvalidLocationState,
    MixedOwner, NoActiveIncarnation,
}
```

Validation (constructor = deserializer, one accepted encoding):

- `OwnerAccessEvidence`: `root_epochs` non-empty, strictly ascending by
  `storage_root_id`; the epoch id set **equals** the distinct root set the
  declaration references across all four target variants (`storage_root_id`,
  `file_location.storage_root_id`, `existing_artifact.storage_root_id`,
  `planned_artifact.target_storage_root_id`).
- `AccessRejectionEvidence`: non-empty; targets strictly ascending. Resolution
  short-circuits at the first domain failure, so producers emit exactly the
  failing reference; reasons for references resolution never reached are never
  invented.
- No type resolves, authorizes, or interprets a root-addressed entry as
  locality (issue #477 comment constraint).

## 2. Migration 0037 (`migrations/0037_owner_local_scheduling_evidence.sql`, MIGRATOR version 2)

1. Preflight guard (temp table, `CHECK (ok = 1)`): fails the migration iff
   `artifact_access_plans` is non-empty, before any DDL. Runs inside ADR 0068's
   single outer transaction, so rejection leaves the schema untouched.
2. `artifact_access_plans` rebuilt (create → copy → drop → rename → index):

```sql
CREATE TABLE artifact_access_plans_0037_next (
    id                INTEGER PRIMARY KEY,
    lease_id          INTEGER NOT NULL REFERENCES leases(id) ON DELETE RESTRICT,
    ticket_id         INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    worker_id         INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    node_id           INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    owner_node_id     INTEGER          REFERENCES nodes(id) ON DELETE RESTRICT,
    access_evidence   TEXT             CHECK (access_evidence IS NULL OR json_valid(access_evidence)),
    status            TEXT NOT NULL CHECK (status IN ('selected','consumed','rejected','failed')),
    reason            TEXT,
    evidence          TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(evidence)),
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    UNIQUE (lease_id),
    CHECK (
        (owner_node_id IS NULL AND access_evidence IS NULL)
        OR (owner_node_id = node_id AND access_evidence IS NOT NULL)
    )
) STRICT;
-- indexes: by_ticket (ticket_id,id), by_worker (worker_id,id),
--          by_node (node_id,id), by_owner_status (owner_node_id,status,id)
```

The nullable pair is **proven absence**, not a second format: exactly one of
"full owner-local proof" or "no declared byte work" holds.

3. `scheduler_decisions`: `ALTER TABLE ... ADD COLUMN access_evidence TEXT
   CHECK (access_evidence IS NULL OR json_valid(access_evidence))`. Nothing
   else changes: columns, indexes, `AUTOINCREMENT` sequence, and all twelve
   reason codes are preserved by construction and proven by test.

## 3. Repository behavior

### `artifact_access_plans` (`voom-store`)

- `NewArtifactAccessPlan` loses `input_handles`, `output_handles`,
  `selected_access_mode`; gains `owner_node_id: Option<NodeId>` and
  `access_evidence: Option<OwnerAccessEvidence>` (Some together or None
  together — validated at write).
- Writes serialize the evidence through the typed vocabulary; reads
  deserialize with checked conversions; any decode failure is
  `VoomError::Database` naming `artifact_access_plans.access_evidence`.
- `list_by_mode_and_status` and the by-mode SELECT are deleted (no production
  callers).
- `validate_plan_coherence_in_tx` keeps the lease↔ticket↔worker agreement
  check and additionally requires the Some/None agreement above and
  `owner_node_id == node_id` when present.

### `scheduler_decisions` (`voom-store`)

- `NewSchedulerDecision` / `SchedulerDecision` gain
  `access_evidence: Option<DecisionAccessEvidence>`.
- Write validation: `owner` evidence only with `outcome = selected`;
  `rejected` evidence only with `reason_code = unsupported_artifact_access`
  and `outcome = no_eligible_candidate`; absent otherwise.
- Read validation: a row whose stored JSON violates the typed contract is
  `VoomError::Database` (corruption), never a domain result. Existing rows
  with `access_evidence IS NULL` read exactly as before.

## 4. Acquire integration (`voom-control-plane`) and wire cutover

### Store layer prerequisite

`ResolvedLocation` gains `root_epoch: i64`; `resolve_file_location` selects
and validates it from the joined `library_roots` row in the same query
(negative → `InvalidRootEpoch`). One resolution therefore yields the full
epoch set for every declared root — no second pass.

### Gate refactor

`declaration_is_owner_local_in_tx` becomes
`resolve_ticket_owner_locality_in_tx` returning
`TicketLocality::{OwnerLocal(AccessResolution), NotOwnerLocal { declaration,
error }, NoDeclaration}`. `AccessResolutionError::DatabaseError` propagates as
`VoomError::Database` — never recorded as a rejection.

### Rejected decisions

Domain failures of gated tickets write one rejected decision per ticket:
kind `no_candidate`, outcome `no_eligible_candidate`, reason
`unsupported_artifact_access`, `ticket_id` set, `candidate_count = 1`,
`access_evidence = DecisionAccessEvidence::rejected(...)` carrying the failing
reference; suppressed by the new key.

### Selected decisions

The selected path reuses the resolution captured by the gate (no second
resolution): plan row carries `owner_node_id` + `access_evidence` (or the
absent pair for `NoDeclaration`); the selected decision row carries
`DecisionAccessEvidence::owner` or NULL. Capacity rejections set the decision
row's `ticket_id` to the capacity-checked candidate's ticket.

### Wire plan cutover

`RemoteArtifactAccessPlan` becomes `{ id: u64, owner_node_id: Option<u64>,
access_evidence: Option<OwnerAccessEvidence> }` (deny_unknown_fields),
mirrored in `voom-node-agent::client`, `voom-fakes::remote_runner` (which also
gains deny_unknown_fields), `voom-fake-support` result validation, and the
conformance echo worker. Old fields (`selected_access_mode`,
`input_handles`, `output_handles`) are rejected on decode (regression tests).
Fake-support drops its mode-vs-advertised-capability cross-check — the mode
vocabulary exits the dispatch path; the capability vocabulary itself is
unchanged.

### Terminal consumption follows the same facts

`validated_artifact_complete_evidence` now compares worker echoes against the
persisted proof instead of legacy fields:

- echo must carry `validated: true`;
- plan with evidence: echoed `owner_node_id` must equal the plan's and echoed
  `access_evidence` must equal the plan's value;
- plan proving absence: neither field may be present;
- any mismatch conflicts (anti-forgery semantics preserved).

## 5. Suppression identity

- Gate rejections:
  `remote_acquire:node:{n}:worker:{w}:ticket:{t}:reason:unsupported_artifact_access:refs:{fp}:bucket:{b}`
  with `fp` = compact canonical JSON of the ticket's **declaration alone**
  (the failed resolution produced no trustworthy epochs).
- Owner-evidence contexts (selected rows are never suppressed; reserved for
  future reuse): declaration + epochs would be the fingerprint input.
- Capacity rejections: existing key plus a `ticket:{t}` segment matching the
  row's `ticket_id`.
- Idle / aggregate no-candidate: unchanged.

## 6. Inspection

- `voom scheduler decisions show/list` renders the evidence block (owner:
  declaration targets + epochs; rejected: failing reference + reason). Absent
  evidence renders as none. Locator-free: ids and codes only. Insta snapshots
  updated.
- `ControlPlane::scheduler_decision(s)` carry the typed evidence through.

## 7. Contract registration

- `docs/payload-contract-inventory.md`: enforced typed roots for
  `artifact_access_plans.access_evidence` (`OwnerAccessEvidence`) and
  `scheduler_decisions.access_evidence` (`DecisionAccessEvidence`).
- `scripts/payload-contract-scope.txt`:
  `crates/voom-core/src/taxonomy/owner_access_evidence.rs`.
- ADR 0071 + `docs/adr/README.md` row (check-adr-index couples them).

## 8. Test matrix (sibling `*_test.rs`, real SQLite, `ManualClock` for domain time)

1. Evidence type: canonical round trip; each validation rejection; unknown
   field rejected; unknown evidence variant rejected; negative epoch rejected;
   epoch-set/declaration mismatch rejected.
2. Migration: fresh init applies 0037; `expected_migrations() == 2`; guard
   rejects a legacy-shaped `artifact_access_plans` row (seeded via the ADR 0061
   check-bypass helper) before mutation — schema columns unchanged after failed
   init; `scheduler_decisions` preservation test — pre-seeded legacy row keeps
   every column value, all indexes exist, `AUTOINCREMENT` continues past the
   preserved max id, all twelve reason codes still accepted.
3. Plans repo: selected round trip with evidence; selected round trip proving
   absence (NULL pair); corruption tests (non-JSON evidence, unknown field,
   epoch mismatch, half-present pair → `VoomError::Database`); coherence check
   incl. owner mismatch rejection.
4. Decisions repo: owner evidence only on selected; rejected evidence only on
   unsupported_artifact_access; NULL reads unchanged; corruption → database
   error.
5. Acquire: gated ticket produces durable rejected decision with the failing
   reference's reason and ticket+locality suppression key; distinct ticket →
   distinct row; changed declaration → distinct row; `DatabaseError`
   propagates, no decision row; selected byte-work path persists evidence on
   both rows; selected declaration-free path persists the absent pair; wire
   plan carries new shape; old wire fields rejected on every mirror.
6. Complete path: matched echo completes; mismatched `owner_node_id` /
   `access_evidence` echoes conflict; absent-pair plan rejects an echo that
   claims evidence.
7. Locator-free: inspection output for selected + rejected decisions contains
   no path/locator strings (assert absence of `/`, `handle:`).
