---
name: voom-sprint-11-design
description: Sprint 11 design for staged artifact verification, host-owned commit, recovery visibility, and a narrow CLI exercise path.
status: draft
date: 2026-05-25
sprint: 11
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-24-voom-sprint-10-design.md
  - docs/superpowers/specs/2026-05-24-voom-sprint-10-closeout.md
---

# VOOM Sprint 11 - Staged Artifact Commit And Verification Worker

## 1. Goal

Sprint 11 proves the real-media mutation envelope before adding real mutation
workers. A generated artifact can be staged, verified by an out-of-process
worker, committed by the host, audited, inspected, and surfaced for recovery
when a commit cannot be completed safely.

This sprint does not add FFmpeg, MKVToolNix, backup policy, daemon cleanup, or
production rollback UX. It establishes the control-plane contract that Sprint
12 and Sprint 13 mutation workers will use.

## 2. Scope

Sprint 11 delivers:

- Durable staged artifact records built on `artifact_handles` and
  `artifact_locations`.
- A simple CLI exercise path that creates a staged copy from an existing scanned
  `FileVersion`.
- A bundled out-of-process `verify_artifact` worker.
- Typed verification request/result payloads over the existing worker protocol.
- Host-owned commit from a verified staged artifact to a target local path.
- Commit audit events and recovery-required visibility.
- CLI inspection for staged, verified, committed, failed, and recovery-required
  artifacts.
- Tests and closeout evidence for staging, verification, commit, audit, and
  recovery behavior.

Sprint 11 explicitly does not deliver:

- Transcode, remux, track editing, backup, delete-artifact, or archive workers.
- Policy-driven mutation planning beyond consuming already known
  `FileVersion` IDs.
- Daemon cleanup of abandoned staging files.
- Production rollback UX or operator repair commands beyond inspection.
- Remote media transfer or object-store commit behavior.
- Worker-owned commits. The host remains the only component that mutates final
  managed media state.

## 3. Architecture

The Sprint 11 CLI path is:

```text
voom scan --path <file>
  -> persisted FileAsset / FileVersion / FileLocation / MediaSnapshot

voom artifact stage-copy --file-version-id <id> [--source-location-id <id>] --staging-path <path>
  -> host copies current source bytes to staging
  -> host records artifact handle + staging location

voom artifact verify --artifact-handle-id <id>
  -> bundled verify worker re-observes staged bytes out of process
  -> host persists verification report

voom artifact commit --artifact-handle-id <id> --target-path <path>
  -> host revalidates successful verification and staged bytes
  -> host performs safety-gated filesystem promotion
  -> host records new FileVersion / FileLocation and audit events
```

Workers never write SQLite and never promote artifacts into managed media
locations. The verification worker validates bytes and returns structured facts.
The control plane owns staging records, commit decisions, final filesystem
promotion, durable identity updates, and events.

The simple CLI path is intentionally a stage-copy path, not a fake transcode.
It copies an existing scanned file version into a staging path so Sprint 11 can
exercise real filesystem staging, verification, host commit, and recovery
without inventing media transformation semantics before Sprint 12.

## 4. Staged Artifact Model

Sprint 11 reuses existing artifact identity tables:

- `artifact_handles` represents a staged output candidate.
- `artifact_locations.kind = 'staging'` records the staged file path.
- `artifact_lineage` records artifact-to-artifact provenance only when both
  sides are artifact handles; source `FileVersion` provenance is recorded on
  the staged handle's identity link columns and `source_lineage` JSON.
- `file_versions` and `file_locations` remain the source of truth for committed
  managed media bytes.

The implementation may extend `ArtifactRepo` with read models tailored for the
CLI, but it should not introduce a parallel artifact identity table.

`voom artifact stage-copy` must:

- require an existing, unretired source `FileVersion`;
- choose the source path deterministically: if `--source-location-id` is
  present, it must name a live `local_path` location for the source version; if
  it is absent, exactly one live `local_path` location must exist for the source
  version or the command fails with `CONFIG_INVALID`;
- copy from a live source location to the requested staging path;
- reject a staging path that already exists unless a later implementation plan
  explicitly designs an overwrite flag;
- canonicalize the source path and staging parent directory before copying;
- reject symlink traversal for source, staging, and target paths in Sprint 11,
  matching Sprint 10 scan's conservative local-filesystem posture;
- compute BLAKE3 hash and byte size for the staged file after copy;
- record an `artifact_handle` with expected size/hash, staging durability, local
  access mode, immutable mutability, and source lineage referencing the source
  `FileVersion`;
- populate the staged handle's nullable `file_version_id` link with the source
  `FileVersion` so the relationship is queryable without parsing JSON;
- record one live `artifact_locations` row with `kind = 'staging'`;
- emit `artifact.staged`.

The staged copy command is a host operation. It may read source bytes directly
because it is not invoking media tooling or crossing the provider boundary. Real
media analysis and mutation tools remain out of process.

## 5. Verification Worker

Sprint 11 adds a real bundled verification worker binary. The final crate name
is an implementation detail, but the expected shape mirrors
`voom-ffprobe-worker`: a standalone process launched by the control plane,
speaking the existing HTTP/JSON worker protocol.

The worker handles `OperationKind::VerifyArtifact` and accepts a typed payload:

```json
{
  "path": "/tmp/voom-stage/output.mkv",
  "expected": {
    "size_bytes": 1234,
    "content_hash": "blake3:...",
    "modified_at": "2026-05-25T00:00:00Z",
    "local_file_key": null
  }
}
```

The worker:

- verifies that the path is a regular file;
- computes size and BLAKE3 hash;
- optionally reports modification time and best-effort local file key;
- compares observed facts to expected facts;
- emits progress frames through the existing protocol;
- returns a typed verification result on success.

The worker fails loudly for:

- missing or inaccessible staged file: `artifact_unavailable`;
- expected size/hash mismatch: `artifact_checksum_mismatch`;
- malformed request payload: `malformed_worker_result`;
- IO failures that prevent observation: `artifact_unavailable` or a more
  precise existing failure class when available.

The control plane persists only worker results that match the requested lease and
operation. A verification worker cannot mark an artifact committed.

## 6. Persistence

Sprint 11 should add a migration with two focused tables.

The same migration updates the `file_versions.produced_by` CHECK constraint to
accept `staged_commit`. `staged_commit` requires
`produced_from_version_id IS NOT NULL` and represents host-committed bytes that
were first staged and verified through the Sprint 11 artifact flow.

`artifact_verifications` records verification attempts:

- `id`
- `artifact_handle_id`
- `artifact_location_id`
- `path`
- `worker_id`
- `status`: `succeeded` or `failed`
- `expected_size_bytes`
- `expected_checksum`
- `observed_size_bytes`
- `observed_checksum`
- `failure_class`
- `error_code`
- `message`
- `report`
- `started_at`
- `finished_at`

`artifact_location_id` must point at the staging location whose path was sent to
the worker, and `path` stores the canonical path value verified by that attempt.
`report` is JSON and stores the typed verification result or failure payload.
The latest successful verification for the artifact's current live staging
location is the gate for commit. Failed attempts are kept for audit and
troubleshooting.

`artifact_commit_records` records host commit attempts:

- `id`
- `artifact_handle_id`
- `source_file_version_id`
- `verification_id`
- `target_path`
- `result_file_version_id`
- `result_file_location_id`
- `state`: `pending`, `committed`, `failed`, or `recovery_required`
- `failure_class`
- `error_code`
- `message`
- `recovery_reason`
- `temp_path`
- `report`
- `started_at`
- `promotion_started_at`
- `finished_at`

`failed` is for failures before final filesystem mutation. `recovery_required`
is for failures after the commit has crossed a point where durable state and
filesystem state may need operator reconciliation.

The migration must also prevent duplicate commits for the same staged artifact.
A partial unique index on `artifact_commit_records(artifact_handle_id)` for
`state IN ('pending','committed','recovery_required')` is required because a
staged artifact can have only one in-flight, successful, or recovery-required
commit owner. Failed pre-mutation attempts are excluded so an operator can retry
after correcting the cause. A second partial unique index on canonical
`target_path` for `state IN ('pending','committed','recovery_required')`
prevents two commands from claiming the same final path while a previous commit
is in flight, successful, or awaiting recovery.

Sprint 11 does not route add-only commits through the existing destructive
commit-intent table. That table only models delete, replace, and move targets
today. Instead, Sprint 11 adds a narrow staged-commit gate with explicit
database/filesystem phase boundaries:

- **Prepare transaction:** acquire SQLite's write lock with the same transaction
  discipline used by the existing repositories; re-read the artifact handle,
  source `FileVersion`, live staging location, and latest successful
  verification for that exact staging location; reject retired source versions,
  missing staging locations, stale verification rows, verification rows for a
  different staging location, staged-byte drift, and existing target paths;
  create the `pending` commit record; emit `artifact.commit_started`; commit the
  transaction before filesystem promotion.
- **Filesystem promotion:** copy the staged bytes to a temporary sibling path,
  fsync, and atomically rename that temporary path to the target path. This
  phase re-observes the staged file immediately before copying, verifies the
  temporary file after copy and before rename, and verifies the final target file
  after rename. All three observations must match the successful verification's
  size and checksum. The commit record already exists before this phase begins,
  so a crash or process kill leaves durable evidence that an in-flight commit
  needs inspection.
- **Finalize transaction:** after successful promotion, acquire a new write
  transaction, re-read the pending commit record, record the resulting
  `FileVersion` and `FileLocation`, retire the staging artifact location, mark
  the commit `committed`, and emit `artifact.commit_completed`.
- **Recovery transaction:** if promotion starts but promotion or finalize cannot
  complete, acquire a new write transaction and mark the existing pending record
  `recovery_required` with the target path, temporary path, observed filesystem
  state, and any durable IDs already created. Emit
  `artifact.commit_recovery_required` in that transaction.

The staged-commit gate is non-destructive: it does not retire source locations
and therefore does not use the destructive use-lease blocking rule by default.
If Sprint 11 implementation discovers it needs replace, move, archive, or delete
semantics, that work is out of scope and must use the existing destructive
commit safety gate or a follow-on design.

## 7. Host Commit Semantics

`voom artifact commit` requires:

- one live staging location for the artifact;
- a latest successful verification for that same live staging location;
- staged bytes that still match the verified size/hash immediately before
  promotion;
- a target path that does not already exist unless a later implementation plan
  explicitly designs replace semantics;
- canonical target path storage; relative paths, symlink aliases, and
  non-canonical parent paths must not bypass `target_path` uniqueness;
- an existing source `FileVersion` from the artifact handle link.

Sprint 11 commit is add-only by default: it creates a new target file path and a
new `FileVersion` produced from the source version. It does not retire the
source location or replace the original file by default. This keeps the first
mutation envelope recoverable and avoids overloading Sprint 11 with destructive
replace behavior.

The host commit sequence follows the staged-commit gate phases:

1. In the prepare transaction, re-observe staged bytes and compare them to the
   latest successful verification.
2. Record `artifact_commit_records.state = 'pending'`, `target_path`, and the
   planned temporary sibling path; emit `artifact.commit_started`; commit.
3. Outside the database transaction, re-observe the staged file. If its size or
   checksum no longer matches the successful verification, transition the commit
   record to `recovery_required` without copying bytes.
4. Copy the staged bytes to the temporary sibling path under the target
   directory, fsync the temporary file, and verify the temporary file's size and
   checksum before rename.
5. Atomically rename the temporary path to the requested target path, then
   verify the target file's size and checksum before finalizing durable identity
   state. The staging file is not moved; keeping it intact makes retry and
   recovery inspection deterministic.
6. In the finalize transaction, record the new `FileVersion` with
   `produced_by = 'staged_commit'`.
7. Record the new `FileLocation` at the target path.
8. Retire the `artifact_locations.kind = 'staging'` row for the staged handle.
   The staged file may remain on disk until a later cleanup feature removes it;
   the retired artifact location means Sprint 11 no longer treats it as the live
   staging source for new commits.
9. Record artifact lineage when there is a committed artifact handle to link,
   then mark the commit record `committed`.
10. Emit `artifact.commit_completed`.

Any failure before the prepare transaction commits marks the command as
`failed_pre_mutation` without creating a durable commit record, or marks the
pending record `failed` if one was already inserted in the same transaction.
Any failure after the prepare transaction commits must preserve the existing
commit record and transition it to `recovery_required`, because the filesystem
phase may have started or may be impossible to prove did not start. The recovery
record stores the temporary path, target path, observed filesystem state, and
any durable IDs already created. The CLI must never report success while
recovery is required. Recovery inspection must show whether the target path
exists, whether the temporary path exists, whether the staging path still exists,
and which durable IDs were created before failure.

## 8. CLI

Sprint 11 adds an `artifact` command family:

```text
voom artifact stage-copy --file-version-id <id> [--source-location-id <id>] --staging-path <path>
voom artifact verify --artifact-handle-id <id>
voom artifact commit --artifact-handle-id <id> --target-path <path>
voom artifact list [--state <state>] [--limit <n>]
voom artifact show --artifact-handle-id <id>
```

All commands emit exactly one JSON envelope on stdout. The command data should
include stable IDs, paths, size/hash facts, verification status, commit state,
and recovery fields when present.

The command family is agent-facing. It should avoid ambiguous prose-only
success messages and should make durable row IDs visible so follow-up commands
can inspect state without guessing.

## 9. Events

Sprint 11 adds typed event payloads for:

- `artifact.staged`
- `artifact.verification_started`
- `artifact.verification_succeeded`
- `artifact.verification_failed`
- `artifact.commit_started`
- `artifact.commit_completed`
- `artifact.commit_failed_pre_mutation`
- `artifact.commit_recovery_required`

Events are audit facts only. Artifact state, verification rows, commit records,
file versions, and file locations remain the source of truth. Events must be
written in the same transaction as the durable state transition they describe
whenever the transition is database-only. Filesystem promotion failures after
mutation must still produce a durable recovery event before returning.

## 10. Error Handling

Stable failure behavior:

- Missing source `FileVersion`: `NOT_FOUND`.
- Source version without a live local path: `CONFIG_INVALID` or
  `artifact_unavailable` in the command payload.
- Staging path already exists: `CONFIG_INVALID`.
- Staged artifact or staging location missing: `NOT_FOUND`.
- Verification required before commit: `CONFIG_INVALID`.
- Staged bytes drift from expected or verified facts:
  `artifact_checksum_mismatch`.
- Worker launch, protocol, timeout, or malformed result failures use the
  existing worker failure taxonomy.
- Commit failure before final filesystem mutation: failed command with
  `artifact.commit_failed_pre_mutation`.
- Commit failure after final filesystem mutation begins: failed command with
  `artifact.commit_recovery_required` and a visible `recovery_required` record.

Silent skips are not allowed. If a command records partial durable state, the
error envelope must include enough IDs for an agent to inspect that state.

## 11. Testing

Required tests:

- Repository tests for `artifact_verifications` and `artifact_commit_records`
  state transitions.
- Control-plane unit tests for stage-copy validation, staged byte hashing,
  verification persistence, and commit precondition checks.
- Verification worker conformance tests for success, missing file, expected
  hash mismatch, expected size mismatch, and malformed payload.
- Integration tests for scan -> stage-copy -> verify -> commit using fixture
  media.
- Integration tests proving unverified commit rejection and staged-byte drift
  rejection.
- Recovery tests that inject failure after filesystem promotion begins and prove
  the command returns failure while durable state is `recovery_required`.
- CLI insta snapshots for stage-copy, verify, commit, list/show inspection, and
  representative failures.
- Event tests proving state transitions and event payloads are written together.
- Documentation placeholder scan.
- `just ci`.

Tests must follow the repository layout convention: sibling `*_test.rs` files
for unit tests and integration tests under `crates/*/tests/`.

## 12. Acceptance Criteria

Sprint 11 is complete when:

- A user can scan fixture media, stage-copy from the resulting `FileVersion`,
  verify the staged artifact through an out-of-process worker, commit it to a
  target path, and inspect the result through JSON-envelope CLI commands.
- The committed output records durable `FileVersion` and `FileLocation` state
  linked back to the staged artifact and source version.
- Verification is required before commit, and drift between verification and
  commit is rejected.
- Commit audit events are emitted for started, completed, pre-mutation failure,
  and recovery-required transitions.
- Recovery-required state is durable and visible through CLI inspection.
- Worker conformance tests cover success and failure cases.
- CLI golden tests lock the agent-facing envelope shape.
- The Sprint 11 closeout matrix records repeatable evidence for staging,
  verification, commit, audit, and recovery behavior.
- `just ci` passes.

## 13. Deferred Work

Deferred to later roadmap work:

- Sprint 12 FFmpeg transcode worker integration with staged commit.
- Sprint 13 MKVToolNix remux and track-edit integration with staged commit.
- Backup policy and destructive replace/delete/archive flows.
- Daemon cleanup of abandoned staging files.
- Production rollback and repair UX for recovery-required commits.
- Remote artifact transfer, object-store staging, and cross-node commit
  promotion.
