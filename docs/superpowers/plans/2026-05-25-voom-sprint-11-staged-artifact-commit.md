# VOOM Sprint 11 Staged Artifact Commit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement staged artifact copy, out-of-process verification, host-owned add-only commit, audit events, recovery visibility, and CLI inspection.

**Architecture:** Keep durable rows and transaction gates in `voom-store`, orchestration and filesystem safety checks in `voom-control-plane`, typed worker payloads in `voom-worker-protocol`, verification bytes-only logic in a new bundled out-of-process worker, and JSON envelope mapping in `voom-cli`. The host may copy and promote bytes, but workers never write SQLite and never mutate managed media locations.

**Tech Stack:** Rust, Tokio, sqlx/SQLite, serde/serde_json, BLAKE3, existing HTTP/NDJSON worker protocol, existing event log, sibling unit tests, CLI insta snapshots, and `just` verification commands.

---

## Success Criteria

- `voom artifact stage-copy --file-version-id <id> --staging-path <path>` copies from a live source `local_path`, rejects symlinks/existing staging paths, records an `artifact_handle`, a live `artifact_locations.kind = 'staging'` row, source lineage, and `artifact.staged`.
- `voom artifact verify --artifact-handle-id <id>` launches a bundled `voom-verify-artifact-worker`, dispatches `verify_artifact`, persists a succeeded or failed `artifact_verifications` row, and emits verification events.
- `voom artifact commit --artifact-handle-id <id> --target-path <path>` requires a latest successful verification for the current live staging location, promotes bytes with temp-file verification, records a new `FileVersion` produced by `staged_commit`, records a target `FileLocation`, retires the staging location, marks the commit row `committed`, and emits commit events.
- Commit failures before durable prepare are visible in the error envelope and emit `artifact.commit_failed_pre_mutation`; failures after prepare transition the durable row to `recovery_required` and emit `artifact.commit_recovery_required`.
- `voom artifact list` and `voom artifact show` expose stable ids, paths, size/hash facts, verification status, commit state, and recovery fields.
- Repository, control-plane, worker, CLI snapshot, integration, event, and recovery tests cover the Sprint 11 acceptance matrix.
- `just ci` passes after intentional snapshot review.

## Assumptions And Decisions

- The verification worker crate is named `voom-verify-artifact-worker`; the binary has the same name and is resolved through `VOOM_VERIFY_ARTIFACT_WORKER_BIN`, then beside the current executable, then on `PATH`.
- The worker protocol already has `OperationKind::VerifyArtifact`; Sprint 11 adds typed payload structs only, not a new operation vocabulary item.
- The staged artifact handle uses existing identity link columns: `artifact_handles.file_version_id` points at the source `FileVersion`; `source_lineage` stores a JSON object with `source_file_version_id`, optional `source_location_id`, and canonical source path.
- sqlx 0.8 SQLite migrations run inside a transaction, so the migration must not depend on `PRAGMA foreign_keys = OFF`; that pragma is ineffective after the transaction starts. The `file_versions.produced_by` CHECK update must use the SQLite table-rebuild pattern with `PRAGMA legacy_alter_table = ON`, rename the old table, create the replacement table under the original name, copy rows, drop the old table, recreate indexes, and prove `PRAGMA foreign_key_check` is empty on a seeded database with existing `file_versions`, `file_locations`, `media_snapshots`, and `artifact_handles.file_version_id` links.
- `ProducedBy::StagedCommit` is a new repo enum variant and, like transcode/remux/restore, requires `produced_from_version_id`.
- Stage-copy and commit reject symlink traversal conservatively by checking `symlink_metadata` on explicit paths and canonicalizing existing parent directories. The staging/target leaf must not exist when the command begins, and the final install step must also be no-overwrite so a concurrent creator cannot be replaced between preflight and promotion.
- Sprint 11 commit is add-only. It never retires source file locations and does not route through destructive `commit_intents`.
- Recovery injection for tests is implemented as an internal control-plane hook behind `#[cfg(test)]` or a trait parameter, not a user-facing CLI flag.
- Direct verify dispatch mirrors Sprint 10 scan’s direct bundled-worker path and uses an ephemeral protocol `LeaseId`; it does not create durable tickets.
- Before launching the bundled verify worker, the control plane creates or reuses one durable worker row named `builtin.verify_artifact` with `verify_artifact` capability and grant. The spawned process receives that `WorkerId`, and every `artifact_verifications.worker_id` and verification event refers to that durable row.
- The new event names intentionally use the Sprint 11 spec names (`artifact.staged`, `artifact.verification_started`, etc.) rather than the older `artifact_handle.created` style.

## File Structure

- Modify `Cargo.toml`: add workspace member and dependency entry for `crates/voom-verify-artifact-worker` only if another crate needs its test helpers; production code must launch the binary rather than link it for verification.
- Modify `crates/voom-core/src/ids.rs`: add `ArtifactVerificationId` and `ArtifactCommitRecordId`.
- Modify `migrations/0012_staged_artifact_commit.sql`: add `artifact_verifications`, `artifact_commit_records`, indexes, and the `staged_commit` `file_versions.produced_by` CHECK migration.
- Modify `crates/voom-store/src/repo/artifacts.rs` and `artifacts_test.rs`: extend artifact handle read models and add verification/commit-record repository methods.
- Modify `crates/voom-store/src/repo/identity.rs` and `identity_test.rs`: add `ProducedBy::StagedCommit` and narrow helpers needed to create a staged-commit file version/location in a caller-owned transaction.
- Modify `crates/voom-events/src/kind.rs`, `payload.rs`, and tests: add Sprint 11 artifact lifecycle event kinds and payloads.
- Modify `crates/voom-worker-protocol/src/lib.rs`: export typed verification payloads.
- Create `crates/voom-worker-protocol/src/verify_artifact.rs` and `verify_artifact_test.rs`: typed request/result/fact/status structs.
- Create `crates/voom-verify-artifact-worker/Cargo.toml`, `src/lib.rs`, `src/main.rs`, `src/observe.rs`, `src/handler.rs`, sibling tests, and `tests/verify_worker.rs`.
- Create `crates/voom-control-plane/src/artifact/mod.rs`, `fs.rs`, `stage.rs`, `verify.rs`, `commit.rs`, `inspect.rs`, `worker.rs`, `bootstrap.rs`, and sibling tests.
- Modify `crates/voom-control-plane/src/lib.rs`: export the artifact module and methods.
- Modify `crates/voom-cli/src/cli.rs`: add `artifact` command family and state enum.
- Create `crates/voom-cli/src/commands/artifact.rs` and `artifact_test.rs`; modify `commands/mod.rs` and `main.rs` to dispatch.
- Create `crates/voom-cli/tests/artifact_envelope.rs` and snapshots under `crates/voom-cli/tests/snapshots/`.
- Create `crates/voom-control-plane/tests/staged_artifact_flow.rs`: scan to stage-copy to verify to commit integration coverage.
- Modify `docs/superpowers/specs/2026-05-25-voom-sprint-11-closeout.md` after implementation with evidence.

## Task 1: Migration And ID Types

**Files:**
- Modify: `crates/voom-core/src/ids.rs`
- Modify: `crates/voom-core/src/ids_test.rs`
- Create: `migrations/0012_staged_artifact_commit.sql`
- Modify: `crates/voom-store/tests/migration_inventory.rs`

- [ ] **Step 1: Add failing ID tests**

Add assertions in `crates/voom-core/src/ids_test.rs` proving `ArtifactVerificationId(7)` and `ArtifactCommitRecordId(9)` serialize as transparent integers and format with `Display`.

Run:

```bash
cargo test -p voom-core ids
```

Expected: compile failure because the new id types do not exist.

- [ ] **Step 2: Add id newtypes**

In `crates/voom-core/src/ids.rs`, add:

```rust
define_id!(ArtifactVerificationId);
define_id!(ArtifactCommitRecordId);
```

Place them next to the existing artifact ids.

- [ ] **Step 3: Add migration inventory and seeded-upgrade expectations**

Update `crates/voom-store/tests/migration_inventory.rs` to include `0012_staged_artifact_commit.sql`.

Add a migration-upgrade test that creates a database through migration `0011`, inserts one scanned-style `file_asset`, `file_version`, `file_location`, `media_snapshot`, and artifact handle linked to the file version, then applies all migrations and asserts:

```rust
let violations: Vec<(String, i64, String, i64)> =
    sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
assert_eq!(violations, Vec::<(String, i64, String, i64)>::new());

sqlx::query(
    "INSERT INTO file_versions \
     (file_asset_id, content_hash, size_bytes, produced_by, produced_from_version_id, created_at) \
     VALUES (?, 'blake3:new', 3, 'staged_commit', ?, '2026-05-25T00:00:00Z')",
)
.bind(file_asset_id)
.bind(source_file_version_id)
.execute(&pool)
.await
.unwrap();
```

Run:

```bash
cargo test -p voom-store --test migration_inventory
```

Expected: failure because the migration file does not exist.

- [ ] **Step 4: Create the migration**

Create `migrations/0012_staged_artifact_commit.sql` with:

```sql
-- Sprint 11 -- staged artifact verification and host-owned add-only commit.

-- sqlx-sqlite wraps each migration in one transaction. `PRAGMA foreign_keys`
-- cannot be disabled inside that transaction, so preserve existing child table
-- references by preventing ALTER TABLE from rewriting them to file_versions_old.
PRAGMA legacy_alter_table = ON;

ALTER TABLE file_versions RENAME TO file_versions_old;

CREATE TABLE file_versions (
    id                          INTEGER PRIMARY KEY,
    file_asset_id               INTEGER NOT NULL REFERENCES file_assets(id) ON DELETE RESTRICT,
    content_hash                TEXT NOT NULL,
    size_bytes                  INTEGER NOT NULL CHECK (size_bytes >= 0),
    produced_by                 TEXT NOT NULL
        CHECK (produced_by IN ('ingest','transcode','remux','restore','external_observed','staged_commit')),
    produced_from_version_id    INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    created_at                  TEXT NOT NULL,
    retired_at                  TEXT,
    epoch                       INTEGER NOT NULL DEFAULT 0,
    CHECK (
        (produced_by IN ('ingest','external_observed'))
        OR produced_from_version_id IS NOT NULL
    )
) STRICT;

INSERT INTO file_versions
SELECT id, file_asset_id, content_hash, size_bytes, produced_by,
       produced_from_version_id, created_at, retired_at, epoch
FROM file_versions_old;

DROP TABLE file_versions_old;

CREATE INDEX file_versions_by_asset ON file_versions (file_asset_id);
CREATE INDEX file_versions_by_hash  ON file_versions (content_hash);

PRAGMA legacy_alter_table = OFF;

CREATE TABLE artifact_verifications (
    id                    INTEGER PRIMARY KEY,
    artifact_handle_id    INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    artifact_location_id  INTEGER NOT NULL REFERENCES artifact_locations(id) ON DELETE RESTRICT,
    path                  TEXT NOT NULL,
    worker_id             INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    status                TEXT NOT NULL CHECK (status IN ('succeeded','failed')),
    expected_size_bytes   INTEGER NOT NULL CHECK (expected_size_bytes >= 0),
    expected_checksum     TEXT NOT NULL,
    observed_size_bytes   INTEGER CHECK (observed_size_bytes IS NULL OR observed_size_bytes >= 0),
    observed_checksum     TEXT,
    failure_class         TEXT,
    error_code            TEXT,
    message               TEXT,
    report                TEXT NOT NULL CHECK (json_valid(report)),
    started_at            TEXT NOT NULL,
    finished_at           TEXT NOT NULL,
    CHECK (
           (status = 'succeeded' AND observed_size_bytes IS NOT NULL AND observed_checksum IS NOT NULL
            AND failure_class IS NULL AND error_code IS NULL AND message IS NULL)
        OR (status = 'failed' AND failure_class IS NOT NULL AND error_code IS NOT NULL AND message IS NOT NULL)
    )
) STRICT;

CREATE INDEX artifact_verifications_by_artifact
    ON artifact_verifications (artifact_handle_id, id DESC);
CREATE INDEX artifact_verifications_success_by_location
    ON artifact_verifications (artifact_handle_id, artifact_location_id, id DESC)
    WHERE status = 'succeeded';

CREATE TABLE artifact_commit_records (
    id                       INTEGER PRIMARY KEY,
    artifact_handle_id       INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    source_file_version_id   INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    verification_id          INTEGER NOT NULL REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    target_path              TEXT NOT NULL,
    result_file_version_id   INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    result_file_location_id  INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    state                    TEXT NOT NULL CHECK (state IN ('pending','committed','failed','recovery_required')),
    failure_class            TEXT,
    error_code               TEXT,
    message                  TEXT,
    recovery_reason          TEXT,
    temp_path                TEXT,
    report                   TEXT NOT NULL CHECK (json_valid(report)),
    started_at               TEXT NOT NULL,
    promotion_started_at     TEXT,
    finished_at              TEXT,
    CHECK (
           (state = 'pending' AND result_file_version_id IS NULL AND result_file_location_id IS NULL
            AND failure_class IS NULL AND error_code IS NULL AND message IS NULL AND recovery_reason IS NULL
            AND finished_at IS NULL)
        OR (state = 'committed' AND result_file_version_id IS NOT NULL AND result_file_location_id IS NOT NULL
            AND failure_class IS NULL AND error_code IS NULL AND message IS NULL AND recovery_reason IS NULL
            AND finished_at IS NOT NULL)
        OR (state = 'failed' AND failure_class IS NOT NULL AND error_code IS NOT NULL AND message IS NOT NULL
            AND recovery_reason IS NULL AND finished_at IS NOT NULL)
        OR (state = 'recovery_required' AND failure_class IS NOT NULL AND error_code IS NOT NULL
            AND message IS NOT NULL AND recovery_reason IS NOT NULL AND finished_at IS NOT NULL)
    )
) STRICT;

CREATE UNIQUE INDEX artifact_commit_records_one_owner_per_artifact
    ON artifact_commit_records (artifact_handle_id)
    WHERE state IN ('pending','committed','recovery_required');

CREATE UNIQUE INDEX artifact_commit_records_one_owner_per_target
    ON artifact_commit_records (target_path)
    WHERE state IN ('pending','committed','recovery_required');

CREATE INDEX artifact_commit_records_by_state
    ON artifact_commit_records (state, started_at DESC);

-- Keep this statement at the end of the migration. The seeded-upgrade test
-- must also query it explicitly because SQLite returns one row per violation.
PRAGMA foreign_key_check;
```

- [ ] **Step 5: Run migration tests and commit**

Run:

```bash
cargo test -p voom-core ids
cargo test -p voom-store --test migration_inventory
cargo test -p voom-store --test init
cargo test -p voom-store --test repo_roundtrip
just fmt
git add crates/voom-core/src/ids.rs crates/voom-core/src/ids_test.rs crates/voom-store/tests/migration_inventory.rs migrations/0012_staged_artifact_commit.sql
git commit -m "feat(store): add staged artifact commit schema"
```

Expected: tests pass.

## Task 2: Artifact Repository Read/Write Models

**Files:**
- Modify: `crates/voom-store/src/repo/artifacts.rs`
- Modify: `crates/voom-store/src/repo/artifacts_test.rs`
- Modify: `crates/voom-store/src/repo/identity.rs`
- Modify: `crates/voom-store/src/repo/identity_test.rs`

- [ ] **Step 1: Write repository tests**

Add tests in `artifacts_test.rs` covering:

- creating a staged handle linked to a source `FileVersion`;
- recording succeeded and failed verification rows;
- selecting the latest successful verification for the live staging location;
- inserting `pending`, `committed`, `failed`, and `recovery_required` commit records;
- partial unique indexes reject a second `pending` owner for the same artifact or target path but allow retry after `failed`.

Run:

```bash
cargo test -p voom-store repo::artifacts
```

Expected: compile failures for missing repository types and methods.

- [ ] **Step 2: Extend artifact handle models**

Add nullable identity-link fields to `ArtifactHandle` and `NewArtifactHandle` only where they are needed by Sprint 11:

```rust
pub file_version_id: Option<FileVersionId>,
```

Update `create_handle_in_tx` to insert `file_version_id`, and update `row_to_handle` to select it.

- [ ] **Step 3: Add verification models**

Add enums and structs in `artifacts.rs`:

```rust
pub enum ArtifactVerificationStatus { Succeeded, Failed }
pub struct NewArtifactVerification { /* columns from migration */ }
pub struct ArtifactVerification { /* id plus columns from migration */ }
```

Implement `record_verification_in_tx`, `latest_successful_verification_for_live_staging_in_tx`, and `list_verifications`.

- [ ] **Step 4: Add commit-record models**

Add:

```rust
pub enum ArtifactCommitState { Pending, Committed, Failed, RecoveryRequired }
pub struct NewArtifactCommitRecord { /* prepare columns */ }
pub struct ArtifactCommitRecord { /* full row */ }
```

Implement `create_pending_commit_in_tx`, `mark_commit_committed_in_tx`, `mark_commit_failed_in_tx`, `mark_commit_recovery_required_in_tx`, `get_commit_record`, and `list_commit_records`.

- [ ] **Step 5: Add identity repo staged-commit support**

Add `ProducedBy::StagedCommit`, parse/as_str branches, and tests proving parent is required. Add or reuse `create_file_version_in_tx` and `record_file_location_in_tx` from existing APIs for finalize transaction use.

- [ ] **Step 6: Run repository verification and commit**

Run:

```bash
cargo test -p voom-store repo::artifacts
cargo test -p voom-store repo::identity
just fmt
git add crates/voom-store/src/repo/artifacts.rs crates/voom-store/src/repo/artifacts_test.rs crates/voom-store/src/repo/identity.rs crates/voom-store/src/repo/identity_test.rs
git commit -m "feat(store): add staged artifact repositories"
```

Expected: tests pass.

## Task 3: Sprint 11 Event Types

**Files:**
- Modify: `crates/voom-events/src/kind.rs`
- Modify: `crates/voom-events/src/kind_test.rs`
- Modify: `crates/voom-events/src/payload.rs`
- Modify: `crates/voom-events/src/payload_test.rs`

- [ ] **Step 1: Write event round-trip tests**

Add tests for these exact event strings:

```text
artifact.staged
artifact.verification_started
artifact.verification_succeeded
artifact.verification_failed
artifact.commit_started
artifact.commit_completed
artifact.commit_failed_pre_mutation
artifact.commit_recovery_required
```

Run:

```bash
cargo test -p voom-events kind payload
```

Expected: failures because the variants do not exist.

- [ ] **Step 2: Add kind variants and parse/as_str branches**

Extend `EventKind` with one variant per Sprint 11 event and add exact dotted strings in `as_str` and `from_str`.

- [ ] **Step 3: Add payload structs and `Event` variants**

Add payloads with stable ids and facts:

- `ArtifactStagedPayload`: handle id, location id, source file version id, optional source location id, staging path, size, checksum.
- `ArtifactVerificationStartedPayload`: handle id, location id, worker id, path.
- `ArtifactVerificationFinishedPayload`: verification id, handle id, location id, worker id, status, observed facts or error code.
- `ArtifactCommitStartedPayload`: commit record id, handle id, source version id, verification id, target path, temp path.
- `ArtifactCommitCompletedPayload`: commit record id, handle id, result version id, result location id, target path.
- `ArtifactCommitFailedPreMutationPayload`: handle id, optional commit record id, target path, error code, message.
- `ArtifactCommitRecoveryRequiredPayload`: commit record id, handle id, target path, temp path, recovery reason, error code, message.

- [ ] **Step 4: Run event tests and commit**

Run:

```bash
cargo test -p voom-events
just fmt
git add crates/voom-events/src/kind.rs crates/voom-events/src/kind_test.rs crates/voom-events/src/payload.rs crates/voom-events/src/payload_test.rs
git commit -m "feat(events): add staged artifact lifecycle events"
```

Expected: event tests pass.

## Task 4: Typed VerifyArtifact Protocol Payloads

**Files:**
- Create: `crates/voom-worker-protocol/src/verify_artifact.rs`
- Create: `crates/voom-worker-protocol/src/verify_artifact_test.rs`
- Modify: `crates/voom-worker-protocol/src/lib.rs`

- [ ] **Step 1: Write protocol tests**

Add tests proving:

- request serializes as `{ "path": "...", "expected": { "size_bytes": ..., "content_hash": "...", "modified_at": null, "local_file_key": null } }`;
- result status serializes as `verified`;
- unknown fields are rejected.

Run:

```bash
cargo test -p voom-worker-protocol verify_artifact
```

Expected: compile failure for missing module.

- [ ] **Step 2: Add typed module**

Create `verify_artifact.rs` with:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactRequest {
    pub path: String,
    pub expected: VerifyArtifactExpectedFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyArtifactStatus {
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArtifactResult {
    pub status: VerifyArtifactStatus,
    pub provider: String,
    pub provider_version: String,
    pub observed: VerifyArtifactObservedFacts,
}

#[cfg(test)]
#[path = "verify_artifact_test.rs"]
mod tests;
```

Export these types from `lib.rs`.

- [ ] **Step 3: Run tests and commit**

Run:

```bash
cargo test -p voom-worker-protocol verify_artifact
just fmt
git add crates/voom-worker-protocol/src/lib.rs crates/voom-worker-protocol/src/verify_artifact.rs crates/voom-worker-protocol/src/verify_artifact_test.rs
git commit -m "feat(protocol): add verify artifact payloads"
```

Expected: protocol tests pass.

## Task 5: Verification Worker

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/voom-verify-artifact-worker/Cargo.toml`
- Create: `crates/voom-verify-artifact-worker/src/lib.rs`
- Create: `crates/voom-verify-artifact-worker/src/main.rs`
- Create: `crates/voom-verify-artifact-worker/src/observe.rs`
- Create: `crates/voom-verify-artifact-worker/src/observe_test.rs`
- Create: `crates/voom-verify-artifact-worker/src/handler.rs`
- Create: `crates/voom-verify-artifact-worker/src/handler_test.rs`
- Create: `crates/voom-verify-artifact-worker/tests/verify_worker.rs`

- [ ] **Step 1: Write worker tests**

Cover:

- success emits progress then `VerifyArtifactResult`;
- missing file emits terminal error `ArtifactUnavailable`;
- size mismatch and hash mismatch emit terminal error `ArtifactChecksumMismatch`;
- malformed request payload returns an accepted operation stream ending in worker-domain `MalformedWorkerResult`;
- unsupported operation returns `ProtocolError::UnknownOperation`;
- binary prints `BOUND addr=...` and exits on stdin close.

Run:

```bash
cargo test -p voom-verify-artifact-worker
```

Expected: package does not exist.

- [ ] **Step 2: Add crate manifest and workspace entry**

Add `crates/voom-verify-artifact-worker` to workspace members. Its dependencies should mirror `voom-ffprobe-worker` where applicable: `voom-core`, `voom-worker-protocol`, `tokio`, `serde_json`, `chrono`, `secrecy`, and `blake3`.

- [ ] **Step 3: Implement observation**

Implement `observe::observe_file_facts(path: &Path)` with `tokio::fs::symlink_metadata`, regular-file requirement, BLAKE3 hash, byte size, optional mtime, and no symlink following beyond the already resolved path.

- [ ] **Step 4: Implement handler**

Handle only `OperationKind::VerifyArtifact`; any other operation returns `ProtocolError::UnknownOperation`. Decode `VerifyArtifactRequest`; if decode fails, return an accepted operation stream terminated by `ProgressFrame::Error` with `FailureClass::MalformedWorkerResult` and `ErrorCode::MalformedWorkerResult` so the control plane can persist a failed verification attempt. For a valid request, emit one progress frame, observe the file, compare size/hash to expected, then emit either result or terminal error.

- [ ] **Step 5: Implement binary main**

Copy the `voom-ffprobe-worker` main shape: load `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`, `VOOM_WORKER_SECRET`, bind `VOOM_WORKER_BIND`, print `BOUND addr=...`, and shut down on stdin close.

- [ ] **Step 6: Run worker tests and commit**

Run:

```bash
cargo test -p voom-verify-artifact-worker
just fmt
git add Cargo.toml crates/voom-verify-artifact-worker
git commit -m "feat(worker): add verify artifact worker"
```

Expected: worker tests pass.

## Task 6: Control-Plane Filesystem Helpers

**Files:**
- Create: `crates/voom-control-plane/src/artifact/fs.rs`
- Create: `crates/voom-control-plane/src/artifact/fs_test.rs`
- Create: `crates/voom-control-plane/src/artifact/mod.rs`
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Write filesystem helper tests**

Cover canonical parent resolution, leaf-does-not-exist validation, symlink rejection, regular-file observation, BLAKE3 hashing, temp sibling path generation, temp copy verification, cleanup of newly-created files after caller-visible failures, atomic no-overwrite install behavior, and races where the final path appears after preflight but before install.

Run:

```bash
cargo test -p voom-control-plane artifact::fs
```

Expected: compile failure for missing module.

- [ ] **Step 2: Add helper module**

Implement narrow helpers:

- `canonical_existing_file_no_symlink(path) -> PathBuf`;
- `canonical_new_leaf_no_symlink(path) -> PathBuf`;
- `observe_regular_file(path) -> ArtifactFileFacts`;
- `copy_regular_file_checked(source, destination) -> ArtifactFileFacts`;
- `copy_to_unique_temp_then_install_no_replace(source, final_path) -> ArtifactFileFacts`;
- `promote_staged_add_only(staging, target, expected, failpoint) -> PromotionReport`.

Use `tokio::fs` for async metadata/read/copy/open and `std::fs::File::sync_all` inside `spawn_blocking` only where needed for fsync. The final install helper must not use a plain overwrite-capable `rename`; use a no-replace primitive such as `renameat2(RENAME_NOREPLACE)` where available or same-directory hard-link-then-unlink-temp semantics, and return `Config`/`CommitFailure` if the destination appears concurrently.

- [ ] **Step 3: Wire module exports**

Export `artifact` from `lib.rs` and add sibling `#[path]` declarations.

- [ ] **Step 4: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane artifact::fs
just fmt
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/artifact
git commit -m "feat(control-plane): add artifact filesystem helpers"
```

Expected: helper tests pass.

## Task 7: Stage-Copy Use Case

**Files:**
- Create: `crates/voom-control-plane/src/artifact/stage.rs`
- Create: `crates/voom-control-plane/src/artifact/stage_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/mod.rs`

- [ ] **Step 1: Write stage-copy tests**

Cover:

- missing source version returns `NotFound`;
- absent source location with zero/multiple live local paths returns `Config`;
- explicit source location must belong to the source version and be live `local_path`;
- existing staging path returns `Config`;
- staging path created by another process after preflight is not overwritten and returns `Config`;
- source/staging symlink rejection;
- DB failure after filesystem copy attempts to remove the newly created staging file and returns an error report naming whether cleanup succeeded;
- success copies bytes, hashes staged file, creates handle linked to source version, creates staging location, and emits `artifact.staged` in the same transaction.

Run:

```bash
cargo test -p voom-control-plane artifact::stage
```

Expected: compile failure for missing use case.

- [ ] **Step 2: Implement `StageCopyInput` and report**

Add public structs with ids and facts:

```rust
pub struct StageCopyInput {
    pub file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub staging_path: PathBuf,
}

pub struct StageCopyReport {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: FileLocationId,
    pub source_path: PathBuf,
    pub staging_path: PathBuf,
    pub size_bytes: u64,
    pub checksum: String,
}
```

- [ ] **Step 3: Implement source-location selection**

Use identity repo live-location reads. If explicit, require matching source version, live row, and `FileLocationKind::LocalPath`. If implicit, require exactly one live local path.

- [ ] **Step 4: Implement copy and transaction**

Copy source bytes to a unique temporary sibling of the canonical staging path, fsync and verify the temp file, then install the temp file at the requested staging path with the no-overwrite helper before opening the transaction. If the destination appeared after preflight, remove the temp file and return `Config` without replacing the destination. If any database step fails after the final staging file exists, attempt to remove that staging file before returning; if removal also fails, include `staging_path`, `cleanup_attempted: true`, and `cleanup_succeeded: false` in the command error data so an agent can inspect the orphan explicitly. In the transaction, re-read source version/location, record the artifact handle with `file_version_id`, record the staging location, append `artifact.staged`, and commit.

- [ ] **Step 5: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane artifact::stage
just fmt
git add crates/voom-control-plane/src/artifact/stage.rs crates/voom-control-plane/src/artifact/stage_test.rs crates/voom-control-plane/src/artifact/mod.rs
git commit -m "feat(control-plane): stage artifact copies"
```

Expected: stage tests pass.

## Task 8: Verification Use Case

**Files:**
- Create: `crates/voom-control-plane/src/artifact/bootstrap.rs`
- Create: `crates/voom-control-plane/src/artifact/bootstrap_test.rs`
- Create: `crates/voom-control-plane/src/artifact/worker.rs`
- Create: `crates/voom-control-plane/src/artifact/worker_test.rs`
- Create: `crates/voom-control-plane/src/artifact/verify.rs`
- Create: `crates/voom-control-plane/src/artifact/verify_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/mod.rs`

- [ ] **Step 1: Write verification tests**

Cover idempotent durable `builtin.verify_artifact` worker bootstrap, retired bootstrap worker conflict, live staging location requirement, missing handle/location `NotFound`, worker success persistence using the bootstrapped `WorkerId`, worker terminal failure persistence, malformed worker result handling, malformed request payload handling as a terminal worker-domain `MalformedWorkerResult` error, unsupported operation as `ProtocolError::UnknownOperation`, and events in the same transaction as persisted verification rows.

Run:

```bash
cargo test -p voom-control-plane artifact::verify artifact::worker
```

Expected: compile failure for missing modules.

- [ ] **Step 2: Implement durable verify worker bootstrap**

Add `ensure_builtin_verify_artifact_worker_in_tx`, mirroring Sprint 10 scan bootstrap but using name `builtin.verify_artifact`, operation `verify_artifact`, and a grant that allows the bundled direct-dispatch path. If the named worker exists and is retired, return `Conflict` rather than creating a second identity.

- [ ] **Step 3: Implement bundled worker launcher**

Clone Sprint 10’s `BundledWorkerProcess` shape with a verify-specific command resolver and dispatch method using `OperationKind::VerifyArtifact`. The launcher receives the bootstrapped durable `WorkerId` and passes it to the worker process through `VOOM_WORKER_ID`.

- [ ] **Step 4: Implement `VerifyArtifactInput` and report**

Expose:

```rust
pub struct VerifyArtifactInput {
    pub artifact_handle_id: ArtifactHandleId,
}

pub struct VerifyArtifactReport {
    pub artifact_handle_id: ArtifactHandleId,
    pub artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub worker_id: WorkerId,
    pub status: ArtifactVerificationStatus,
    pub path: PathBuf,
    pub expected_size_bytes: u64,
    pub expected_checksum: String,
    pub observed_size_bytes: Option<u64>,
    pub observed_checksum: Option<String>,
    pub error_code: Option<ErrorCode>,
    pub message: Option<String>,
}
```

- [ ] **Step 5: Implement verification orchestration**

Find exactly one live staging location, ensure the durable verify worker row, build expected facts from the artifact handle, emit `artifact.verification_started` with that `worker_id`, launch the worker with that same id, dispatch worker, persist succeeded or failed verification row with that same `worker_id`, then emit succeeded/failed event.

- [ ] **Step 6: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane artifact::verify artifact::worker
just fmt
git add crates/voom-control-plane/src/artifact/bootstrap.rs crates/voom-control-plane/src/artifact/bootstrap_test.rs crates/voom-control-plane/src/artifact/worker.rs crates/voom-control-plane/src/artifact/worker_test.rs crates/voom-control-plane/src/artifact/verify.rs crates/voom-control-plane/src/artifact/verify_test.rs crates/voom-control-plane/src/artifact/mod.rs
git commit -m "feat(control-plane): verify staged artifacts"
```

Expected: verification tests pass.

## Task 9: Host Commit Use Case

**Files:**
- Create: `crates/voom-control-plane/src/artifact/commit.rs`
- Create: `crates/voom-control-plane/src/artifact/commit_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/mod.rs`

- [ ] **Step 1: Write commit tests**

Cover:

- unverified commit rejected with `Config`;
- stale verification for a retired/different staging location rejected;
- staged-byte drift before prepare rejected with `ArtifactChecksumMismatch`;
- target exists rejected with `Config`;
- every pre-prepare rejection emits `artifact.commit_failed_pre_mutation` with no `artifact_commit_records` row;
- target path created by another process after prepare is not overwritten and transitions the pending record to `recovery_required`;
- successful commit creates pending record, promotes target, creates `FileVersion` with `ProducedBy::StagedCommit`, records target `FileLocation`, retires staging location, marks committed, and emits started/completed events;
- injected failure after prepare marks `recovery_required`;
- injected failure after target rename but before finalize commit leaves the target file visible and marks `recovery_required` with target/temp/staging existence facts;
- duplicate pending/committed/recovery owners are rejected by repo constraints.

Run:

```bash
cargo test -p voom-control-plane artifact::commit
```

Expected: compile failure for missing commit use case.

- [ ] **Step 2: Implement input/report**

Add:

```rust
pub struct CommitArtifactInput {
    pub artifact_handle_id: ArtifactHandleId,
    pub target_path: PathBuf,
}

pub struct CommitArtifactReport {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub verification_id: ArtifactVerificationId,
    pub target_path: PathBuf,
    pub temp_path: Option<PathBuf>,
    pub state: ArtifactCommitState,
    pub result_file_version_id: Option<FileVersionId>,
    pub result_file_location_id: Option<FileLocationId>,
    pub recovery_required: Option<CommitRecoveryReport>,
}
```

- [ ] **Step 3: Implement prepare transaction**

Inside a write transaction, re-read handle, source version, live staging location, latest successful verification for that exact location, target non-existence, and staged facts. If any of those preconditions fails before inserting the pending row, append `artifact.commit_failed_pre_mutation` in the same transaction and return an error envelope with `artifact_handle_id`, optional `verification_id`, `target_path`, `error_code`, and `message`; do not create an `artifact_commit_records` row. If all preconditions pass, insert `pending`, append `artifact.commit_started`, and commit before filesystem promotion.

- [ ] **Step 4: Implement promotion phase**

Outside the DB transaction, re-observe staging, copy to temp sibling, fsync temp, verify temp, install temp at the target with the no-overwrite helper, and verify target. Any failure after pending exists calls recovery transition. If staged bytes drift before copying, do not create the temp file; still transition the pending record to `recovery_required` because the durable prepare already committed and the command cannot silently discard that ownership record. If the target appears after prepare, do not replace it; keep/remove the temp path according to the recovery report and transition the pending record to `recovery_required` with an observed `target_exists: true` fact.

- [ ] **Step 5: Implement finalize transaction**

Create staged-commit `FileVersion` with same `file_asset_id`, content hash, size, `produced_from_version_id = source_file_version_id`, record target `local_path` location, retire staging artifact location, mark commit `committed`, emit `artifact.commit_completed`, and commit. If this transaction returns an error after the target rename succeeded, open a fresh recovery transaction and mark the existing pending commit `recovery_required`; because the finalize transaction rolls back atomically, the recovery report should name the target path as present and durable result ids as absent unless a later implementation proves a partial durable id escaped rollback.

- [ ] **Step 6: Implement recovery transition**

Mark the pending commit `recovery_required` with target/temp/staging existence facts and any created durable ids, emit `artifact.commit_recovery_required`, and return an error/report that prevents CLI success.

- [ ] **Step 7: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane artifact::commit
just fmt
git add crates/voom-control-plane/src/artifact/commit.rs crates/voom-control-plane/src/artifact/commit_test.rs crates/voom-control-plane/src/artifact/mod.rs
git commit -m "feat(control-plane): commit verified staged artifacts"
```

Expected: commit tests pass.

## Task 10: Artifact Inspection Use Cases

**Files:**
- Create: `crates/voom-control-plane/src/artifact/inspect.rs`
- Create: `crates/voom-control-plane/src/artifact/inspect_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/mod.rs`

- [ ] **Step 1: Write inspection tests**

Cover list by state, list limit ordering, show staged/verified/committed/failed/recovery-required rows, missing artifact `NotFound`, and recovery filesystem facts for target/temp/staging paths.

Run:

```bash
cargo test -p voom-control-plane artifact::inspect
```

Expected: compile failure for missing inspection module.

- [ ] **Step 2: Implement read models**

Create `ArtifactSummary`, `ArtifactDetail`, `VerificationSummary`, and `CommitSummary` with ids, state/status, paths, facts, and recovery fields.

- [ ] **Step 3: Implement list/show**

Use repository list methods and direct path observation for recovery visibility. Keep events out of inspection state derivation; durable artifact/verification/commit rows remain source of truth. The state filter accepted by `voom artifact list --state` must include `staged`, `verified`, `committed`, `failed`, and `recovery_required`; `verified` means latest successful verification exists for the current live staging location and no commit owner exists.

- [ ] **Step 4: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane artifact::inspect
just fmt
git add crates/voom-control-plane/src/artifact/inspect.rs crates/voom-control-plane/src/artifact/inspect_test.rs crates/voom-control-plane/src/artifact/mod.rs
git commit -m "feat(control-plane): inspect staged artifacts"
```

Expected: inspection tests pass.

## Task 11: CLI Artifact Command Family

**Files:**
- Modify: `crates/voom-cli/src/cli.rs`
- Create: `crates/voom-cli/src/commands/artifact.rs`
- Create: `crates/voom-cli/src/commands/artifact_test.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Modify: `crates/voom-cli/src/main.rs`

- [ ] **Step 1: Write CLI mapping tests**

Cover command names, state enum parsing for `staged`, `verified`, `committed`, `failed`, and `recovery_required`, path string serialization, error-code mapping, and exactly one JSON envelope per command.

Run:

```bash
cargo test -p voom-cli commands::artifact
```

Expected: compile failure for missing command.

- [ ] **Step 2: Add CLI parser**

Add:

```rust
Artifact(ArtifactCommand)
```

with subcommands `StageCopy`, `Verify`, `Commit`, `List`, and `Show`, matching the spec flags exactly.

- [ ] **Step 3: Add envelope DTOs**

Serialize stable ids as integers, paths as strings, statuses/states as snake_case strings, and omit optional fields only when they are not relevant to that artifact state.

- [ ] **Step 4: Add main dispatch**

Map stage-copy/verify/commit/list/show to control-plane use cases. Exit code `1` remains `BAD_ARGS`; runtime errors use exit code `2`.

- [ ] **Step 5: Run CLI unit tests and commit**

Run:

```bash
cargo test -p voom-cli commands::artifact
just fmt
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/commands/artifact.rs crates/voom-cli/src/commands/artifact_test.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/main.rs
git commit -m "feat(cli): add artifact commands"
```

Expected: CLI mapping tests pass.

## Task 12: End-To-End CLI Snapshots And Recovery Coverage

**Files:**
- Create: `crates/voom-cli/tests/artifact_envelope.rs`
- Create: `crates/voom-control-plane/tests/staged_artifact_flow.rs`
- Create/Update: `crates/voom-cli/tests/snapshots/artifact_envelope__*.snap`

- [ ] **Step 1: Write integration tests**

Cover:

- scan fixture media, stage-copy, verify, commit, show committed;
- list and show staged, verified, committed, failed verification, and recovery-required states;
- unverified commit rejection;
- staged-byte drift rejection;
- recovery-required injection after promotion begins;
- representative CLI failure envelopes for missing artifact, failed verification, target path already exists, and recovery-required commit failure.

Run:

```bash
cargo test -p voom-cli --test artifact_envelope
cargo test -p voom-control-plane --test staged_artifact_flow
```

Expected: snapshot failures or compile failures until command wiring is complete.

- [ ] **Step 2: Add test helpers**

Reuse `tiny.mp4`, `voom` binary, `voom-ffprobe-worker`, and new `voom-verify-artifact-worker` binary paths. Redact temp paths, hashes, timestamps, and worker ids consistently with `scan_envelope.rs`.

- [ ] **Step 3: Review snapshots**

Run:

```bash
cargo insta review
```

Accept snapshots only after confirming each envelope includes ids needed for follow-up inspection, including `verification_id` on failed verification output and `commit_record_id`, `target_path`, `temp_path`, `target_exists`, `temp_exists`, and `staging_exists` on recovery-required output.

- [ ] **Step 4: Run integration tests and commit**

Run:

```bash
cargo test -p voom-cli --test artifact_envelope
cargo test -p voom-control-plane --test staged_artifact_flow
just fmt
git add crates/voom-cli/tests/artifact_envelope.rs crates/voom-control-plane/tests/staged_artifact_flow.rs crates/voom-cli/tests/snapshots
git commit -m "test: cover staged artifact CLI flow"
```

Expected: integration tests and snapshots pass.

## Task 13: Documentation, Forbidden-Marker Scan, And CI

**Files:**
- Create: `docs/superpowers/specs/2026-05-25-voom-sprint-11-closeout.md`
- Modify: `docs/specs/voom-control-plane-design.md` if the implemented command contract or event table needs architectural cross-reference.

- [ ] **Step 1: Write closeout evidence**

Create the closeout with rows for schema, stage-copy, verification worker, commit, recovery, CLI snapshots, and CI. Include exact commands and pass/fail outcomes.

- [ ] **Step 2: Run forbidden-marker scan**

Run:

```bash
rg -n -e 'TO''DO' -e 'TB''D' -e 'place''holder' -e 'fake transcode' -e 'in-process verify' docs crates migrations
```

Expected: no Sprint 11 forbidden marker text outside intentional historic specs or fixture names. Any hit in new code/docs must be resolved or explicitly documented in closeout as historical/non-Sprint-11.

- [ ] **Step 3: Run full CI**

Run:

```bash
just ci
```

Expected: pass with no skipped steps.

- [ ] **Step 4: Commit closeout**

Run:

```bash
git add docs/superpowers/specs/2026-05-25-voom-sprint-11-closeout.md docs/specs/voom-control-plane-design.md
git commit -m "docs: close out sprint 11 staged artifact flow"
```

Expected: closeout is committed after verified evidence exists.

## Self-Review Notes

- Spec coverage: stage-copy, verification worker, typed payloads, persistence tables, host commit phases, add-only semantics, CLI list/show, events, error behavior, tests, recovery visibility, and closeout evidence are represented by tasks.
- Important conflict surfaced: the migration plan recreates `file_versions` to change a CHECK constraint. Execution should validate SQLite foreign-key behavior carefully because dependent tables reference `file_versions`.
- Important risk surfaced: Sprint 10 worker-launch code is scan-specific. Task 8 duplicates the pattern narrowly first; a later simplification pass can extract a shared bundled-worker launcher once the verify path is stable.
- Important risk surfaced: recovery injection must not become a production CLI knob. Keep it test-only or trait-driven from control-plane tests.
