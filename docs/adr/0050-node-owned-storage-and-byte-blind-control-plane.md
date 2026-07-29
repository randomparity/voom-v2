# ADR 0050: Node-owned storage and a byte-blind control plane

Status: Accepted

Date: 2026-07-29

Issue: #414

## Context

VOOM already has durable remote nodes, authenticated lease APIs, scheduler
scoring, artifact handles, and network-capable out-of-process workers. Real
media execution nevertheless assumes one filesystem namespace:

- library roots and file locations contain host-absolute paths;
- scan hashing and source selection open those paths in the control-plane
  process;
- concrete tool readiness excludes remote workers; and
- final artifact promotion runs on the control-plane host.

A two-host experiment exposed that boundary. A remote worker produced an audio
artifact, but the control plane then read, hashed, verified, and copied it
through a shared mount. This made a shared path namespace an undeclared
requirement and kept post-dispatch work outside the operation's claim heartbeat.

The original specification chose distributed coordination, remote nodes,
logical artifact handles, same-node locality, and host-owned commit. "Host" was
not precise enough: the machine hosting SQLite need not be the host that owns
the media bytes.

This ADR defines the target architecture for epic #413. It does not claim that
the current path-based implementation already satisfies the decision.
Issues #416 through #425 replace that implementation in dependency order.

## Decision

### Authority model

One authoritative control plane owns durable policy, planning, tickets, leases,
grants, safety decisions, fencing, catalog state, and audit events. Tickets
continue to route work and events continue to record facts.

One pull-based node agent runs on each storage host. It:

- authenticates as a durable logical node and heartbeats its current
  incarnation;
- activates and resolves storage roots owned by that logical node;
- claims leases from the control plane;
- supervises version-matched child workers over node-local endpoints;
- resolves provider-relative locations to paths only on that node; and
- performs or supervises every operation that reads or changes owned bytes.

A logical node is the stable storage authority. A node incarnation is one
authenticated agent process lifetime, identified by the logical node ID and a
monotonically fenced node epoch. Restarting an agent creates a new incarnation
without changing root ownership. Messages from an older epoch are stale.

Each storage root has exactly one logical owner. Ownership is immutable for the
root's lifetime; changing hosts requires an explicit future migration that
creates or transfers authority under a separate accepted design. A stale or
retired node has no active incarnation, so its roots are unavailable rather
than silently reassigned.

A configured root becomes active only after its current owner incarnation
validates it, which advances the root epoch. Loss of that incarnation or failed
local validation makes the root unavailable and fences the prior epoch. Only a
new validation by the same logical owner may reactivate it, with another epoch.
Configured, active, or unavailable roots may be retired; retirement is
terminal.

The node agent is the host trust boundary. Child FFprobe, FFmpeg, MKVToolNix,
backup, verification, scan, and hash workers remain out of process but bind
only to node-local interfaces. Remote agent traffic is authenticated and
encrypted. Existing exact worker-protocol version matching remains mandatory.

### Storage roots and locations

A storage root is identified by:

- a stable root ID;
- its owner logical node ID;
- a provider kind;
- a node-scoped provider root locator; and
- lifecycle and fencing epochs needed to reject stale activation.

`local_filesystem` is the only provider implemented by this epic. The provider
root locator is configuration for the owner agent, not a path the control plane
may open. Provider credentials are never stored in location payloads.

A live file location is identified by `(storage_root_id,
provider_relative_locator)`, plus its location epoch and optional
provider-specific immutable-object proof. The relative locator is normalized by
the provider, cannot escape its root, and is never compared with an absolute
path from another node. Two nodes may use the same local path string without
aliasing either the root or its locations.

For a local filesystem root, only its owner agent canonicalizes the configured
root and joins a relative locator to it. The control plane validates
provider-independent shape and ownership but does not canonicalize or open the
result. Policy-input scoping follows durable root/location relationships, never
canonical-path prefix matching.

The provider kind and opaque relative-locator boundary leave room for a future
object-store or archive provider. Such a provider must define its own locator,
proof, credential, commit, and recovery semantics in a later ADR. No S3
behavior is implied here.

### Testable byte-ownership boundary

The control plane must remain correct when its operating-system identity cannot
read or traverse any configured media, staging, output, backup, or recovery
root. It may persist locators, sizes, checksums, stat evidence, media facts,
verification reports, and commit receipts supplied through authenticated
contracts. It must not:

- traverse a media root;
- open, hash, probe, copy, verify, promote, remove, or recover media bytes;
- turn a provider-relative locator into a local path; or
- send an absolute storage-host path to another node.

All byte-touching protocol requests use stable root, location, artifact,
version, and commit-intent references. A hermetic two-host acceptance test
enforces the boundary by denying the control-plane process filesystem access,
not merely by asserting that a particular helper was not called.

### Scan sessions and observation lifecycle

Scan, hash, and probe are distinct node-agent capabilities. A manual scan
session is durable and bound to one storage root, one root epoch, and the owner
node incarnation that accepted it. The session lifecycle is:

- `running`: ordered observation batches may be accepted;
- `succeeded`: the agent has submitted a complete traversal watermark and all
  required observations;
- `failed`, `cancelled`, or `stale`: terminal without absence reconciliation.

Observation batches carry a session-scoped monotonically increasing sequence
and an idempotency key. Replaying an identical batch is a no-op; reusing its
identity with different content fails closed. Each observation names a
provider-relative locator and includes provider-local object identity/stat
facts and stability evidence. Hash and probe results bind to the observed
object facts so content drift cannot combine facts from different byte
versions.

A primary-media location becomes eligible for policy input only after its
current content hash and media snapshot are durable. A sidecar that is not a
probe target instead requires its current hash and the classification and
bundle evidence required by its role. Unsupported or incompletely evidenced
observations remain inspectable but ineligible. The control plane infers a
missing location only when it atomically accepts successful session completion
for the complete traversal. A partial, failed, cancelled, timed-out,
stale-incarnation, or root-epoch-mismatched session never retires an unseen
location.

This delivers the manual scan-session and reconciliation substrate from Sprint
18. It does not add a filesystem watcher, debounce loop, or continuous daemon
scan.

### Owner-local scheduling and access plans

Every ticket and artifact access plan carries stable handles and root/location
references. For a byte-touching operation, the scheduler applies ownership as
a hard eligibility gate before scoring:

- the selected worker is supervised by the active incarnation of the source
  root's logical owner;
- source, staging, output, backup, verification, and commit roots all have that
  same logical owner; and
- every referenced root and location epoch is current.

An owner mismatch or unavailable owner produces no dispatch. Locality is not a
soft score for these operations. An access plan in this epic authorizes
owner-local resolution only; it never transfers or streams bytes between
nodes.

Concrete policy tool readiness is evaluated for workers on the required owner
node. The control plane still evaluates grants and authenticated capability
facts, while the agent proves that its node-local provider is ready.

### Distributed commit intent and fencing

The control plane remains the authority that decides whether a mutation may
commit. The storage-owning node is the only authority that may perform the
filesystem mutation.

After worker output validation and the existing safety checks, the control
plane creates one durable, idempotent commit intent. The intent binds:

- job, ticket, lease, operation attempt, and artifact identities;
- owner node, node epoch, storage roots, and root epochs;
- source, staged, and target location identities and epochs;
- expected size, checksum, and provider-local object facts;
- the evaluated lineage closure and safety epochs; and
- a unique commit generation and fencing token.

Authorizing the intent reserves the affected durable scope. While it remains
authorized, conflicting blocking-lease acquisition and in-scope
location/lineage mutation fail closed. The node agent accepts only the current
owner, root epochs, operation generation, and fencing token. Immediately before
mutation it revalidates the expected local facts. The agent owns a
crash-durable local journal outside worker staging. It must durably write and
sync an idempotent `not_started` receipt keyed by the commit-intent ID and
generation before requesting permission to apply. Journal failure prevents the
mutation.

The durable control-plane lifecycle is:

- `prepared`: safety evidence captured, no node mutation authorized;
- `authorized`: fence active and eligible for owner-node execution;
- `applying`: the owner reported that the fenced mutation may have begun;
- `committed`: node receipt and post-mutation facts finalized with catalog
  lineage/location state;
- `aborted`: mutation is proven not to have happened; or
- `recovery_required`: the outcome cannot yet be proven.

Allowed transitions are monotonic:

- `prepared -> authorized | aborted`;
- `authorized -> applying | aborted`;
- `applying -> committed | recovery_required`; and
- `recovery_required -> committed | aborted`.

`committed` and `aborted` are terminal. A retry after an aborted generation
creates a successor intent with a new generation and fence; no state transition
reopens an old generation.

Application uses an explicit begin handshake:

1. the agent persists the `not_started` receipt and revalidates local facts;
2. it requests begin-apply with the authorized intent and exact fencing token;
3. the control plane atomically revalidates the lease and all bound epochs,
   advances `authorized` to `applying`, and only then returns permission for
   that generation; and
4. the agent performs the mutation only after receiving that response, then
   durably advances the receipt with its outcome before reporting completion.

A response lost after step 3 is never treated as a pre-apply failure: the
control plane enters `recovery_required` and reconciles the durable receipt and
target facts. A response lost before the agent receives step 3 leaves its
receipt `not_started`, which proves the mutation did not begin.

The `authorized -> applying` transaction is the lease-freshness linearization
point. Expiry before that transaction prevents permission and permits abort
after `not_started` is proven. Expiry after it cannot revoke a mutation that
may already have started; it forces `recovery_required`, while the intent fence
prevents ticket takeover or other work from authorizing a conflicting commit.

The node's receipt distinguishes `not_started`, `applied`, and an ambiguous
local outcome. Replaying the same generation returns the same receipt or safely
completes its provider-specific idempotent action. A different or stale
generation cannot reuse the target. Loss or corruption of the local journal
after `applying` is itself ambiguous and requires operator recovery; it never
authorizes an automatic repeat.

Failure before `applying` may abort or retry with a new generation after the
old fence is terminal. Once mutation may have started, timeout, lost response,
agent crash, or database failure enters `recovery_required`; the fence remains
blocking. Recovery queries the same owner for its receipt and verifies actual
target facts. It then finalizes `committed` if application is proven, returns
the old generation to terminal `aborted` if non-application is proven, or stays
blocked for operator recovery when neither fact is provable. A retry after
`aborted` uses a new successor intent and generation. Recovery never repeats a
destructive mutation merely because an HTTP response was lost.

During healthy execution, workflow leases remain live through worker dispatch,
post-dispatch validation, verification, commit, and the terminal lease
transition. Operation claims renewed by those heartbeats cover the same
interval. A genuinely missed heartbeat or expiry follows the existing
fail-closed lease rules and is never resurrected. If the intent may have
started, the lease failure also leaves the intent `recovery_required`; its
independent fence continues blocking conflicting work until reconciliation.
Issue #415 fixes the current narrower heartbeat independently. Commit fencing
protects ambiguous distributed outcomes; it is not a workaround for claim
expiry.

### Compatibility and delivery

This is a pre-release flag-day replacement. Implementation removes
control-plane-local path and promotion behavior as its node-owned equivalent
lands. There is no dual path/location protocol and no compatibility shim.
Durable payload changes continue to follow ADR 0013, and every control-plane,
agent, and child-worker protocol boundary continues to require the exact
version from ADR 0016.

The implementation maps to the original roadmap as follows:

- Sprint 7 node identity becomes logical-node plus incarnation identity.
- Sprint 8 access plans become owner-local handle/location authorization.
- Sprint 9 locality becomes a hard owner gate before scoring.
- Sprint 10 scan/hash/probe move from the control-plane host to the root owner.
- Sprint 18 gains manual durable scan sessions and complete-session
  reconciliation, without its watcher loop.

ADR 0027 is superseded where it uses globally canonical paths, rejects a
root/location relationship, or scopes policy input by path prefix. Its library
configuration and fail-closed disabled-root behavior remain.

ADR 0034 is superseded where it excludes authenticated remote workers from
concrete tool readiness. Its typed requirements, deny-wins grants, identity
proof, and fail-before-execution behavior remain.

ADRs 0019 and 0025 are amended only as to execution host: their commit safety,
backup-before-mutation, durability, and recovery guarantees remain, but byte
inspection and filesystem mutation occur on the storage owner under
control-plane authorization.

Rollout and rollback for the later implementation are migration-specific.
Before a location/root migration commits, an old binary remains valid. After
the flag-day migration, rollback requires restoring the pre-migration database
and binaries together; old binaries must not reinterpret provider-relative
locations as local paths.

## Consequences

- The control plane can coordinate and inspect real media workflows without
  filesystem access to media bytes.
- Absolute path equality is no longer a distributed identity assumption.
- One root has one clear storage authority, and every byte-touching operation
  has a mechanically testable owner.
- Node loss makes owned storage unavailable. This design chooses safety over
  implicit failover.
- Scan and commit gain more durable states because network loss can make
  completion ambiguous even when the local filesystem action succeeded.
- Owner-local scheduling cannot use otherwise idle workers on another node
  without an explicit future transfer facility.
- Object stores can later implement the same root/location/receipt boundaries,
  but this ADR provides no object-store behavior or credentials.

## Security, observability, and verification

The remote control-plane API and agent authentication are delivered by #416
and #417. Transport must be encrypted, agent credentials must be node-scoped,
and child worker credentials and endpoints must not be remotely reachable.
Provider root locators and relative locations must not contain secrets.

Every lifecycle transition records an append-only fact event, but events never
claim work or activate a fence. Operators can inspect node incarnation, root
availability, scan-session progress, owner-gate rejection, commit intent,
receipt evidence, and recovery state without gaining byte access.

Required verification includes:

- schema and repository tests for authority, epochs, uniqueness, lifecycle,
  idempotency, and stale-message rejection;
- traversal and locator property tests proving a relative locator cannot escape
  its root;
- scheduler tests proving non-owner workers are ineligible before scoring;
- scan tests proving replay safety and absence reconciliation only on complete
  success;
- commit tests for success, pre-mutation failure, lost response, agent restart,
  journal write/sync failure or loss, stale fences, fact drift, and ambiguous
  recovery;
- operation tests that exceed the original lease/claim TTL after dispatch; and
- a two-host acceptance test where the control-plane OS identity cannot access
  any media root.

## Considered and rejected

### Keep a shared filesystem namespace

Rejected because it makes identical paths across hosts alias, lets the control
plane read bytes, and turns NFS or an equivalent mount into an undeclared
requirement.

### Let the control plane transfer bytes between nodes

Rejected because no demonstrated workflow requires transfer. It adds bandwidth,
partial-transfer, cache, credential, and cost semantics before they are needed.

### Run peer-to-peer or federated control planes

Rejected for this epic because multiple coordination authorities would require
consensus over tickets, ownership, fences, and catalog truth. The node-agent
contract keeps authority messages explicit, so a future peer-control-plane
design can replace the single authority without moving byte ownership into
today's control plane.

### Let roots fail over or have multiple owners

Rejected because an unreachable owner cannot prove filesystem state, and two
writers cannot safely share local mutation authority without another fencing
system.

### Store absolute paths alongside provider-relative locations

Rejected because a dual contract would preserve the ambiguity this decision
removes. VOOM is pre-release, so the implementation replaces the path contract
in lockstep.

### Implement S3 or archival backup now

Rejected as speculative scope. Stable provider kinds, relative locators,
immutable proofs, and commit receipts are the intended extension seam for that
later work.
