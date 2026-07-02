# Spec: Real backup worker, records, report, CLI (T9 / #278)

Status: draft · Date: 2026-07-02 · Issue: #278 · ADR:
[0025](../adr/0025-backup-worker-and-backup-before-mutation-gate.md)

## Goal

Backup becomes a real out-of-process worker with durable records, execute-path
integration that produces the first real `BACKUP_FAILURE`, CLI inspection, and
backup evidence in `compliance report`.

## Success criteria (falsifiable)

1. `voom-backup-worker` binary starts, prints `BOUND addr=<socket>`, serves
   `POST /v1/operations` for `back_up_file`, copies a source file to a destination
   directory, returns `{destination_path, size_bytes, checksum}`, and shuts down on
   stdin EOF. An integration test spawns the real binary and asserts the contract.
2. A missing source file and an unwritable destination each return a terminal
   `Error` frame with class `BACKUP_FAILURE` (or a more specific I/O class for a
   missing source), never a panic or a silent success. A destination that already
   exists with a matching size+checksum is treated as an idempotent success (re-copy
   short-circuit), not a clobber failure.
3. Migration `0018_backups.sql` creates a `STRICT` `backups` table; the migration
   inventory test, embedded-count literal, and a schema-shape assertion all pass.
4. `SqliteBackupRepo` inserts a `pending` record, transitions it to `verified` /
   `failed`, and supports `get`, `list` (`created_at ASC, id ASC LIMIT ?`),
   `list_by_file_version`, and a pending-recovery query. Unit tests cover each,
   including the both-or-neither status CHECK.
5. `voom compliance execute --backup-root <DIR>` backs up the source of every
   mutating operation before dispatch: on success a `verified` backup record exists
   for the source file version; on backup failure the operation aborts with
   `BACKUP_FAILURE` and a `failed` record exists. Without `--backup-root`, no backup
   record is written and behaviour is unchanged.
6. `voom backup list` and `voom backup show --backup-id N` emit the standard
   envelope; `show` on an unknown id returns `NOT_FOUND` (exit 2). Insta snapshots
   are committed.
7. `compliance report` output includes a `backups` evidence array reflecting durable
   backup state for the report's inputs.
8. `just ci` is green: fmt, clippy `-D warnings`, `check-test-layout`,
   `check-paused-time-db`, `check-payload-deny-unknown`, tests, doc, deny, audit.

## Non-goals

- No durable safety policy (T12/#281). The `--backup-root` option is the V1 trigger.
- No object-store / remote backup target (local filesystem only for V1).
- No backup planned-DAG phase; no `PlanOperationKind::BackUpFile`.
- No new `voom-events` variants; no change to `fake-backup-store` or its conformance.
- No un-gating of env-gated real-media integration tests.

## Design

### Wire contract — `crates/voom-worker-protocol/src/operations/backup.rs`

```rust
#[serde(deny_unknown_fields)] BackUpFileRequest { source_path: String, destination_path: String }
#[serde(rename_all = "snake_case")] enum BackUpFileStatus { BackedUp }
#[serde(deny_unknown_fields)] BackUpFileResult {
    status: BackUpFileStatus, provider: String, provider_version: String,
    destination_path: String, size_bytes: u64, checksum: String,
}
```
Registered via `pub(crate) mod backup;` in `operations/mod.rs` and re-exported from
`lib.rs`. Additive-only (ADR 0013). Wire types only — not added to
`payload-contract-scope.txt`.

### Worker crate — `crates/voom-backup-worker`

Mirror `voom-verify-artifact-worker`: `Cargo.toml` (deps `voom-core`,
`voom-worker-protocol`, `blake3`, `serde_json`, `time`, `tokio` fs/io/rt;
dev `tempfile`; `[lints] workspace = true`; no `[[bin]]`), `src/main.rs` (verbatim
startup/watchdog pattern), `src/lib.rs`, `src/handler.rs` (+`_test`),
`src/observe.rs` or `src/backup.rs` (the copy+hash I/O, +`_test`),
`tests/backup_worker.rs` (spawn-binary contract test). Add to root `Cargo.toml`
`[workspace] members`; no `[workspace.dependencies]` entry (leaf subprocess binary).

Handler: reject `operation != BackUpFile` with `ProtocolError::UnknownOperation`;
decode payload to `BackUpFileRequest` (decode failure → terminal `Error`,
`MalformedWorkerResult`). The request carries a fully-qualified `destination_path`
(the control plane builds it collision-free, see below), not just a directory. The
handler creates the destination's parent dir, stream-copies source →
`destination_path` computing size + BLAKE3, `fsync`s the destination file and its
parent directory before returning (durability: a `verified` record must mean the
bytes are on stable storage), and returns `BackUpFileResult`. **Idempotent re-copy:**
if `destination_path` already exists, the handler reads it back and, when its
size+checksum match the freshly-copied source, returns success (`BackedUp`) instead
of failing — so a retried dispatch is a no-op, not a clobber error. A pre-existing
destination whose bytes differ is a `BackupFailure` (refuse to overwrite a
non-matching file). Domain error enum `BackUpFileError { failure_class(),
error_code(), payload(), Display, Error }`: missing source → `ArtifactUnavailable`;
copy/fsync failure and mismatched-existing-destination → `BackupFailure`.

### Durable records

`migrations/0018_backups.sql` (STRICT, no rebuild dance):

```sql
CREATE TABLE backups (
    id                     INTEGER PRIMARY KEY,
    source_file_version_id INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    job_id                 INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    ticket_id              INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    provider               TEXT NOT NULL,
    destination_path       TEXT NOT NULL,
    size_bytes             INTEGER CHECK (size_bytes IS NULL OR size_bytes >= 0),
    checksum               TEXT,
    status                 TEXT NOT NULL CHECK (status IN ('pending','verified','failed')),
    failure_class          TEXT,
    error_code             TEXT,
    message                TEXT,
    started_at             TEXT NOT NULL,
    finished_at            TEXT,
    created_at             TEXT NOT NULL,
    CHECK (
        (status='pending'  AND size_bytes IS NULL AND checksum IS NULL
             AND failure_class IS NULL AND error_code IS NULL AND message IS NULL AND finished_at IS NULL)
     OR (status='verified' AND size_bytes IS NOT NULL AND checksum IS NOT NULL
             AND failure_class IS NULL AND error_code IS NULL AND message IS NULL AND finished_at IS NOT NULL)
     OR (status='failed'   AND failure_class IS NOT NULL AND error_code IS NOT NULL
             AND message IS NOT NULL AND finished_at IS NOT NULL)
    )
) STRICT;
CREATE INDEX backups_by_file_version ON backups (source_file_version_id, id DESC);
CREATE INDEX backups_by_job ON backups (job_id, id DESC);
-- Idempotency: at most one verified backup per (ticket, source version) so a
-- retried operation reuses the existing copy instead of writing a duplicate row.
CREATE UNIQUE INDEX backups_verified_key
    ON backups (ticket_id, source_file_version_id) WHERE status = 'verified';
```

`SqliteBackupRepo` in `crates/voom-store/src/repo/media/backups.rs` (template:
`repo/execution/workflow_summaries.rs`): `NewBackup`/`Backup` structs, `BackupStatus`
enum (`Pending|Verified|Failed`, `as_str`/`parse`), methods `insert_pending_in_tx`
/`insert_pending`, `mark_verified_in_tx`/`mark_verified`, `mark_failed_in_tx`
/`mark_failed`, `get`, `list`, `list_by_file_version`, `list_pending`. Wire into
`repo/media/mod.rs` and `repo/mod.rs` re-exports. `BackupId` newtype via `define_id!`
in `crates/voom-core/src/taxonomy/ids.rs`. Register migration in `migrator.rs`,
`tests/migration_inventory.rs` (`EXPECTED_MIGRATION_FILES`), and bump the
`schema_test.rs` count literal `16 → 17`; add a schema-shape assertion for `backups`.

### Execute-path gate

Thread `backup_root` (`Option<PathBuf>` owned; `Option<&Path>` in the `Copy`
`OperationAdapterContext`) through the seven points identified in the ADR:
`ComplianceExecutionOptions` → `From<…> for WorkflowExecutorOptions`
(→ `WorkflowArtifactRoots.backup_root`) → `dispatch_options()` →
`WorkflowDispatchOptions` → `OperationAdapterContext.backup_root` →
each `Execute*Input.backup_root` (populated in the `*_input_for_workflow_ticket`
builders) → each `execute_*_core`.

New `crates/voom-control-plane/src/backup/` module: `BackUpFileDispatcher` trait +
`BundledBackUpFileDispatcher` (mirrors `artifact/verify.rs` + `artifact/worker.rs`,
launching `voom-backup-worker` via `bundled_worker_command_from(…,
"voom-backup-worker", …)`, env `VOOM_BACKUP_WORKER_BIN`), and a use-case helper
`back_up_source_before_mutation(cp, backup_root, &selected, job_id, ticket_id,
source_file_version_id) -> Result<(), VoomError>`:

1. **Idempotency short-circuit:** if a `verified` backup already exists for
   `(ticket_id, source_file_version_id)`, return `Ok(())` without re-copying. This is
   the primary retry guard — the phase-barrier coordinator requeues tickets on
   retriable failures (and `BackupFailure` is itself retriable), so `execute_*_core`
   (and thus this helper) is re-entered on retry. Without the short-circuit a
   transient upstream blip would re-run the backup and a duplicate would be written.
2. **Collision-free destination:** the destination is
   `<backup_root>/<source_file_version_id>/<source_basename>`, so distinct sources
   that share a basename (common in a library) never map to the same path.
3. `insert_pending` backup record (own tx).
4. dispatch backup worker (`BackUpFileRequest { source_path: selected.canonical_path,
   destination_path }`). The worker's own re-copy short-circuit (matching
   size+checksum) makes a redundant dispatch safe even if step 1 races.
5. success → `mark_verified` (size/checksum/destination); failure → `mark_failed`
   (class/code/message) then return `VoomError::BackupFailure(message)`. The
   `backups_verified_key` unique index is the durable backstop against duplicate
   verified rows under concurrency.

Called from each `execute_*_core` immediately after `source::select_source`, guarded
by `if let Some(root) = &input.backup_root`. For testability the dispatcher is
injected the same way verify's is (a `&dyn BackUpFileDispatcher` parameter on the
execute functions, real impl wired in the `workflow.rs` adapters, fake in tests).

### Report evidence

`ComplianceReportData` (`cases/policy/compliance.rs`) gains `backups:
Vec<BackupEvidence>`. `BackupEvidence` is a serializable view
(`source_file_version_id, provider, destination_path, size_bytes, checksum, status,
created_at`). Evidence is keyed off the file versions resolved from the report's
**input set** (`generate_compliance_report` already loads the durable input set and
plans it), not off `ExecutionPlan` node fields — the plan is not assumed to carry
`source_file_version_id`. The exact input-set → file-version accessor is confirmed
during implementation; if the input set does not resolve to durable file-version ids
at report time, evidence is scoped to the job's backups instead and this is recorded
in the ADR consequences. Rows sorted deterministically (`created_at ASC, id ASC`).
`ControlPlane` gains a `pub(crate) backups: SqliteBackupRepo` field.

### CLI

`voom backup list [--limit N=100] [--status pending|verified|failed] | show
--backup-id N`. New `cli.rs` `BackupCommand` enum + `Command::Backup` arm,
`commands/backup/backup.rs` handler (mirror `media/bundle.rs`), `dispatch_backup` in
`main.rs`. New `ControlPlane::list_backups`/`get_backup` read-side case wrappers.
Insta test `crates/voom-cli/tests/backup_envelope.rs`.

## Test plan

- Protocol: `backup_test.rs` — serde round-trip + `deny_unknown_fields` rejection.
- Worker: handler unit tests (success, missing source, bad payload, idempotent
  re-copy when destination already matches, mismatched-existing-destination →
  `BackupFailure`) + `observe/backup` copy+hash unit tests + `tests/backup_worker.rs`
  spawn contract.
- Store: `backups_test.rs` — insert/transition/get/list/list_pending + CHECK
  violations rejected + `backups_verified_key` uniqueness rejects a duplicate
  verified row.
- Execute: control-plane test with an injected fake `BackUpFileDispatcher` — verified
  record on success; abort + `failed` record + `BACKUP_FAILURE` on dispatcher error;
  no record without `--backup-root`; **retry idempotency** — re-running the helper for
  the same `(ticket, source version)` short-circuits (no duplicate row, no clobber
  abort); two sources sharing a basename get distinct destinations.
- Report: evidence appears for a backed-up file version.
- CLI: `backup_envelope.rs` list/show/not-found snapshots.
- Recovery: a `pending` record with `finished_at IS NULL` is returned by
  `list_pending` (crash-recovery visibility).

## Operational limitations (V1)

- **No backup retention/cleanup.** Every `execute --backup-root` copies full source
  files into the backup root; V1 does not prune, dedup across runs, or bound total
  size. The operator owns the backup volume. This is a documented V1 limitation, not
  a bug.
- **Out-of-space is a fail-closed abort.** A full backup volume surfaces as a
  `BackupFailure`, which (fail-closed) aborts the mutating operation and is recorded
  on the ticket. This is intentional — a mutation must not proceed when its requested
  backup cannot be written.

## Rollback / cleanup

Additive migration, new crate, new table, new CLI command, new opt-in option — no
data migration. Reverting removes the crate/table/command; no destructive change to
existing rows. Copied backup bytes under the operator's `--backup-root` are left in
place (operator-managed, per the limitation above).
