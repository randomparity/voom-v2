# Policy-driven artifact verification

Issue: #334

## Outcome

An explicit published `verify artifact` node verifies the exact active bytes selected for its
file branch through the bundled out-of-process verifier. Successful and failed evidence is durable,
downstream phases cannot run after a failed verification, a resume does not repeat already-recorded
verification, and both `compliance execute` and `compliance report --job-id` expose the result.

## Review charter

The permitted surface is the policy-to-workflow binding, workflow dispatch/finalization/resume,
artifact target resolution and verification, artifact/workflow repositories, one additive
migration, compliance result views, tests, and governing documentation.

Direct dependencies are ADRs 0001, 0002, 0006–0010, 0013, 0017, 0038, 0039, 0045, the published
V1 grammar, the compiled `{"type":"verify_artifact"}` shape, the worker eligibility predicate, and
the existing bundled verifier protocol.

The following are excluded:

- No new policy syntax, aliases, arguments, or compiled-policy fields.
- No in-process verifier and no `worker run-local` kind for verification.
- No changes to the staged-artifact commit gate's existing verification semantics.
- No post-dispatch lineage lease; ADR 0045 defines ticket creation as the lineage linearization
  point.
- No work on #338, #339, #367, #368, or #369.

Any independent gap discovered outside this surface is filed as a native sub-issue of #325 with
`status:needs-triage`.

## Current gap

The compiler and planner already preserve the fieldless verify operation and emit a planned
`verify_artifact` node. The workflow bridge maps it to `OperationKind::VerifyArtifact`, but the
generic payload lacks selected file identity, the executor requires an HTTP runtime even though the
verifier is bundled-direct, and no adapter records a workflow result.

The artifact verifier currently accepts only a handle with exactly one live `staging` location.
That is correct for the commit gate but cannot address a committed artifact's active `local_path`
or an existing active file that has never passed through the staging pipeline.

Finally, the workflow summary vocabulary assumes planned success either advances the file chain
(`committed`) or did no work (`skipped`). Verification is successful work without mutation, so
neither term describes it. Failed evidence also cannot currently be correlated to its workflow
ticket for exact reporting or idempotent retry.

## Invariants

1. The source and compiled policy contracts remain exactly `verify artifact` and
   `{"type":"verify_artifact"}`.
2. A policy ticket pins `source_file_version_id` and, when selected, `source_location_id`; it never
   chooses a different chain tip during dispatch.
3. Artifact resolution accepts only the pinned active file version and one live local-path
   location. Missing, retired, mismatched, or superseded identity fails before worker launch.
4. A dependency-produced version reuses the artifact handle from its committed artifact record.
   An existing active version receives a stable artifact handle and local-path artifact location.
5. The bundled worker remains out of process. Its durable worker row, capability, grant, and deny
   checks remain authoritative.
6. Each workflow lease owns at most one verification evidence row. A retriable ticket may acquire a
   new lease and record a new attempt; a completed verified phase is not dispatched again on
   workflow resume.
7. Failed verification fails the workflow lease and therefore gates dependants. Its durable
   evidence row and terminal artifact event commit before the ticket failure is exposed.
8. Successful read-only work is recorded as `verified`, not `committed` or `skipped`.
9. Existing durable rows and existing serialized fields retain their meanings.

## Design

### Bind the exact selected file

Add `render_policy_verify_artifact_payload`, using the same `PolicyFileSource` contract as the
transform adapters. The root renderer accepts only `FileVersion` and `FileLocation` plan targets
and inserts the exact version and optional location IDs into the workflow ticket.

ADR 0045 still guards root-ticket creation against a superseded selected version. The verification
adapter repeats the active-version/location validation while resolving the artifact target so a
bad durable payload or retired location fails closed.

### Resolve an active file to an artifact

Add one store-owned, `BEGIN IMMEDIATE` resolution operation:

1. Validate that the pinned version exists, is the greatest live version for its asset, and has the
   selected live local-path location (or exactly one when no location was selected).
2. Look for a committed artifact record whose `result_file_version_id` is the pinned version.
3. Otherwise look for the canonical policy-verification handle already tied to that version.
4. If neither exists, create an immutable, active-durability artifact handle whose expected
   size/hash comes from the version.
5. Reuse or create the handle's live `local_path` artifact location for the exact selected path.
6. Emit handle/location creation events in the same transaction when rows are created.

The immediate transaction serializes competing policy resolvers, and the canonical-handle query is
repeated after the write lock is held. This avoids a new identity table and keeps older readers
compatible with the artifact catalog. A committed transform's handle is preferred because it is
the provenance-bearing identity of those output bytes.

### Generalize verification without weakening the commit gate

Keep public `ControlPlane::verify_artifact(VerifyArtifactInput)` unchanged. It continues selecting
exactly one live staging location and enforcing the caller-supplied staging root.

Extract an internal exact-target entry point used by policy execution. It receives the resolved
handle, artifact location, local-path containment root, selected worker ID, and workflow ticket ID.
Shared verification logic revalidates the exact location owner, kind, path, and liveness before
persisting the terminal result. The staging entry point additionally preserves its exactly-one-live-
staging check.

The policy path derives the containment root from the validated local path's parent. This gives the
worker the same traversal/symlink protections without pretending the active file is staged.

### Model bundled dispatch explicitly

Before planning execution, detect a planned verify node and ensure the built-in verify worker
inside a transaction. Existing denies, retirement, wrong kind, or node ownership still fail closed;
missing capability/grant rows are bootstrapped. The grant's concurrency key is
`verify_artifact`.

Worker selection and atomic lease acquisition continue through the shared store predicate. After
selection, the executor uses a `BundledVerify` dispatch variant for
`OperationKind::VerifyArtifact`; every other operation still requires a registered HTTP runtime.
No dummy client or fake endpoint enters the runtime registry.

The adapter invokes the existing `BundledVerifyArtifactDispatcher`, heartbeats the workflow lease
while it runs, persists evidence, then:

- releases the lease with the verification report on success; or
- fails the lease with the recorded failure class/error on verification failure.

### Correlate evidence to ticket attempts

Migration 0026 adds nullable `workflow_ticket_id` and `workflow_lease_id` columns to
`artifact_verifications`. The lease column has a unique partial index. Existing staged-commit rows
remain valid with `NULL` values.

Before launching a policy verification, the adapter reads evidence for its current lease. If found,
it replays that result into the lease. The unique index is the final concurrency guard for
re-entrancy of one attempt. A retriable ticket's next lease is a new attempt and may dispatch
again; failed attempt evidence is retained rather than overwritten.

Workflow resume idempotency is a separate reconciliation invariant. Ordinarily, a successful
`verified` row is carried into the replacement job, whose run start advances beyond that phase. The
coordinator also closes the crash window between evidence persistence and file-phase finalization:
when the prior job has no row for the next phase, resume looks for one successful
ticket-correlated verification for the same branch, phase, pinned version, and still-live
location. It verifies the artifact/evidence ownership and expected facts, seeds a `verified` row in
the replacement job, and advances its cursor without dispatch. Ambiguous or inconsistent evidence
fails resume visibly rather than guessing.

The nullable field is additive and ignored by older code. Existing verification report JSON and
artifact events are unchanged.

### Record read-only phase completion

Migration 0026 also rebuilds `workflow_file_phase_summaries` and
`workflow_file_run_history` to add the `verified` outcome while preserving all existing rows,
foreign keys, indexes, and checks. File-phase summaries gain a nullable
`artifact_verification_id`; the `verified` shape requires it and its artifact handle.

A successful verify ticket records a `verified` file-phase row with:

- the unchanged active file version, location, and snapshot;
- the resolved artifact handle and exact successful verification; and
- the ticket ID that owns the evidence.

`verified` satisfies `run_if ... completed` but not `run_if ... modified`. It counts as completed
in progress and phase rollups. Resume carries it as completed history, advances past the phase, and
therefore does not create a new ticket or verification row.

`committed`, `skipped`, and `blocked` keep their current wire values and meanings. The new value can
only appear for newly executable verify nodes, which older binaries never execute.

### Expose exact evidence

Add an `artifact_verifications` array to both compliance execute data and stored-run report data,
omitted when empty. Rows are selected by joining `artifact_verifications.workflow_ticket_id` to
the requested job's tickets, ordered by verification ID. Each view includes ticket, verification,
artifact handle/location, worker, status, expected and observed facts, and failure details.

The file-phase view exposes both artifact handle and verification IDs. The success envelope exposes
`verified` file phases and succeeded evidence. A failed execute's
partial data exposes the failed evidence even when abort semantics intentionally omit a completed
file-phase row. `compliance report --job-id` reconstructs the same view from durable state without
dispatching work.

For a resumed run, evidence loading includes exact verification IDs referenced by seeded
file-phase rows as well as evidence owned by the current job's tickets. This makes recovered
verification visible without pretending the prior ticket belongs to the replacement job.

## Failure and recovery

- Missing/ambiguous active locations, stale versions, or inconsistent committed-artifact
  provenance fail before worker launch.
- Worker launch, protocol, checksum, and file-availability failures persist failed verification
  evidence and artifact terminal events, then fail the workflow ticket.
- `on_error: abort` stops the phase and all downstream phases. `on_error: continue` records the
  branch blocked and excludes it from later phases.
- Retriable worker failures may use the ticket's normal new-lease retry budget; each lease records
  its own attempt. A failed workflow may also be resumed into a new job and ticket. Already
  successful verification phases are inherited and never repeated.
- If a process dies after successful evidence persistence but before phase finalization, resume
  validates and adopts that exact evidence into a seeded `verified` row. It does not launch the
  worker again.
- The migration is forward-only under the existing database version contract. Rollback uses the
  pre-upgrade compatible backup procedure verified by #351; it does not reinterpret new rows with
  an older binary.

## Verification

Tests will prove:

- published and compiled policy shapes are unchanged;
- policy payloads pin exact file identity;
- dependency-produced and pre-existing active artifacts resolve correctly;
- stale/retired/mismatched identity launches no worker;
- the selected store-eligible built-in worker is the dispatched worker;
- success persists one evidence row and the started/succeeded verification events;
- checksum/worker failure persists failed evidence and events, fails the ticket, and prevents
  downstream dispatch;
- continue mode blocks only the failed branch;
- re-entering the same lease replays evidence without a second row, while a new lease can retry;
- resuming after a later failure skips an already-verified phase without a second row;
- a forced crash after evidence persistence but before phase-row finalization is reconciled on
  resume without a second verification row;
- a real bundled worker process completes a policy verification end to end;
- execute and `report --job-id` envelopes expose identical verification evidence;
- migration preserves historical summary/history and verification rows;
- failure tests inspect durable job, ticket, lease, verification, and event state;
- behavior tests fail when binding, failure gating, durable evidence, or resume suppression is
  deliberately broken.
