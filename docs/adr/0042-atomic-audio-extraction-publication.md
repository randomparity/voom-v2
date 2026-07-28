---
status: accepted
date: 2026-07-26
deciders: [VOOM core]
---

# 0042 — Publish audio extraction outputs as one recoverable operation

## Context

ADR 0041 gives every matched source audio stream a stable, ordered extraction
output descriptor and requires the worker to return the complete correlated
result set. The control plane still stages, verifies, commits, registers, and
reports only the first output.

Extraction publishes multiple filesystem objects and multiple relational rows.
SQLite cannot atomically commit filesystem links. Publishing each sidecar
independently would expose partial bundles and make a retry duplicate already
published outputs. The source stream also has no source artifact handle, so the
generic `artifact_lineage` parent/child-handle relation cannot express the
required file-version and snapshot-stream lineage.

Design:
[`docs/superpowers/specs/2026-07-26-issue-337-atomic-audio-sidecars-design.md`](../superpowers/specs/2026-07-26-issue-337-atomic-audio-sidecars-design.md).

## Decision

### Persist an operation and ordered outputs

Add an audio-extraction operation table and an ordered output table. One
descriptor-bearing operation is addressed by the planned operation ID, source
file version, and complete ordered normalized target-path set. Historical
singleton payloads use a domain-separated semantic key over source file
version, pinned media snapshot, selected snapshot stream ID/provider index,
codec, container, role, and normalized target path.
The legacy key therefore survives the fresh job/ticket identities created by
resume (ADR 0009) while distinct target locations remain distinct operations.
Two otherwise identical legacy executions for the same add-only target
intentionally coalesce: the old contract cannot publish two different
artifacts at one path. Each
output row records its ordinal, stable output ID (or the legacy singleton key),
source snapshot stream ID/provider index, role, paths, observed facts,
pre-commit probe payload/worker attribution, artifact/verification/commit
references, and committed result references.

The operation and output rows are the idempotency ledger. Retrying a committed
operation returns the recorded ordered result. Retrying a staged, prepared, or
recovery-required operation resumes it instead of creating new artifacts or
commit records.

On every legacy-singleton ledger miss, the host first checks the canonical
target and its pre-0042 owner before creating an operation or dispatching. The
existing target-owner uniqueness constraint permits at most one active or
committed artifact commit. If neither an owner nor target bytes exist, normal
new execution may begin. The host adopts only a committed owner when all
durable evidence agrees: source file
version, extraction source-lineage JSON and selected snapshot stream,
successful verification, target and staging paths, expected and observed
bytes, result file/version/location, and source-bundle membership/role. It
validates and probes the exact committed result, reusing an existing result
media snapshot when present. One transaction then inserts the committed
operation/output, any missing result media snapshot metadata, and normalized
stream-lineage row around the existing file/artifact/commit/bundle identities.
No extraction worker runs and no replacement file, artifact, commit, or bundle
identity is created.

`audio_extract_operation_outputs.commit_record_id` is nullable before prepare
and has `UNIQUE(commit_record_id)` when present. Legacy adoption uses one
immediate transaction to insert the operation and bind the committed record,
so concurrent first adopters cannot both succeed. Historical extraction
lineage records only the stable selected stream, not the pinned snapshot.
Before first binding, the host therefore proves that the requested pinned
snapshot belongs to the source file version and contains that exact snapshot
stream ID/provider index with the requested extraction facts. If the unique
commit binding already exists, replay returns it only when operation key,
pinned snapshot, selected stream ID/index, descriptor, and target all match.
Any different key, snapshot, stream, descriptor, or target for that commit is a
visible adoption conflict naming both operation keys and the commit ID.

A legacy staged handle with no target-bound commit cannot be proven to own the
current target. An uncommitted target owner, missing, malformed, conflicting,
or multiple candidate evidence, an owner whose target is missing, or occupied
target bytes with no matching owner is not adopted: execution fails with the
candidate IDs and corrective operator action before creating a new operation
or touching files. Thus the upgrade path is automatic for uniquely provable
committed state, including commit-before-snapshot, success-event, or ticket
result, and deliberately fail-closed for uncommitted, unbound, missing, or
ambiguous state.

A planned operation is protected by a durable, expiring host-writer claim tied
to the workflow lease. Claim acquisition and renewal use compare-and-swap
predicates on operation state, claim token, expiry, and dispatch generation.
Every cleanup, dispatch completion, validation, staged verification write,
prepare, promotion, owned observed-failure transition, recovery, and finalize
requires the same live claim and expected generation. A stale completion cannot
mutate the ledger or filesystem publication state.

Each dispatch generation writes to a distinct private attempt directory. This
prevents a late prior-generation worker from racing the current generation's
files. Only a claimant whose token and generation still match may bind one
complete validated attempt's immutable leaves as the operation's durable
staging paths in the `planned -> staged` transaction. Stale completions never
touch bound staging or target paths.

Before a worker request can start, the claimant commits an active dispatch row
containing generation, worker ID/epoch, idempotency key, and canonical attempt
paths. A crash on either side of send therefore leaves either no active
dispatch or complete replay/cleanup evidence. Stale attempt directories are
recorded with dispatch status as cleanup evidence. Elapsed TTL or a database-only
worker retirement is not proof that a process stopped writing. Automatic
recovery first replays the same idempotency key to the same live worker epoch;
a cached terminal response proves quiescence only because the extraction worker
contract requires every provider child and output writer to exit before a
terminal response. When that is unavailable, recovery reports the exact worker epoch and paths
and stays blocked until the operator stops/isolates that process and records an
explicit quiescence acknowledgement. Without proof the operation creates no
competing generation. After proof, cleanup removes only the recorded leaves
following path-containment and file-type checks, then advances the durable
generation and worker idempotency key. This recovers a host crash after worker
completion through idempotent replay without changing published
operation/output identity.

The operation table has one semantic uniqueness constraint: the complete
`operation_key` derived above. It has no narrower unique constraint on
`(source_file_version_id, operation_id)`, because the same plan/source may
legitimately execute to a distinct complete target set. Resume recomputes the
key and compares every ordered descriptor and target path before accepting the
ledger.

### Make relational publication atomic

The host validates the complete worker result before recording staged
artifacts, then verifies every staged member before prepare. Prepare creates
all pending artifact commit records and binds them to the operation in one
SQLite transaction.

Immediately before each add-only install the claimant renews and rechecks its
claim/generation. A claim can expire after that check but before the filesystem
call; add-only no-replace installation is the final fence. Competing claimants
can only install the same ledger-verified expected bytes. One link wins; the
other observes the occupied target and accepts it only after exact size/hash
validation. A mismatch fails closed and is never overwritten. After install,
the actor rechecks its claim and stops on loss; the exact target remains
recovery evidence for the successor. No relational sidecar identity is visible
yet. After every target has exact expected facts, one SQLite transaction whose
update predicate includes the live claim/generation:

1. creates every sidecar file asset/version/location;
2. records every pre-probed media snapshot against its result file version;
3. adds every bundle member;
4. records one source-file-version/source-stream lineage row per output;
5. commits every artifact commit record; and
6. marks the extraction operation committed.

The transaction either publishes the complete ordered set or publishes none.

Publication rejects symlink and non-regular target leaves. Every add-only
install and recovery comparison uses the exact persisted size and checksum;
those facts, the staging path, target path, and local file key are also durable
audit evidence. An occupied target is accepted only when its bytes match.

This decision does not claim host ownership, mode, device, inode, or link-count
fencing. Stronger parent/leaf identity checks and protection from a malicious
same-account process belong to the following execution-safety campaign, not
#337.

### Recover partial filesystem promotion

A failure or crash after prepare may leave zero, some, or all target bytes
installed. `prepared` is reserved for an unobservable crash or claim loss that
prevented the former claimant from writing. On any owned promotion/finalize
error, one transaction marks the operation and every still-active member commit record
`recovery_required` with shared operation failure identity and per-member path
evidence. No sidecar identities, memberships, snapshots, or lineage rows exist.

A claimant that discovers its claim is lost performs no write. The successor,
after acquiring the claim, sees the prior claim/generation on a `prepared`
operation and atomically records claim-loss recovery evidence for the operation
and every active member before it resumes. A stale token is never authorized to
transition state.

Recovery acquires the operation claim, re-reads either the prepared or
recovery-required ledger, and rechecks the source commit-safety gate. It
asserts the exact live claim once before the first filesystem mutation and
again after every promoted member. Each post-member assertion fences the next
promotion or finalization. For each output in order it:

- accepts an existing target only when its size and content hash exactly match;
- promotes from the verified staging file only when the target is absent; and
- fails closed on a symlink, stat error, or mismatched collision.

After all targets match, recovery runs the same all-output finalize
transaction. A crash before or during finalize therefore resumes safely; a
crash after finalize returns the already committed report.

### Record stream lineage explicitly

Add a dedicated extraction lineage table populated only by the finalize
transaction. Each row uniquely joins one operation output and result file
version to the source file version, pinned media snapshot, source snapshot
stream ID, and provider stream index. `file_versions.produced_from_version_id`
continues to record the coarser file-version edge.

### Evolve reports additively

Execution reports and audio extraction events gain an ordered `outputs` list.
Each success item includes the stable output descriptor, paths, artifact and
verification identities, commit identity, result file asset/version/location,
result media snapshot/facts, bundle member/role, and lineage relationship.
Historical singular fields stay as the first-output projection so existing
singleton data remains readable.

Compliance planning already exposes the descriptor list in each extraction
check's `desired_state`. Post-run compliance data additionally includes the
ordered extraction results from succeeded ticket results. Artifact promotion
reads every result location from the list, with the historical scalar as the
legacy fallback.

## Consequences

- A malformed, short, extra, or reordered worker result cannot create staged
  artifact rows or target bytes. Operation-owned partial staging leaves are
  removed deterministically before redispatch.
- A probe or verifier failure on any member prevents prepare and promotion for
  all members.
- The filesystem can temporarily contain unregistered exact target bytes after
  a crash. This is recovery evidence, not a partially published bundle.
- Retry and recovery reuse the operation ledger and cannot duplicate artifact
  handles, commit records, bundle memberships, or lineage rows.
- A uniquely proven pre-0042 singleton commit is lazily adopted without
  republishing media; incomplete or ambiguous legacy evidence fails visibly.
- Competing or late executors cannot stage an obsolete generation; attempt
  paths isolate worker writes and the operation claim fences host mutations.
- The dedicated tables and recovery state machine add schema and code, but keep
  the generic single-artifact commit contract unchanged.
- Rollback follows ADR 0013's binary-before-database ordering. The migration is
  additive; historical rows need no eager migration backfill because the
  singleton execution path performs strict lazy adoption when needed.

## Considered and rejected alternatives

### Commit every output independently

Rejected. A later failure exposes a partial bundle and makes retry ownership
ambiguous.

### Treat multiple filesystem links as atomic

Rejected. The filesystem provides no portable multi-path transaction.
Relational visibility must remain closed until every link is exact.

### Store the group only in artifact commit JSON reports

Rejected. Recovery and idempotency require indexed uniqueness and foreign keys,
not untyped JSON scans.

### Use `artifact_lineage`

Rejected. The source file version and snapshot stream are not represented by a
source artifact handle. Inventing one would misstate identity and duplicate the
media model.

### Delete already promoted targets on failure

Rejected. A crash can occur before cleanup and deletion may destroy evidence or
race an uncertain durable state. Exact-byte recovery is deterministic and
add-only.

## Governing deferral

ADR 0019 accepts the small window in which a blocking use lease can appear
after the prepare-time gate check and before additive filesystem promotion.
This change rechecks the same gate on recovery and does not broaden that
accepted window.
