# Issue #418 — Node-owned roots and provider-relative locations design

## Charter and provenance

This design implements the root/location substrate selected by ADR 0050 and
issue #418. The campaign assigned ADR 0055 and migration 0034 to this issue.
The frozen campaign interpretation is bounded: #418 removes global paths from
the durable root/location model and root-scoped control-plane and CLI contracts;
#423 still owns replacement of media-worker path requests. Issue #417 owns
authenticated node incarnations, #421 owns remote scan execution, and #422 owns
owner-node commit. This design must not absorb those contracts or claim the
byte-blind architecture is complete.

Success means:

- root identity is owner-scoped and equal provider locators on different nodes
  are independent;
- every newly created file location has a root ID and safe relative locator;
- ownership, provider, lifecycle, inspection, and fail-closed availability are
  durable and testable;
- policy input scope follows root IDs rather than path prefixes;
- legacy rows are quarantined without guessed ownership or lost lineage; and
- the remaining local-only path resolver is explicit, contained, and removable
  by #421/#423.

Only `local_filesystem` is implemented. Object stores, root transfer, dual input
formats, inference from legacy paths, and worker-protocol changes are excluded.

## Domain model

### Core vocabulary

`StorageRootId` replaces the narrower `LibraryRootId` name and is the stable ID
used anywhere a location or root relationship crosses a crate boundary. It
follows the existing SQLite-generated, `u64`-backed ID conventions.

`StorageProviderKind` is a closed enum with one value,
`LocalFilesystem`. Its durable and JSON token is `local_filesystem`.

`ProviderLocator` is opaque control-plane configuration for the owning agent.
The control plane enforces non-empty UTF-8, no NUL, and a 4096-byte limit. It
does not canonicalize the value, compare it across owners, or claim it is a
valid local path. The temporary local resolver may interpret it only after the
owner gate passes.

`ProviderRelativeLocator` is normalized at construction:

- 1 through 4096 UTF-8 bytes;
- `/` separators only;
- no leading or trailing `/`;
- no empty, `.`, or `..` components;
- no NUL or backslash; and
- no platform prefix or absolute form.

It is stored and serialized as the normalized string. Joining and filesystem
canonicalization are intentionally not methods on this core type.

`StorageRootState` has `unassigned`, `configured`, `active`, `unavailable`, and
`retired`. `unassigned` exists for migrated roots awaiting explicit ownership;
normal root creation requires an owner and begins at `configured`.

### Library root

The durable root record contains:

| Field | Meaning |
|---|---|
| `id` | Stable `StorageRootId` |
| `library_id` | Existing library relationship |
| `owner_node_id` | Nullable only while migrated state is `unassigned` |
| `provider_kind` | `local_filesystem` |
| `provider_locator` | Owner-scoped opaque provider configuration |
| `display_locator` | Optional operator-facing label, never identity |
| `state` | Explicit lifecycle |
| `enabled` | Independent operator gate |
| `root_epoch` | Fence for provider configuration/resolution identity |
| `activation_identity` | Opaque owner-reported resolution identity |
| timestamps | Existing inspection data |

The unique live-root key is `(owner_node_id, provider_kind,
provider_locator)` when an owner is assigned and state is not `retired`. There
is no global uniqueness constraint on provider locators.

New roots require an existing non-retired owner node. Assignment of a migrated
root requires the same check and moves `unassigned` to `configured` while
remaining disabled until the operator enables it. Owner changes are accepted
only in `unassigned` or pre-activation `configured`; once activation identity
has ever been recorded, ownership is immutable.

Provider kind and provider locator are immutable from creation. Repointing a
root retires the old root and creates a new stable root; it is not an update.
This avoids changing the byte authority under an active ID. The root epoch
therefore fences activation resolution identity in #418. A later design that
permits provider reconfiguration must clear activation, advance the epoch, and
define in-flight-work handling before adding that mutation.

Activation is a repository/control-plane capability for the agent work in
#417, not an operator CLI in #418. It requires the assigned logical owner to be
active and records an opaque validation identity. The first activation advances
the root epoch. Reactivation by the same owner with unchanged provider locator
and identity preserves the epoch; a changed validation identity advances it.
Unavailable and active roots may be retired; retirement is terminal.
Root deletion is removed: retirement preserves its stable ID, location
relationships, and fact history. The root-to-library foreign key changes from
cascade to restrict, so a library with any configured or historical root must
also be retained rather than erasing root history.

Persisted state describes the latest provider-validation result. Owner-node
liveness is an availability overlay, not a fan-out mutation: an active root may
remain `state = active` while inspection reports it effectively unavailable
with `owner_stale` or `owner_retired`. Effective availability is computed,
never inferred from `enabled` alone:

```text
parent library enabled
AND root enabled
AND state = active
AND owner_node_id IS NOT NULL
AND owner node status = active
```

Therefore a disabled, unassigned, configured, unavailable, stale-owner, or
retired-owner root fails before scan, policy selection, or scheduling. The
existing `nodes.epoch` is not used as an incarnation fence. #417 adds the
authenticated current-incarnation evidence to the activation call.

Inspection returns one effective reason from `available`, `library_disabled`,
`root_disabled`, `root_unassigned`, `root_not_active`, `owner_registered`,
`owner_stale`, and `owner_retired`. Corrupt or missing parents/owners are
database errors, not availability reasons.

The control plane appends root-created, owner-assigned, activated,
validation-lost, reactivated, and retired fact events in the same transaction
as the corresponding root write. Node stale/retired events are the single facts
that change the availability overlay; no per-root event fan-out occurs. Event
append failure rolls back the root mutation, and events never schedule work.

Default output, staging, and backup configuration use optional root-ID
relationships rather than path strings. Any selected default must belong to the
same library and be effectively available for the operation that consumes it.

### File location

A normal location record contains stable `FileLocationId`, mandatory
`storage_root_id`, `ProviderRelativeLocator`, content proof, retirement state,
location epoch, and existing observation timestamps. A live unique index on
`(storage_root_id, provider_relative_locator)` preserves multiple locations for
one file while preventing duplicate live addresses inside one root.

The repository create and lookup APIs accept a typed rooted address. They have
no constructor for a location-kind/value pair or for a rootless new row. Reads
represent migrated rootless rows as `UnassignedLegacy` so inspection and
lineage remain truthful, while every work-producing method rejects that variant.

Retirement retains the stable row, lineage references, use leases, hardlink
facts, and proof history. Rediscovery of a live rooted address follows current
location identity/version semantics; it must not merge addresses across roots.

## Persistence and migration 0034

Migration 0034 follows the repository's established SQLite table-rebuild
protocol: it exits sqlx's wrapper transaction, disables foreign-key rewriting,
enables legacy alter-table behavior, rebuilds the tables, reenables foreign
keys, runs `foreign_key_check`, and begins a new transaction for migrator
bookkeeping:

1. Rebuild `library_roots` with owner, provider, state, epoch, and activation
   columns. Copy IDs and operator configuration. Map every old row to
   `owner_node_id = NULL`, `state = 'unassigned'`, and `enabled = 0`; copy the
   old canonical path only as the opaque provider locator for operator
   recognition. Replace path default columns with nullable root-ID defaults;
   every migrated text default becomes `NULL` because its containing root and
   owner cannot be inferred safely.
2. Rebuild `file_locations` with `storage_root_id` and
   `provider_relative_locator`. Preserve IDs, file relationships, proof,
   timestamps, retirement, and epochs. Existing values are copied only into an
   explicitly quarantined legacy-locator column/state with no root; they are not
   accepted as relative locators and are never returned as usable addresses.
3. Recreate every foreign key and index that references preserved IDs, then add
   live uniqueness indexes for roots and rooted locations.
4. Validate migrated counts before dropping old tables and run
   `foreign_key_check` after rebuilding all indexes and constraints.

The concrete schema permits a null `storage_root_id` only when
`address_state = 'unassigned_legacy'`; `provider_relative_locator` is then null
and the historical locator is present only for inspection. For
`address_state = 'rooted'`, root and relative locator are both non-null and the
legacy locator is null. Database checks make every other combination invalid.
Normal SQL write methods create only `rooted` rows.

The rebuild cannot be made atomic because the required SQLite session PRAGMAs
are ignored inside sqlx's migration transaction. Operators therefore need the
normal pre-upgrade database backup. A failed migration is reported as dirty and
must be restored from that backup rather than resumed through a partially
rebuilt schema. Post-migration rollback likewise requires the pre-migration
database and old binaries. There is no reverse transform, because ownership and
safe relative locators cannot be reconstructed from old absolute strings.

## Application flows

### Root administration and inspection

`library root add` replaces `--path`/`--root-kind` with `--owner-node-id`,
`--provider local_filesystem`, and `--provider-locator`. Inspection returns the
stable root ID, owner, provider, persisted state, effective availability and
reason, enabled flag, root epoch, and display locator. It never labels the
provider locator a canonical or globally usable path.

`library root assign-owner` is limited to migrated `unassigned` roots or roots
that have never activated. Assignment, activation, and retirement each run in a
write transaction that checks current state before mutation. Enabling a root
does not activate it; scan and scheduling remain blocked until owner validation
activates it. `library root retire` replaces `remove`; retirement cannot be
reversed. Library deletion reports a conflict while any root row remains.

### Scan and local transition

The CLI removes unrooted `scan --path`; scans name a root. Before traversal the
control plane checks effective availability. Until #421, scanning is permitted
only when the root owner equals the exact `local_node_id` configured for this
control-plane process. Absence or mismatch fails closed; `NodeKind::Local`,
worker placement, and locator text are never substitutes for ID equality. The
temporary resolver canonicalizes the provider root once, resolves each observed
filesystem entry beneath it, derives a normalized relative locator, and rejects
symlink or lexical escape. It records only the rooted address and proof.

`local_node_id` is an optional control-plane construction/configuration value.
The CLI reads `VOOM_LOCAL_NODE_ID` as a checked nonzero `u64`; commands that do
not resolve storage remain usable when it is absent. Test constructors inject
the exact ID explicitly.

Remote-owner roots return an actionable unavailable/unsupported boundary error;
the control plane never tries the provider locator in its own namespace. #421
replaces this local traversal with owner-node observation batches.

### Policy input and operation source

Policy input queries join live locations to effectively available roots and
compare `storage_root_id` with the selected policy root. No `Path::starts_with`
or canonical-path prefix check remains.

Operation source selection carries the rooted address. For the current local
child-worker adapter only, an explicit resolver checks the active local owner,
exact configured local node ID, root and location epochs, canonicalizes the
root, joins the relative locator, and proves the result is contained beneath the
root immediately before dispatch. It returns no durable path. An absent or
mismatched local ID fails closed. #423 removes this resolver and converts child-
worker requests to stable references.

Artifact output/commit path removal belongs to #422 and #423. Code changed in
#418 must not create a new rootless `file_locations` row for those flows.
Existing artifact finalization preserves successful local behavior through
bounded rooted-result plumbing: the selected source location identifies its
source root and library; the target root is that root's configured
`default_output_root_id`, or the source root when no output default is set. The
exact-local resolver proves the existing target path is contained by that
explicit target root and derives the provider-relative locator before the
commit-safety finalizer records it. Missing, unavailable, unowned, or out-of-
root targets fail before location creation. The artifact schema, commit
authority, worker request, and child path contract do not change here; #422 and
#423 retain those responsibilities.

## Error and failure ordering

Validation order is observable and remains deliberate:

1. parse and validate typed IDs and locator shape;
2. load the root and owner, reporting corrupt persisted values as database
   errors rather than absence;
3. enforce ownership and provider-configuration immutability and lifecycle
   transition rules;
4. enforce enabled and effective availability;
5. validate location/root epochs and relationships;
6. only at the temporary local boundary, resolve and prove containment; and
7. begin scan, policy, scheduling, or dispatch work.

No mutation or event occurs before all gates relevant to it pass. Root mutation
precedes its fact append inside one transaction and is rolled back when append
fails; the event becomes visible only with the committed root state. Existing
transaction ownership and event ordering elsewhere remain unchanged. A corrupt
root state, provider token, epoch, or locator shape is a database error.
Operator validation errors remain configuration errors and missing IDs remain
not found.

## Security and trust boundaries

- Provider locators are untrusted persisted configuration. The control plane
  stores and displays them but does not assume they name its filesystem.
- Relative locators are validated on input and again when read from SQLite.
  Backslashes, absolute prefixes, `.`/`..`, NUL, empty components, and overlong
  values are rejected; locators are opaque text and are never URL-decoded.
- Root ID plus relative locator is authorization context, not authorization by
  itself. Every work path checks root owner, lifecycle, enabled state, node
  status, and epochs.
- Local resolution requires exact equality with the configured local node ID.
  Node kind, worker placement, and path similarity do not establish authority.
- The temporary local resolver uses canonical filesystem results and component-
  aware containment, not textual prefix matching. Symlink escape fails closed.
- Root owner and activation mutations check and update lifecycle state in one
  write transaction. Activation by an authenticated current incarnation is
  intentionally deferred to #417; #418 exposes no unauthenticated remote
  activation endpoint.
- Inspection may reveal opaque provider locator strings to authorized local CLI
  users under the existing access model. Provider credentials are forbidden.

## Verification strategy

Tests must first fail against the old model, then prove:

- two active nodes may own roots with identical provider locator strings;
- duplicate live locators for one owner/provider fail;
- owner assignment is rejected after activation;
- every disabled/unassigned/configured/unavailable/stale-owner/retired-owner
  combination fails scan and policy eligibility;
- invalid relative locators and corrupt persisted locator/state/epoch values
  fail with the correct class before business classification;
- location uniqueness is root-scoped, multiple locations per file remain, and
  retirement/rediscovery, lineage, hardlink proof, and location epochs survive;
- policy inputs use root relationships and cannot be widened by path prefixes;
- local resolution rejects lexical and symlink escape and remote ownership;
- artifact finalization preserves successful same-root/default-output-root
  completion while rejecting targets outside the explicit available root;
- migration preserves root/location IDs and foreign-key relationships while
  disabling roots and quarantining all legacy locations;
- removed CLI fields and former serialized location fields are rejected, with
  `deny_unknown_fields` on concrete durable payloads where applicable; and
- no production dispatch code newly persists a globally meaningful absolute
  path.

Focused repository, control-plane, and CLI tests run during implementation.
Completion requires `just ci`, `git diff --check origin/main...HEAD`, an
adversarial review loop, and a diff-scoped threat scan. The known baseline
SQLite-contention flake is tracked separately in #435; a fresh final suite must
still pass before this branch is reported green.
