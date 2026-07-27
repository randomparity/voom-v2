# Issue #337: Atomic audio sidecar publication design

## Goal

Consume ADR 0041's ordered extraction descriptors and publish every produced
sidecar as one deterministic, atomic, lineage-complete, and recoverable
operation. A completed operation must be fully inspectable from its ticket
result and compliance run report.

## Scope

This change owns:

- plural selection, path, request, result, staging, and verification assembly;
- operation/output persistence and idempotent resume;
- all-output prepare, add-only promotion, finalize, and recovery;
- explicit source file-version and source-stream lineage;
- ordered execution/event/compliance report data; and
- generated-media and durable failure-boundary tests.

It does not add DSL forms, implement synthesis (#333), change the #99 output ID
algorithm, or absorb #334, #343-#344, #346, #351-#353, #358-#359, #364, or
#338-#339. A newly found independent gap belongs under #325.

## Existing contracts

- ADR 0041 fixes descriptor identity, provider-index ordering, collision-safe
  `name_suffix`, and request/result list correlation.
- Historical extraction payloads omit `operation_id` and `outputs` and may
  execute only when exactly one stream matches.
- The worker validates the whole request before FFmpeg and returns no shortened
  success result.
- Artifact commit records own one handle and one target. Active uniqueness
  constraints already prevent a second owner for either.
- A sidecar file version has `produced_by = staged_commit` and
  `produced_from_version_id = source_file_version_id`.
- Events are audit facts. Tickets, operation/output rows, artifact rows,
  identity rows, bundle rows, and lineage rows are authoritative.

## Operation identity and states

`audio_extract_operations` contains:

- numeric database ID;
- non-empty stable operation key;
- optional published plan operation ID;
- source file version, source bundle, and pinned media snapshot;
- state: `planned`, `staged`, `prepared`, `recovery_required`, or `committed`;
- recovery failure fields; and
- a monotonically increasing dispatch generation; and
- an optional claim lease/token and claim expiry; and
- created/finished timestamps.

For a descriptor-bearing plan, the key is a domain-separated hash of the
published operation ID, source file version, and complete ordered set of
normalized target paths. For a historical payload it is a domain-separated
semantic hash of source file version, pinned media snapshot, selected snapshot
stream ID/provider index, target codec/container, bundle role, and normalized
target path. It does not contain job, ticket, lease, or attempt identity, so
ADR 0009 resume finds the same incomplete operation. Two requests that differ
by any target path or by target order are distinct. Repeating the same planned
operation, source, and ordered target set coalesces. Two otherwise identical
historical requests for the same add-only target also intentionally coalesce
because the legacy contract cannot own two different artifacts at one path;
the second execution was already a target collision before this change.

`operation_key` is the operation table's sole semantic uniqueness constraint.
The schema deliberately has no narrower unique index on
`(source_file_version_id, operation_id)`: the same published plan operation and
source may legitimately execute to a different complete target set. On resume,
the host recomputes the key and compares every ordered target path and
descriptor byte-for-byte before accepting the ledger. The key is internal;
reports expose the published operation ID and output IDs.

### Legacy singleton adoption

Before creating a legacy-singleton ledger row, inspect the canonical target and
query its unique active or committed artifact-commit owner. Only an absent
target with no owner proceeds to a new operation. Adopt an existing owner only
when it is committed and all evidence is complete and agrees with the
recomputed semantic key:

- commit source file version and canonical target/temp paths;
- artifact-handle extraction lineage source version/location, selected snapshot
  stream, and intended bundle role;
- successful verification and exact staging/target size and checksum;
- committed result file asset/version/location and target location, when
  committed; and
- exactly one membership in the requested source bundle with the intended
  role.

Probe the exact committed target before adoption, or reuse and revalidate its
existing result media snapshot. One transaction creates a committed
operation/output around the existing file/artifact/commit/bundle identities,
records any missing result media snapshot metadata and normalized stream
lineage, and returns the reconstructed report. The extraction worker does not
run and adoption never creates replacement file, artifact, commit, or bundle
identities.

Historical staged lineage names the stable selected stream but not its pinned
snapshot. Before the immediate adoption transaction, require the requested
pinned snapshot to belong to the source file version and contain exactly that
snapshot stream ID/provider index with the requested extraction facts. The
transaction is the first-writer binding of the uniquely constrained commit
record. On a uniqueness conflict, load the existing operation/output and return
it only when its operation key, pinned snapshot, stream ID/index, descriptor,
and target match byte-for-byte. Otherwise fail with both operation keys and the
commit record ID. A different snapshot or stream cannot reinterpret an adopted
commit.

A staged-only legacy artifact has no target-bound commit and cannot prove it
owns the requested target. A pending/recovery-required target owner, no
committed owner for occupied bytes, multiple candidates, malformed
lineage/report JSON, inconsistent facts, a committed owner whose target is
missing, or staged-only evidence fails with an actionable list of candidate IDs
before operation creation or filesystem mutation. There is no best-effort
match or adoption of an uncommitted legacy operation.

State transitions are monotonic except that recovery changes
`recovery_required -> committed`:

```text
planned -> staged -> prepared -> committed
                         \-> recovery_required -> committed
```

No code path replaces an existing operation or output row.

`audio_extract_operation_outputs` contains one row per canonical ordinal:

- operation row and zero-based ordinal;
- stable output ID, source snapshot stream ID/index, and bundle role;
- staging, target, and durable temp paths;
- observed size/checksum and optional local file key;
- artifact handle/location, verification, and commit record IDs;
- result file asset/version/location and bundle-member IDs; and
- pre-commit probe worker/payload, result media-snapshot ID, and
  provider/container/codec/language/title facts needed to rebuild the report.

For a historical singleton the host creates a deterministic internal output key
from the legacy operation key and marks the published `output_id` absent in the
report. New operations require the ADR 0041 output ID.

Unique constraints cover `(operation, ordinal)`, `(operation, output_id)`,
target path, artifact handle, nullable `commit_record_id`, result file version,
and bundle member. `UNIQUE(commit_record_id)` is the schema fence that prevents
two outputs, including concurrent legacy adopters, from wrapping one artifact
commit. The host also checks descriptor bytes against a loaded ledger before
resuming.

`audio_extract_dispatch_attempts` contains one row per generation with worker
ID/epoch, idempotency key, exact attempt directory/leaves, status
(`active`, `terminal`, `quarantined`, `quiesced`), and terminal or explicit
quiescence evidence. The active attempt row and every intended leaf are
committed before the request is sent. A terminal response means the request
handler, all provider child processes, and every output writer/file descriptor
for the attempt have exited. The evidence is the authority for cleanup, not
elapsed wall-clock time.

## Durable lineage

`audio_extract_output_lineage` is inserted only at finalization and contains:

- operation-output row ID (unique);
- source file version;
- source media snapshot;
- source snapshot stream ID and provider stream index;
- result file version (unique); and
- recorded timestamp.

The source columns must match the operation/output ledger. Repository finalize
constructs them from typed inputs in the same transaction; callers cannot
insert lineage independently. This row is the stream-level relationship.
`file_versions.produced_from_version_id` remains the file-level relationship.

## Execution flow

### Resolve and resume

1. Select and byte-revalidate the source and pinned media snapshot.
2. Resolve every extraction descriptor against the snapshot. The selection
   type is a non-empty ordered vector; no later layer silently chooses index 0.
3. Derive every target path from the source stem plus the planned `name_suffix`.
   Derive generation-specific staging paths beneath the private operation
   directory. Normalize and reject duplicate/case-folded paths as a set.
4. Load or create the operation ledger. A committed ledger returns its recorded
   report. A prepared/recovery-required ledger enters recovery. A staged ledger
   resumes verification. A planned ledger dispatches. On a legacy singleton
   miss, run strict adoption before creating the planned row.

Before mutating a non-committed ledger, the executor acquires a durable writer
claim using one compare-and-swap update over the expected state/generation and
an absent, expired, or same-lease claim. The claim token includes the workflow
lease and an unguessable nonce; it expires and is renewed alongside lease
heartbeats. A competing live claimant fails without touching paths. Every
cleanup, generation increment, result acceptance, staged transition, prepare,
verification persistence, each promotion, observed-failure transition,
recovery, and finalize predicates on the claim token and expected generation.
The claim remains required through every non-committed state.

A `planned` row may coexist with generation-owned staging leaves when the worker
wrote member 0 and failed, returned a malformed result, or the host crashed
after the worker completed but before the staged transaction committed. Before
each redispatch the host:

1. canonicalizes and mode-checks the private operation/attempt directories;
2. resolves every prior-generation staging leaf and proves it is an immediate
   child of its recorded attempt directory;
3. requires a cached terminal response or explicit operator quiescence
   acknowledgement after the named worker epoch has been stopped/isolated;
4. removes only regular-file leaves from that exact ledger;
5. treats absent leaves as already clean;
6. rejects symlinks, directories, device nodes, and cleanup/stat errors; and
7. increments `dispatch_generation` with the live-claim CAS.

It never removes a target or commit temp path in planned-state reconciliation.
Each generation uses distinct attempt paths, so a late old worker cannot write
the current generation's leaves. A completion may enter validation/staging only
when its generation and claim still match; stale completions are discarded and
their attempt directory is recorded as cleanup evidence.
The worker idempotency key is
`audio-extract:<operation-key>:<dispatch-generation>`. Advancing the generation
after cleanup avoids replaying a cached success whose files were deliberately
removed. Published operation/output identities do not include the generation.

The active worker writes only generation attempt paths. After the complete
result and every attempt leaf validate, the host renews the claim and uses one
`state = planned AND generation = ? AND claim_token = ?` transaction to bind
that generation's immutable attempt leaves as the operation outputs' durable
staging paths and mark the operation staged. There is no pre-bound stable leaf
that a stale worker can write. Once staged, those paths never change.

An obsolete generation completion can observe or validate only its own attempt
directory. It cannot bind rows, create artifact handles, touch the current
generation's bound staging leaves, or touch target/temp paths. Its directory is
cleaned only after the remote dispatch reports terminal/cancelled completion or
the process supervisor observes exit. A database worker-retirement row or TTL
alone quarantines the attempt and blocks redispatch; neither authorizes
deletion. Automatic recovery replays the same idempotency key to the recorded
live worker epoch, allowing its cached terminal response to establish
quiescence after a host crash. If replay/process supervision cannot prove it,
the actionable recovery report names the worker ID/epoch, idempotency key, and
exact attempt paths. After the operator stops or isolates that worker process,
an explicit acknowledgement records quiescence and unlocks cleanup. Cleanup
first reacquires the operation claim. An obsolete generation must be unbound.
The current generation may be cleaned only when the operation is still
`planned`, the exact attempt is positively quiesced, every persisted leaf is
unbound, and no other active attempt exists. Cleanup then canonicalizes the
exact directory, rejects symlinks/non-regular leaves, removes only persisted
attempt leaves, and advances the current generation in the same
claim-predicated transition. A staged/bound generation is never
cleanup-eligible. Thus cleanup never races a worker still allowed to write and
an unquiesced attempt cannot coexist with a new writer generation.

### Dispatch and validate

5. Build one plural request whose descriptors and paths exactly match the
   ordered selection.
6. Commit the dispatch-attempt row, worker epoch, idempotency key, and exact
   intended attempt leaves before sending the request. Dispatch using the
   operation key plus the current durable dispatch generation. Mark it terminal
   only when the worker contract proves the handler, every provider child, and
   every output writer/file descriptor has exited.
7. Validate the complete result through
   `validate_extract_audio_result`, then validate source pre/post facts,
   language/title preservation for each output, and each staged file's
   size/hash.

Missing, extra, duplicated, reordered, or projection-inconsistent results fail
here. No artifact, verification, commit, bundle, lineage, or target row is
created. The operation stays `planned`; its exact owned leaves are cleaned
before the next generation dispatch. A crash before send leaves a durable
attempt with no terminal evidence; replaying that same idempotency key
establishes whether it ran. A crash after send is handled identically, so
recovery never infers dispatch or quiescence from missing database evidence.

### Stage and verify

8. Probe every staged output with ffprobe before commit. Validate the observed
   size/hash and require exactly one expected Opus audio stream with the
   published codec, channels, language, title, and disposition facts. Keep each
   normalized probe payload and worker attribution.
9. In one transaction create one artifact handle and staging location per
   result, record their source lineage JSON, bind them to ordered output rows,
   persist every pre-commit probe payload/worker attribution, append staged
   events, and mark the operation `staged`.
10. Verify every handle/path. Persisted successful verifications are reused on
   retry. If any verification fails, return an error and do not prepare or
   promote any member.

The staging lineage JSON includes operation/output IDs, source file
version/location, pinned media snapshot, source snapshot stream ID/provider
index, and intended role. It is evidence; the normalized lineage table remains
the durable relational contract.

### Prepare and promote

11. Recheck the source lineage commit-safety gate in one transaction.
12. In that transaction create one pending artifact commit record per output,
    bind every record to its output row, append started events, and mark the
    operation `prepared`.
13. For each output in order:
    - re-observe staged facts;
    - copy to the recorded temp sibling;
    - install add-only;
    - fsync;
    - re-observe the target; and
    - require exact size/hash.

Before every temp copy/install, renew and recheck the operation claim and
generation. Claim loss stops the losing executor immediately; it is fenced from
all later database writes. The successor that acquires the claim inventories
the persisted member states and, before promotion or finalize, runs one failure
transaction that changes the operation and every still-active commit record to
`recovery_required`, carrying one operation failure identity plus per-output
target/temp/staging evidence. An executor that still owns the claim performs
the same transaction for an observed promotion or finalize error. `prepared`
remains only when a crash or claim loss prevented the current executor from
recording the transition. Zero, some, or all targets may exist. No sidecar file
asset, version, location, media snapshot, membership, or normalized lineage row
exists.

The claim check cannot be atomic with a filesystem link. Therefore the install
primitive remains add-only/no-replace and all actors use the same persisted
expected facts. If a claim expires just after the check, at most one link
succeeds. A successor treats `AlreadyExists` as recovery evidence only after
exact size/hash validation; mismatched bytes are a conflict. The former
claimant rechecks after install and performs no further mutation after claim
loss.

Publication rejects symlink and non-regular target leaves. Add-only install and
recovery accept an occupied target only when its size and checksum match the
persisted expected facts. The ledger records those facts plus the staging path,
target path, and local file key.

Host ownership, permission mode, device, inode, link-count, and stronger parent
identity fencing are deliberately outside #337. They belong to the following
execution-safety campaign before the full published grammar corpus runs.

### Finalize

14. In one SQLite transaction, for every output in order:
    - create the sidecar identity rows through the existing verified-sidecar
      rules;
    - insert the persisted pre-commit probe payload as that result file
      version's media snapshot, preserving probe-worker attribution;
    - add it to the source bundle in its planned role;
    - insert its stream-lineage row;
    - mark its commit record committed; and
    - update the output's result references.
15. Mark the operation committed and append per-output commit events plus one
    complete extraction success event.
16. Return the ordered report.

A database failure rolls back all steps. No partial bundle or lineage set can
be observed.

## Recovery and retry

Recovery claims a persisted `prepared` or `recovery_required` operation. It
reloads every bound artifact, successful verification, commit record, path, and
expected fact and rechecks the commit-safety gate. The claim/generation is
renewed and checked immediately before every missing-target promotion and in
the all-output finalize transaction.

For each output:

- absent target: promote from the still-live verified staging path;
- exact target: accept as already promoted;
- mismatched target, symlink, permission/stat error, missing or drifted staging
  needed for promotion: fail closed and keep recovery state.

When every target is exact, recovery calls the same finalize transaction.
Unique constraints and state predicates make re-entry safe:

- crash after durable prepare: promotion resumes from zero targets;
- claim expires after prepare or between member installs: the stale executor
  stops without writing; the successor records recovery-required for the
  operation and every active member before resuming;
- crash after member N: exact targets `0..N` are accepted, remaining targets
  are promoted;
- crash after all promotions: finalize runs;
- crash during finalize: SQLite rolls back or committed state is returned;
- retry after finalize: no worker, filesystem, artifact, membership, or lineage
  mutation occurs.

A fresh job/ticket created by resume resolves the same descriptor-bearing or
legacy semantic operation key before claiming it. It cannot create a second
ledger merely because ticket identity changed.

## Reports and compatibility

`ExecuteExtractAudioReport` keeps all existing singular fields as a first-item
projection and adds:

- published `operation_id`;
- `outputs: Vec<ExecuteExtractAudioOutputReport>`; and
- operation-level recovery details containing every member.

Each output report exposes:

- ordinal and published output ID;
- source file version, media snapshot, stream ID/index;
- staging/target locations;
- artifact handle/location and verification;
- commit record;
- result file asset/version/location;
- result media snapshot ID and produced container/stream facts;
- source bundle, bundle-member ID/role;
- lineage row ID and both ends of the relationship; and
- output container, codec, language, title, provider, and provider version.

New success events add the same ordered output payload and keep singular fields
as the first projection. Started/failed events add optional ordered member
context. Additive fields use `#[serde(default)]` and omit empty values so
historical event payloads deserialize under ADR 0013.

Compliance preview and phase reports already copy the plan payload into
`desired_state`, including ordered descriptors. Execute and `report --job-id`
add ordered extracted-output views loaded from the succeeded tickets named by
each file-phase summary. Ticket results remain the durable execution report.
The terminal artifact promoter reads all
`outputs[*].result_file_location_id` values and falls back to the historical
top-level scalar.

Historical compiled policies and legacy singleton payloads remain readable.
Legacy singleton reports retain their existing scalar values. No grammar,
parser, or compiled-policy shape changes.

## Failure matrix

| Boundary | Target bytes | Published identities/bundle/lineage | Resume |
|---|---:|---:|---|
| malformed/short/extra/reordered result | none | none | redispatch |
| worker writes member 0 then fails | none | none | clean owned leaves, next generation |
| crash after worker success before staged tx | none | none | replay same key, bind same generation |
| crash before/after dispatch send | none | none | replay persisted attempt/key |
| competing resume / late prior generation | none | none | CAS rejects writer/stale completion |
| probe/verifier failure on member N | none | none | redispatch or reuse verified stages |
| crash after prepare | none | none | promote all |
| crash after promotion member N | exact prefix may exist | none | validate prefix, promote rest |
| claim loss after install | exact prefix may exist | none | successor records recovery, resumes |
| target size/hash drift or symlink | conflicting bytes/evidence | none | fail closed |
| crash after all promotions | all may exist | none | finalize all |
| SQLite finalize failure | all may exist | none | retry finalize |
| retry after committed | all | all | return recorded report |

## Test strategy

Behavior tests must fail before implementation and inspect durable state, not
only request bytes or exit codes.

- Generated Ogg/Opus media: one match, two ordered matches, two sanitized-name
  collisions, and source facts including language/title.
- Complete plural execution: inspect every produced file with ffprobe, then
  assert ordered report items, file assets/versions/locations, bundle members,
  per-output media snapshots/facts, `produced_from_version_id`, normalized
  stream-lineage rows, and compliance run output.
- Worker result failures: missing, extra, reordered, duplicated, and malformed
  projections; assert no target bytes or operation staging/commit identities.
- Planned-state failpoints: when a worker writes member 0 then returns a
  terminal failure, retry deletes only the exact owned staging leaves, advances
  the dispatch generation, and redispatches. When the host crashes after
  complete worker output but before staged persistence, retry replays the same
  key, obtains the cached terminal result, validates the existing leaves, and
  binds that same generation. Both keep the same operation/output IDs.
- Dispatch failpoints: crash immediately before and immediately after request
  send. Both recover through the committed attempt row and same idempotency
  key. A provider child or output file descriptor that outlives the request
  handler prevents terminal evidence and therefore blocks cleanup/redispatch.
- Concurrency tests: two executors race to resume one operation; only one claim
  wins. A delayed generation-N result arrives after generation N+1 is claimed
  and staged; it cannot bind rows, delete, stage, or overwrite N+1 paths. Its
  orphan attempt bytes remain evidence until positive quiescence. A worker that
  outlives TTL blocks cleanup/redispatch until terminal,
  or an explicit post-isolation operator acknowledgement.
- Quarantine test: without terminal/operator proof, retry cannot
  clean or create generation N+1. Recording proof after retiring and actually
  stopping the named worker epoch unlocks cleanup/redispatch without operation
  or output identity drift. Host crash after worker completion replays the same
  idempotency key and obtains the cached terminal result.
- Probe or verification failure on member N: assert all targets absent and no
  pending commits, result media snapshots, bundle members, or lineage rows.
- Commit failpoints: immediately after prepare, after installing member N, and
  after all installs before finalize. Assert no visible result identities,
  then recover and compare the exact ordered report/IDs with an uninterrupted
  run.
- Claim failpoints: expire/steal the claim after prepare and between member
  installs. Assert the stale executor performs no later mutation, every active
  commit is made recovery-required by the successor before it continues, and
  only the successor completes.
- Add-only race failpoint: expire the claim immediately after the pre-install
  check. Whether the old or new claimant links first, the target is the exact
  expected bytes, one actor finalizes, and neither overwrites a mismatch.
- Exact collision recovery: accept matching bytes and reject mismatched bytes
  without overwriting.
- Finalize drift: replace or mutate a target after install; changed size/hash
  or a symlink/non-regular leaf blocks relational publication.
- Operation identity: the same plan/source/ordered target set resumes one
  ledger, while changing any target or target order creates a distinct ledger
  without conflicting on a narrower plan/source uniqueness constraint.
- Adoption concurrency: race two first adopters of one legacy commit. The
  nullable unique commit binding admits one; an exact key/snapshot/stream replay
  returns it, while a different pinned snapshot or stream fails with both keys
  and the commit ID.
- Retry after finalize: assert unchanged counts for files, artifact handles,
  artifact locations, verifications, commit records, media snapshots, bundle
  members, lineage, and physical target paths.
- Compatibility: deserialize historical singleton operation/event/ticket
  result fixtures and execute a legacy one-match payload. Recreate the job and
  ticket as ADR 0009 resume does and prove it finds the same incomplete legacy
  operation/artifact/commit/target identities. Also prove identical legacy
  semantics at different normalized targets do not alias, while a repeated
  request for the same add-only target intentionally returns the same
  operation. Seed a pre-0042 committed singleton at the
  commit-before-ticket-result boundary and prove retry adopts its existing
  identities, adds only snapshot/ledger/lineage rows, and does not dispatch.
  Cover crashes after the old commit transaction but before result snapshot,
  success event, and ticket completion, plus a normal historical committed
  read. Seed pending/recovery-required and ambiguous/staged-only legacy
  evidence; prove each fails before mutation. Existing compiled policy versions
  still load and plan.

Focused crate tests and the full `just ci` gate must pass.
