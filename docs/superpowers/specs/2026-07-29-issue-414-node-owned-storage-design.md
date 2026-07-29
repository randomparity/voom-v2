# Issue #414 — Node-owned storage architecture

## Status

Approved architecture for epic #413. Implementation is pending issues #415
through #425.

## Purpose

Define the authority, identity, lifecycle, failure, and recovery contract that
lets remote nodes operate on their own storage while the authoritative control
plane coordinates the work without reading media bytes.

ADR 0050 is the governing decision. This design maps that decision onto the
existing VOOM domain and delivery sequence. It does not define a second
normative state vocabulary; if wording conflicts, ADR 0050 controls.

## Existing invariants

The design preserves these accepted contracts:

- durable tickets and leases schedule work; events only record facts (ADR
  0001);
- every provider remains an out-of-process worker (ADR 0002);
- durable JSON changes are deliberate, additive when compatibility is required,
  and reject unknown fields (ADR 0013); and
- control-plane, agent, and worker protocol participants require an exact
  version match (ADR 0016).

The implementation is a pre-release replacement, not a dual-mode transition.
No control-plane-local fallback may silently activate when an owner agent is
unavailable.

## Domain language

| Term | Meaning |
|---|---|
| Logical node | Stable durable identity that owns storage roots |
| Node incarnation | One authenticated agent process lifetime with a fresh opaque incarnation ID |
| Node agent | Pull-based host supervisor that resolves roots and manages node-local workers |
| Storage root | Stable provider namespace owned by exactly one logical node |
| Provider-relative locator | Provider-normalized identity within one storage root |
| File location | Root ID, relative locator, location epoch, and optional immutable proof |
| Byte-touching work | Any discovery, hash, probe, mutation, backup, verification, promotion, deletion, or recovery that opens storage |
| Scan session | Durable complete-or-incomplete traversal attempt for one root and incarnation |
| Commit intent | Control-plane authorization and fence for one idempotent owner-node mutation |
| Commit receipt | Node-local evidence of whether the fenced mutation started or applied |

Absolute paths are node-local implementation details, not distributed domain
identifiers.

## Authority matrix

| Decision or action | Control plane | Owner node agent | Child worker |
|---|---|---|---|
| Policy, plan, ticket, and lease truth | Authoritative | Pulls and reports | None |
| Logical node and root ownership | Authoritative | Proves current incarnation | None |
| Provider root canonicalization | Stores opaque config and evidence | Authoritative local resolver | None |
| File traversal and stat facts | Persists authenticated observations | Supervises and reports | Scan worker observes |
| Content hash and media facts | Persists bound evidence | Supervises and reports | Hash/probe worker computes |
| Worker capability and grant | Authoritative eligibility | Proves node-local readiness | Proves exact identity/version |
| Safety and commit authorization | Authoritative | Enforces current fence | None |
| Filesystem mutation | Must not perform | Authoritative local executor | May stage output under supervision |
| Catalog finalization | Authoritative after receipt verification | Supplies receipt and facts | None |

The matrix is fail-closed. If an authority cannot prove its part, the operation
does not cross that boundary.

## Identity and lifecycle

### Logical node and incarnation

The existing durable node ID becomes the logical storage-owner identity. A
successful agent start authenticates and receives a fresh opaque incarnation
ID. Root activation, scan sessions, leases, observation batches, access plans,
and commit requests bind to that ID. The existing `nodes.epoch` continues to
serve optimistic row concurrency and may advance on heartbeats; it is not the
incarnation fence.

Node lifecycle remains registered, active, stale, and retired:

- `registered`: no active incarnation has proved readiness;
- `active`: the current incarnation is authenticated and heartbeating;
- `stale`: heartbeat authority expired; owned roots are unavailable;
- `retired`: terminal; no future incarnation or owned-root work is accepted.

A late message from an older incarnation is rejected even if its credential was
once valid.

### Storage root

A root is configured before activation. The owner agent validates and
canonicalizes its provider locator locally, then reports an activation result
bound to the current incarnation ID. Activation advances the root epoch only
when provider configuration or validated resolution identity changes.

Operational root states are:

- `configured`: durable owner and provider configuration exist;
- `active`: current owner incarnation proved local resolution;
- `unavailable`: owner or activation is stale or failed;
- `retired`: terminal and ineligible for new work.

Allowed transitions are `configured -> active | retired`, `active ->
unavailable | retired`, and `unavailable -> active | retired`. Activation and
reactivation require validation by the current incarnation of the same logical
owner. A benign agent restart may reactivate unchanged evidence without root
epoch churn. `retired` is terminal.

Root ownership never follows a path or an agent registration. A future host
transfer needs a separate fenced migration protocol.

### Location

Location uniqueness is `(storage_root_id, provider_relative_locator)` among live
locations. The provider normalizes the locator and proves that resolution stays
within the activated root. The durable location epoch advances when the live
object identity changes or the location is retired/re-established.

For `local_filesystem`, observations may include device/inode, size, timestamps,
and stability samples. These are evidence, not global identity. A content hash
defines immutable byte-version identity only after the agent proves the hash
was computed for the same pre/post stat facts.

## Byte-blind request contracts

Control-plane application services must accept and return identifiers and
evidence, never `Path` values for storage operations. Boundary tests should
make forbidden byte access impossible:

1. start the control plane under an OS identity with no traversal permission on
   the media, staging, output, or backup roots;
2. expose only the authenticated remote API;
3. configure a root using provider data passed opaquely to its owner agent;
4. scan, plan, execute, verify, commit, recover, and inspect through durable
   references; and
5. assert success without mounting or granting the root to the control-plane
   host.

Provider locators may be visible for operator inspection, but no control-plane
code may parse them into a filesystem path. Secrets use credential references
owned by the agent deployment, not root/location payload fields.

## Scan, hash, and probe flow

1. The control plane creates a scan ticket for an active root.
2. Owner-local scheduling leases it only to the root owner.
3. The agent starts a scan session bound to root ID, root epoch, node ID, and
   incarnation ID.
4. The scan worker emits ordered, idempotent observation batches.
5. The agent submits the complete traversal watermark.
6. One transaction marks the session succeeded and reconciles previously live
   locations not observed by that complete traversal.
7. Separate owner-local hash and probe tickets enrich durable observations
   independently, without delaying traversal completion.
8. The control plane creates or advances file versions and snapshots only from
   enrichment bound to current object facts, root epoch, and incarnation ID.

Session state transitions are monotonic. Duplicate identical batches and
completion requests return their original result. Conflicting replay,
out-of-order gaps, stale root/incarnation identity, content drift, and
observations outside the root fail the session without inferring absence. A
session deadline or owner-heartbeat expiry moves `running` to terminal `stale`;
there is no separate timeout state.

Policy input selection joins roots to live locations. Primary media that
requires probing is eligible only with a current content hash and matching
media snapshot. A sidecar that is not a probe target instead requires a current
hash plus the classification and bundle evidence required by its role.
Selection does not use a path prefix. An unsupported, incompletely evidenced,
unprobed primary-media, or drifted observation can remain inspectable without
becoming policy-eligible.

## Scheduling and worker supervision

The scheduler first derives the set of roots touched by a ticket. A
byte-touching ticket is eligible only when:

- all roots have one logical owner;
- that owner has one active current incarnation;
- all referenced root and location epochs are current;
- the candidate worker is supervised by that incarnation; and
- capability, grant, deny, health, protocol, and capacity checks pass.

Owner equality is a hard predicate evaluated again during atomic lease
acquisition. Scoring never compensates for a false owner predicate.

The access plan contains the selected identifiers and `owner_local` mode. The
agent rechecks ownership and epochs, resolves relative locators, and then
renders node-local child-worker protocol paths. Child workers never receive
credentials for the control-plane database or a different node.

No access plan has a transfer source/destination in this epic. A ticket needing
roots with different owners is unsupported and fails before dispatch.

## Terminal operation and claim liveness

One workflow lease covers all operation work:

1. owner-local worker dispatch;
2. result collection;
3. result-shape and expected-fact validation;
4. post-dispatch hash/probe or explicit verification;
5. safety-gate preparation and commit-intent authorization;
6. owner-node promotion and receipt reconciliation;
7. durable result/event finalization; and
8. terminal lease transition.

During healthy execution, the heartbeat wrapper owns this complete future.
Lease heartbeat transactions renew operation-specific claims. A heartbeat
failure still triggers the existing watchdog and fail-closed workflow path;
neither the lease nor a claim may be resurrected after genuine expiry. If a
commit may have started, lease failure leaves its independent intent fence in
`recovery_required` until receipt reconciliation. Chaos heartbeat suppression
must remain effective around the complete operation, not only worker dispatch.

Issue #415 delivers this invariant before distributed commit work. Commit
intent fencing handles uncertainty after a remote mutation may have started; it
must not be used to tolerate expired claims during ordinary healthy work.

## Commit protocol and recovery

ADR 0050 is the sole normative commit state machine and failure table. Issue
#422 extends the existing `commit_intents` lifecycle rather than creating a
parallel model:

- control-plane state remains `pending`, `authorized`, `completed`, `aborted`,
  or `recovery_required`;
- the owner journal records `not_started`, `applying`, `applied`,
  `outcome_unknown`, or `cancelled` for one intent generation;
- authorization is the one transaction that recomputes safety and activates the
  fence while the lease is live;
- the agent syncs `applying` after permission and before byte mutation; and
- finalization verifies the receipt and target facts before catalog state and
  the workflow lease become terminal.

Code and tests must import those names from their owning domain types rather
than duplicate string vocabularies. An authorized generation never becomes
retryable based on time or `not_started` alone; recovery must prove that no
process can still apply it.

## Compatibility, migration, and rollback

Implementation issues will introduce migrations and protocol changes in
dependency order. The replacement must:

- add root ownership and provider-relative location truth before consumers use
  it;
- backfill or explicitly re-observe current local roots under their owner;
- switch readers and writers together;
- remove global-path/prefix and control-plane byte operations; and
- bump exact protocols whenever a durable request/response shape changes.

No old-path fallback remains after cutover. Until a concrete migration is
designed, its issue must specify the backfill, preconditions, failure recovery,
and database-plus-binary rollback boundary.

## Security and observability

- Remote agent traffic requires authenticated encrypted transport.
- Agent authorization is node-scoped; a node cannot activate or resolve another
  node's root.
- Provider credentials remain agent-local and are never sent to child workers
  that do not need them.
- Node-local child endpoints do not bind remotely.
- Logs and debug output redact credentials and must not turn root locators into
  authentication material.
- Root activation, scan progress, owner-gate rejection, commit state, and
  recovery evidence are inspectable durable facts.
- Events cannot activate leases, ownership, sessions, or commit fences.

## Delivery map

| Issue | Architectural slice |
|---|---|
| #415 | Full-operation lease and claim liveness |
| #416 | Authenticated encrypted control-plane server |
| #417 | Pull-based node agent and incarnation supervision |
| #418 | Owned roots and provider-relative locations |
| #419 | Scan sessions and idempotent reconciliation |
| #420 | Owner hard gate and owner-local access plans |
| #421 | Owner-local scan, hash, and probe |
| #422 | Fenced node-local verification and commit |
| #423 | Node-local media worker routing |
| #424 | Owner-scoped tool readiness |
| #425 | Hermetic two-host acceptance proof |

The original roadmap relationship is:

- Sprints 7–9 supply node identity, leases, access plans, and scheduling;
- Sprint 10's real ingest moves behind the owner agent; and
- Sprint 18 supplies scan-session/reconciliation semantics, excluding watchers.

## Non-goals

- peer-to-peer, replicated, or federated control planes;
- cross-node transfer or cache management;
- S3/object-store implementation, archival upload, restore, credentials, or
  lifecycle policy;
- root ownership transfer or multiple owners;
- filesystem watchers and debounce loops;
- general background scheduling or Web UI work; and
- new video-transcode behavior.

The root/location/receipt abstractions must not prevent later object-store
archive work, but no field or branch may pretend that work exists.

## Verification strategy

Each implementation issue owns focused red/green tests for its boundary.
Campaign acceptance additionally requires:

- property tests for locator normalization and traversal rejection;
- real-time liveness tests for each post-dispatch operation path;
- stale epoch and non-owner authorization tests;
- scan replay, drift, partial-session, and completion-atomicity tests;
- commit lost-response, crash, replay, journal failure/loss, stale-fence, and
  ambiguity tests;
- protocol/conformance tests for every changed wire contract;
- a two-host policy covering audio synthesis/transcode, subtitle or track
  selection, remux, explicit verification, and add-only commit; and
- `just ci` with zero warnings.

The two-host test does not require video transcoding or file transfer. Its key
assertion is that the control-plane process cannot access the roots at the OS
boundary while the complete workflow succeeds.
