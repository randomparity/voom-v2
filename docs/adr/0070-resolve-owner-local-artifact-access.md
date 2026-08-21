# ADR 0070: Resolve and gate owner-local artifact access

## Status

Accepted

## Context

Issue #476 requires resolving a ticket's canonical artifact-access declaration against persisted storage state to prevent non-owner and mixed-owner byte work from becoming schedulable. This builds on the declaration vocabulary added in issue #475 (ADR 0069).

The resolution must prove one common configured owner using stable IDs and epochs only, never treating path strings or shared-mount naming as ownership evidence. It is read-only with respect to storage and scheduling state.

### Constraints from Issue #476 Comments

The adversarial review of #475 surfaced three implementation constraints:

1. **Do not read the `write` entry as a destination.** For the seven output-producing operations, the declaration emits `StorageRoot(source_root) -> write` alongside `FileLocation(source_root, source_location) -> read`. The write entry names the root the ticket **reads from**, not the one it writes to. This is read locality, not write locality. #484 owns fixing it.

2. **Fake-scanner ids name no row.** `voom-fake-support`'s fake scanner emits `file_location_id` values from a synthetic band (`9_100_001+`) that reference no `file_locations` row. Resolution must fail closed on them. The fix is to seed real rows using `voom_store::test_support::seed_test_rooted_location` or assert the failure — never by tolerating an id that names nothing.

3. **Do not derive locality from root-addressed entries.** When a ticket's source is `TicketStorageSource::Root`, every right it declares attaches to the root. An output-producing operation collapses to a single `storage_root` entry carrying `[read, write]`. This is a claim over every artifact in the root, made by a ticket that touches one staged file. The only sound reading is "this ticket touches something in this root" — do not derive read locality, co-scheduling decisions, or serialization scope from it.

### Requirement from #475 (ADR 0068 Consequences)

**Resolution must read `file_location_id` whenever a declaration entry names one, and must never satisfy a `file_location` entry from its `storage_root_id` alone.**

The hazard is a resolver keyed only on the root — it would satisfy a fabricated location without ever reading the location id, letting fake-scanner-descended tickets route as though their bytes were real.

## Decision

### Architecture

All SQL queries live in `voom-store::repo::execution::artifact_access_resolution` following the SQL boundary guardrail. The control-plane layer (`voom_control_plane::workflow::plan::artifact_access_resolution`) orchestrates resolution using these typed repository methods.

### Resolution Module Structure

**voom-store layer:**
- `resolve_storage_root` - Typed query validating root existence, owner, state, epoch
- `resolve_file_location` - Typed query enforcing storage_root_id constraint
- `resolve_active_incarnation` - Typed query for owner's current active incarnation
- Types: `ResolvedRoot`, `ResolvedLocation`, `AccessResolutionError` (stable, locator-free)

**Control-plane layer:**
- `resolve_artifact_access` - Orchestration function consuming repository methods
- Validates single common owner across all references
- Returns `AccessResolution` with owner node ID and incarnation for downstream consumers

### Resolution Algorithm

1. **Validate each declaration entry:**
   - `StorageRoot`: Call `resolve_storage_root`, validate owner and state
   - `FileLocation`: Call `resolve_file_location` (validates storage_root_id internally), then check owner
   - `ExistingArtifact`: Verify location via `resolve_file_location`, check owner
   - `PlannedArtifact`: Validate only the target storage root (no location exists yet)

2. **Enforce single owner:** All resolved references must share one common `owner_node_id`

3. **Validate state:** Reject retired, unassigned, or invalid state roots and locations

4. **Check epochs:** Decode `root_epoch` and reject negative values as corrupt; valid epoch zero is accepted

5. **Resolve active incarnation:** Call `resolve_active_incarnation` for the owner's current active incarnation

### SQLite Data Treatment

All SQLite data is treated as untrusted persisted data. Resolution performs:
- Checked numeric conversions (e.g., `Option<i64>` before using)
- Explicit error messages that never include paths or locators

### Database Error Classification

- Missing rows → Domain error (`StorageRootNotFound`, `FileLocationNotFound`)
- Invalid state → Domain error (`InvalidRootState`, `InvalidLocationState`)
- Mixed ownership → Domain error (`MixedOwner`)
- Database access failures → `DatabaseError`

### Scheduler Integration

The scheduler-facing path lives in the remote-acquire candidate building
(`voom_control_plane::cases::execution::remote_execution::acquire`): every ready
ticket is resolved against storage state inside the acquire transaction before
any candidate reaches the scorer, and a ticket whose declaration fails
resolution — or whose single common owner is not the acquiring node — is
dropped from the snapshot. Filtering preserves the deterministic
ready-ticket ordering, and a fully rejected snapshot falls through to the
existing idle path. `operation_eligibility_in_tx` was deliberately left
untouched: it sees only a worker and an operation, never a ticket's
declaration, so it cannot carry this check.

## Consequences

- **Materialized handles** resolve through a live location and active root; a
  retired location row fails closed
- **Every referenced root/location epoch and relational binding is checked** including valid epoch zero
- **A `file_location` entry is satisfied only by reading its `file_location_id`** — never from `storage_root_id` alone
- **Missing, inactive, stale, retired, mixed-owner, or corrupt references fail closed** with stable, locator-free evidence
- **The configured owner node and its current active incarnation are checked** without new per-incarnation activation APIs
- **A scheduler-facing path rejects non-owner and mixed-owner candidates before scoring** while preserving deterministic ordering
- **Fake-scanner tickets fail resolution** unless real `file_locations` rows are seeded
- **Root-addressed entries are treated as "this ticket touches something in this root"** — no locality/co-scheduling/serialization scope derived from them
- **All SQL lives in voom-store typed repository methods** — control-plane orchestrates only. The repository functions are executor-generic, so resolution composes inside a caller's transaction.

## Alternatives Considered

### SQL in control-plane
Rejected by guardrail `check-control-plane-sql-boundary`. All SQL must live in typed voom-store repository methods.

### Accept root-keyed resolution with location id validation
Rejected because it would still satisfy fabricated locations. The specification requires reading the `file_location_id` and verifying the row exists.

### Add per-incarnation physical-storage activation APIs
Rejected because #421 owns this runtime proof. This slice uses only what exists: configured owner and active incarnation.

### Make tolerance for fake-scanner ids configurable
Rejected because fabricated evidence should fail closed, not be tolerated. Tests can seed real rows for fake flow testing.

## Testing

Resolution is covered by:
- Unit tests for each resolution path (storage root, file location, artifact, planned artifact)
- Error case tests (not found, invalid state, mixed owner)
- Fake-scanner id rejection test
- Real-SQLite corruption tests (incomplete rows, invalid types)
- Concurrency tests (transaction composure under concurrent reads)
- Scheduler integration tests (candidate filtering preserves ordering)

## Migration

No database schema changes required. This is read-only resolution of existing tables (`library_roots`, `file_locations`, `node_incarnations`, `artifact_handles`, `artifact_locations`).