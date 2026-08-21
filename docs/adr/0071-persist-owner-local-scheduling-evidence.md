# ADR 0071: Persist owner-local scheduling evidence

## Status

Accepted

## Context

Issue #477 closes the persistence gap in the owner-local artifact-access chain.
ADR 0069 gave byte-work tickets a canonical, locator-free access declaration;
ADR 0070 resolves that declaration against storage state and gates remote
acquire on a single common owner. Neither fact survives the transaction that
produced it:

- The durable plan record is still the **legacy synthetic/shared-mount
  representation**: `artifact_access_plans.selected_access_mode` carries the
  pre-#475 vocabulary (`shared_mount`, `control_plane_placeholder`,
  `staged_output_placeholder`), and `input_handles`/`output_handles` store
  worker-facing handle strings whose production fallback is literally
  `handle:input:synthetic`. Nothing in the row names the owner, the canonical
  references, or the epochs that justified the selection.
- Scheduler decisions record *that* a selection or rejection happened but not
  *why* in owner-local terms. The #476 owner-local gate
  (`declaration_is_owner_local_in_tx`) silently drops non-owner and mixed-owner
  candidates: the rejection is in-memory only, exactly the observability gap
  the canonical-access spec assigns to #477.
- Suppression identity (`remote_acquire:node:N:worker:W:reason:R:ops:F:bucket:B`)
  names no ticket and no locality evidence, so two distinct attempts — different
  tickets, or the same ticket after its root's epoch changed — collapse into one
  suppressed decision and reuse a stale explanation.
- The remote-complete path's anti-forgery check
  (`validated_artifact_complete_evidence`) compares worker echoes against
  exactly these legacy fields, so replacing the representation necessarily
  redefines that comparison.

The schema was squashed to `migrations/0001_schema.sql` (issue #505), so this
slice's "migration 0037" is the second entry in the embedded `MIGRATOR` —
logical number 0037, physical version 2.

### Binding constraints (issue #477 and its comments)

1. Replace the legacy representation **without retaining dual formats**.
2. Preflight guards reject incompatible legacy rows **before** schema mutation;
   migrated `scheduler_decisions` rows preserve every existing column, index,
   sequence, and supported reason.
3. Selected decisions persist canonical ticket, worker, owner, access-reference,
   and epoch evidence; rejected decisions persist stable per-reference reasons
   without paths or provider locators.
4. Suppression identity includes the ticket and canonical locality evidence.
5. Every persisted numeric, enum, JSON, and cross-row relationship is checked on
   read; corruption is a database error, never an eligibility result.
6. A root-addressed entry proves only "this ticket touches something in this
   root" (issue comment; ADR 0069 Consequences). Persisted evidence records the
   declaration as claimed — it never derives read locality, co-scheduling, or
   serialization scope from a root-addressed entry.

## Decision

### One persisted evidence type, owned by `voom-core`

`voom-core` gains `owner_access_evidence.rs` with the durable evidence
vocabulary, following the ADR 0069 precedent (shared scheduling/durable
vocabulary lives in core):

- `OwnerAccessEvidence { declaration: ArtifactAccessDeclaration,
  root_epochs: Vec<RootEpoch> }` — the canonical declaration exactly as
  validated at the gate, plus the resolved epoch of every referenced root.
  `#[serde(deny_unknown_fields)]`; `root_epochs` is non-empty, strictly
  ascending with unique `storage_root_id`s, and its id set must equal the set
  of roots the declaration references (all four target variants), so a row
  cannot claim epochs for roots it does not name or omit epochs for roots it
  does. `RootEpoch.root_epoch` is `u64`: a negative persisted epoch fails
  deserialization, matching ADR 0070's corrupt-epoch rule (epoch zero is
  valid).
- `AccessRejectionEvidence { references: Vec<AccessReferenceRejection> }` —
  the stable reason resolution actually produced, attached to the reference
  that failed. Resolution short-circuits at the first domain failure, so the
  evidence records **exactly the failing reference** — one entry pairing that
  declared `ArtifactAccessTarget` with a closed `AccessReferenceReason` code
  (`storage_root_not_found`, `file_location_not_found`,
  `location_root_invalid`, `invalid_root_state`, `invalid_root_epoch`,
  `invalid_location_state`, `mixed_owner`, `no_active_incarnation`). Reasons
  for references resolution never reached are never invented; a cross-reference
  verdict (`mixed_owner`) attaches to the reference whose fold detected it.
  Targets only — never a path, mount name, or host string.
- `DecisionAccessEvidence` — internally tagged (`tag = "evidence"`) enum with
  newtype variants `owner(OwnerAccessEvidence)` and
  `rejected(AccessRejectionEvidence)` over `deny_unknown_fields` content
  structs, per ADR 0013; the tag discriminator rejects unknown variant names.

The types expose no resolution or authorization behavior; they are the durable
projection of what ADR 0070 already proved or rejected.

### Absence of declared byte work is one explicit shape

The gate admits tickets with no `declared_artifact_access` (non-byte-touching
operations). For those there is no owner proof, no reference, and no epoch —
and this slice does not fabricate any. The plan row encodes proven absence as
an explicit, single alternative shape: `owner_node_id IS NULL AND
access_evidence IS NULL`. Exactly one of "full owner-local proof" or "no
declared byte work" holds; a row carrying one without the other is corrupt by
construction (table CHECK) and by repository validation. This is not a second
evidence format — nothing about the ticket's access is claimed in the absent
arm.

### Migration 0037 (physical version 2): one representation, guarded

`migrations/0037_owner_local_scheduling_evidence.sql`, appended to `MIGRATOR`
as `Migration::new(2, ...)`:

1. **Preflight guard before any DDL.** A temp guard table with a
   `CHECK (ok = 1)` constraint is fed `1` only when `artifact_access_plans` is
   empty. Any existing row is a legacy-representation row whose mode/handle
   fields have no owner-local translation, so the INSERT fails the CHECK,
   the migration aborts, and ADR 0068's single outer transaction rolls back
   before any schema mutation. The guard table's name carries the diagnosis.
   This matches the issue-#505 precedent: pre-release databases are disposable,
   and the remedy is to remove legacy rows (or recreate the database) rather
   than silently translate or dual-format them.
2. **`artifact_access_plans` rebuilt in its final shape.** `selected_access_mode`,
   `input_handles`, and `output_handles` are gone. New columns:
   `owner_node_id INTEGER REFERENCES nodes(id)` and
   `access_evidence TEXT CHECK(json_valid(access_evidence))` — both nullable,
   bound together by the table CHECK `(owner_node_id IS NULL AND
   access_evidence IS NULL) OR (owner_node_id = node_id AND access_evidence
   IS NOT NULL)` holding `OwnerAccessEvidence` when present. The status
   lifecycle, `reason`, worker-validated `evidence` passthrough column,
   timestamps, `UNIQUE (lease_id)`, and the by-ticket/by-worker/by-node indexes
   are preserved. The `by_mode_status` index is replaced by `by_owner_status`
   on `(owner_node_id, status, id)` — the mode it served no longer exists.
   The rebuild is create-new → copy (zero rows, guaranteed by the guard, but
   the column mapping stays explicit) → drop → rename → re-index. No table
   holds an incoming foreign key on `artifact_access_plans`, so the rebuild is
   legal without the forbidden `PRAGMA foreign_keys = OFF` dance.
3. **`scheduler_decisions` extended additively.** One new column:
   `access_evidence TEXT CHECK (access_evidence IS NULL OR json_valid(access_evidence))`
   holding `DecisionAccessEvidence` when present. No existing column, index,
   the `AUTOINCREMENT` sequence, or any of the twelve supported reason codes is
   touched; existing rows read back with `access_evidence = NULL`, and the
   repository treats that exactly as today's code treats absent evidence.
   Cross-column shape rules are enforced by repository write validation and
   checked reads, the same mechanism the table already uses for its
   CHECK-inexpressible invariants — `ALTER TABLE ADD COLUMN` cannot add a
   table-level CHECK.

### Selected decisions persist the proof

At acquire, the selected ticket's declaration has already passed the owner-local
gate inside the same transaction. The gate is refactored from `bool` to return
what it proved (`OwnerLocal(AccessResolution)` / `NotOwnerLocal { declaration,
error }` / `NoDeclaration`), so the selected path persists evidence **without a
second resolution pass** — one resolution, one point in time, per ADR 0069's
collapse of double resolution.

- A selected byte-work ticket yields `owner_node_id` and `access_evidence`
  built from the resolution (declaration + per-root epochs).
- A selected declaration-free ticket yields the explicit absent shape above.
- The selected `scheduler_decisions` row carries
  `DecisionAccessEvidence::owner` in `access_evidence`, or NULL when the gate
  returned `NoDeclaration`.

Building epochs needs the root epoch behind *every* declared root, including
roots referenced through `file_location` and `existing_artifact` entries that
resolution reaches via `resolve_file_location`. The store layer therefore
extends `ResolvedLocation` with the joined root's `root_epoch` (validated
non-negative inside the same query path), so one resolution still produces the
complete epoch set — no second pass, no TOCTOU window.

#### Terminal consumption follows the same facts

`validated_artifact_complete_evidence` compared worker echoes against the
legacy mode and handle strings. With those gone, the anti-forgery comparison
is redefined onto the persisted proof: the echoed `artifact_access` block must
carry `validated: true`, and — when the plan carries evidence — the same
`owner_node_id` and an equal `access_evidence` value; when the plan proves
absence, neither field may be present. A mismatched echo still conflicts; only
the compared facts change. `fail.rs` embeds the wire plan unchanged in shape.

The dispatch wire plan (`RemoteArtifactAccessPlan`, mirrored by the node agent,
the fakes runner, fake-support's result validator, and consumed by the
conformance echo worker) is replaced by the same clean cutover:
`{ id, owner_node_id, access_evidence }` with the nullable pair semantics
above. Every mirror gains `#[serde(deny_unknown_fields)]` where it deserializes
(the fakes runner struct lacks it today), and regression tests assert the old
fields are rejected on decode. Fake-support's validator drops its
mode-vs-advertised-capability cross-check: the mode vocabulary exits the
dispatch path entirely (the capability vocabulary itself remains untouched).

Worker capability advertisement (`artifact_access` config) and scorer
eligibility semantics are a different surface and remain untouched.

### Rejected decisions persist the reason resolution produced

The owner-local gate stops being silent. When a ready ticket fails resolution
with a **domain** error, the acquire transaction writes a rejected decision:
`decision_kind = no_candidate`, `outcome = no_eligible_candidate`,
`reason_code = unsupported_artifact_access` (already in the supported
vocabulary), `ticket_id` set, `candidate_count = 1`, and
`access_evidence = DecisionAccessEvidence::rejected(...)` attaching the failing
reference's stable reason. The SQL CHECK that reserves `suppression_key` for
idle/no-candidate outcomes is satisfied by this shape, so no table rebuild is
needed.

A `DatabaseError` from resolution is **not** a rejection: it propagates as a
database error and fails the acquire, per criterion 5 — corruption is never an
eligibility result.

### Suppression identity includes ticket and locality

`remote_acquire_suppression_key` gains a ticket segment and a locality
fingerprint for every decision that names exactly one ticket:

- gate rejections: `...:ticket:{id}:reason:unsupported_artifact_access:refs:{fingerprint}:bucket:{b}`
  where the fingerprint is the compact canonical JSON of the ticket's
  **declaration alone** — the failed resolution produced no trustworthy epochs,
  so only the locality claim is hashed. A different ticket or a changed
  declaration produces a different key, so a stale decision or explanation can
  never be reused for a distinct attempt.
- capacity rejections: the key names the capacity-checked candidate's ticket,
  and the decision row's `ticket_id` is set to that same ticket so the key and
  the row agree.
- idle and aggregate no-candidate decisions name no ticket and keep the
  existing key shape; their staleness is bounded by the time bucket.

### Checked reads

Every read of the new columns goes through the typed vocabulary: JSON is
deserialized into `deny_unknown_fields` types (unknown fields, unknown
evidence variants, non-canonical epoch ordering, epoch-set mismatch, negative
epochs all fail), numeric columns go through checked conversions, and any
failure is a `VoomError::Database` naming the column — corruption is a database
error, never a missing/conflict domain result.

## Consequences

- A selected lease can be inspected and replayed deterministically from
  persisted facts alone: ticket, worker, owner (when byte work was declared),
  canonical references, epochs.
- Denied tickets become durably observable: one suppressed row per
  ticket × declaration bucket, with the failing reference's stable reason and
  no paths.
- The legacy representation has no read path left: the mode enum's
  persistence use is gone, `list_by_mode_and_status` is deleted with its
  callers migrated, and the wire plan's old fields are rejected on decode.
- `expected_migrations()` reports 2; the init/schema tests are updated to the
  two-migration reality, and a new migration-preservation test seeds a
  pre-0037-shaped `scheduler_decisions` row through the guard-rail bypass
  helper (ADR 0061) and proves every column, index, the sequence, and all
  supported reasons survive.
- Databases carrying legacy plan rows fail `init` with a named guard error
  before anything is mutated; the remedy (remove legacy rows / recreate the
  disposable database) is documented here and in the guard name.
- `docs/payload-contract-inventory.md` and
  `scripts/payload-contract-scope.txt` gain the new durable typed roots
  (`artifact_access_plans.access_evidence`,
  `scheduler_decisions.access_evidence`, and the `voom-core` evidence module),
  keeping `check-payload-deny-unknown.sh` honest.
- The root-addressed constraint is preserved by construction: evidence
  persistence copies the declaration as claimed and adds epochs; nothing in
  this slice reads a root entry as locality, and the rejection path records
  targets without interpreting them.

## Alternatives considered

- **Translate legacy plan rows during migration.** Rejected: the legacy mode
  and handle strings carry no owner or epoch proof; any translation would be
  fabrication, and retaining the old columns for old rows is precisely the dual
  format criterion 1 forbids.
- **Keep the mode on the wire, derive handles at dispatch.** Rejected: the
  dispatch plan is the synthetic/shared-mount representation the issue names;
  keeping it anywhere leaves two representations of one fact.
- **Two evidence columns on `scheduler_decisions` (owner / rejected).** Rejected
  in favor of one tagged enum column: ADR 0013's tagged-enum pattern makes the
  discriminator reject unknown shapes, and one column cannot disagree with
  itself.
- **Require evidence on every selected decision (reject `NoDeclaration`).**
  Rejected: it would change eligibility for non-byte-touching operations, which
  the frozen scope excludes.
- **Evaluate every reference independently to fill per-target reasons.**
  Rejected: it duplicates resolution logic (including cross-reference owner
  folding) for no consumer benefit, and reasons for unreached references would
  still be fabrication. Recording the failing reference honestly satisfies
  criterion 3's "stable per-reference reasons".
- **Suppress gate rejections per acquire instead of per ticket.** Rejected:
  it is exactly the stale-explanation reuse criterion 4 forbids.
