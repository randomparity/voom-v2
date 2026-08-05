# ADR 0055: Node-owned roots and provider-relative file locations

Status: Accepted

Date: 2026-08-04

Issue: #418

## Context

ADR 0050 makes a logical node the stable authority for its storage roots and
requires file locations to be root-relative. The current durable model instead
identifies a library root by a globally unique canonical path and identifies a
file location by an absolute path. The control plane canonicalizes both. This
aliases equal path strings on different hosts, permits path-prefix policy
scoping, and gives persisted rows no durable owner or root relationship.

Issue #418 replaces that model. It does not implement the node-incarnation
protocol from #417, remote scan execution from #421, owner-node commit from
#422, or reference-only worker dispatch from #423. Those issues remain
responsible for removing the remaining transitional byte-access paths.

## Decision

### Root identity and lifecycle

A library root is a storage root with a stable root ID, one logical owner node,
the `local_filesystem` provider kind, an owner-scoped opaque provider locator,
and a fencing epoch. A live locator is unique only within `(owner_node_id,
provider_kind)`; the same locator on two owners identifies two different roots.

New roots are created in `configured`. Their owner may be assigned or corrected
only before first activation. Provider kind and locator are immutable after
creation; a different configuration requires retirement and a new stable root.
Activation records provider validation supplied by the owner-node boundary and
moves the root to `active`. Explicit validation loss moves it to `unavailable`;
successful validation by the same owner may reactivate it. Retirement is
terminal. After first activation, owner changes are rejected rather than
modeled as updates.

Persisted root state records provider validation; effective availability
overlays the current owner-node status without rewriting every root when one
node becomes stale or retires. Inspection exposes both the persisted state and
the effective unavailable reason. Root use fails closed unless its parent
library and the root are enabled, it is assigned and active, and its owner
logical node is active. Unassigned, configured, unavailable, disabled-library,
disabled-root, stale-owner, and retired-owner roots cannot scan or schedule
byte-touching work. Issue #417 will add the authenticated current-incarnation
proof to activation; this change does not mistake `nodes.epoch` for an
incarnation fence.

Root creation, owner assignment, activation, validation loss, reactivation, and
retirement each append a fact event in the same control-plane transaction as
the state change. An owner-node stale or retired event already records the fact
that changes the computed availability of all its roots; it does not fan out
synthetic root-state mutations.

### Root-relative locations

A new live file location is identified by `(storage_root_id,
provider_relative_locator)`. It retains its stable location ID, location epoch,
content proof, retirement history, and lineage relationships. The relative
locator is a provider-independent normalized slash-separated string. It is
non-empty, contains no NUL or backslash, is not absolute, and contains neither
empty, `.` nor `..` components. These checks prevent syntactic root escape;
only the owner node may canonicalize a local-filesystem root and prove resolved
containment.

The control plane compares root IDs for policy scope and validates ownership,
state, and epochs. It neither treats provider locators as globally meaningful
paths nor uses path-prefix containment. New repository and command inputs do
not accept the removed location kind/value shape.

### Flag-day migration and transitional boundaries

Migration 0035 rebuilds the root and file-location tables in place. It preserves
stable IDs and all foreign-key relationships. Because existing absolute paths
do not prove either an owner or a containing root, migrated roots are disabled
and `unassigned`, and migrated file locations are explicitly quarantined as
unassigned legacy records. They remain inspectable for history and lineage but
are ineligible for scan, policy input, scheduling, or mutation. An operator must
assign each migrated root deliberately; ownership is never inferred from path
text. Existing files are rediscovered through a later owner-node scan rather
than silently rebound.

This is one durable schema and one accepted write format, not a dual-schema
compatibility path. The nullable root on a quarantined legacy row is a migration
state unavailable to normal create APIs. Removed absolute-path inputs are
rejected.

Until #421 and #423 land, the existing in-process local scanner and current
media-worker adapter may resolve a rooted locator at a narrow, owner-local
boundary. That resolver requires the root owner to equal the control-plane
process's explicitly configured local node ID and must perform canonical
containment before byte access. It never infers locality from node kind, worker
placement, or locator text. It does not persist a global path or make remote
roots usable. Worker-protocol and worker implementations remain unchanged by
#418; #423 replaces their path-bearing requests.

## Consequences

- Equal provider locator strings on different nodes no longer alias roots.
- Every newly recorded location has a stable root relationship and a bounded
  relative locator; legacy absolute locations cannot enter new work.
- Policy root scope is relational and cannot be widened by textual prefix
  collisions.
- Root availability and owner status become explicit fail-closed gates.
- Existing deployments require deliberate root assignment and rescan after the
  flag-day migration. Rollback requires restoring a pre-migration database and
  binaries; there is no down-conversion to the removed shape.
- The control plane still has a documented temporary local path-resolution seam
  until #421 and #423. This ADR does not claim the full ADR 0050 byte-blind
  acceptance test is delivered.
- Only `local_filesystem` is supported. Object stores, ownership transfer, path
  guessing, and compatibility shims remain out of scope.

## Considered and rejected

- **Add owner/root columns beside the existing path identity.** Rejected because
  two accepted write models would preserve ambiguous global-path behavior.
- **Infer owners and containing roots from existing path prefixes.** Rejected
  because paths are meaningful only on their source hosts and overlap does not
  prove authority.
- **Create a second generic storage-root table beside library roots.** Rejected
  because #418 needs one root concept and an extra mapping adds no implemented
  provider or behavior.
- **Permit ownership changes by updating a live root.** Rejected because in-
  flight and historical authority would become ambiguous; a future transfer
  design must define fencing and migration explicitly.
- **Implement reference-only worker requests here.** Rejected because #423 owns
  that coordinated worker-protocol and worker rollout.
