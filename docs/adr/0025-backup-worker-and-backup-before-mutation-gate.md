---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0025 — Real backup worker, durable backup records, and a backup-before-mutation gate

## Context

Backup was scaffolding only. `OperationKind::BackUpFile` (`"back_up_file"`),
`FailureClass::BackupFailure` / `ErrorCode::BackupFailure` (`"BACKUP_FAILURE"`),
and a `file_locations.kind='backup_path'` enum value all existed, but there was:

- no real worker — only `fake-backup-store` (`crates/voom-fakes/src/bin/`), driven
  solely by the conformance harness and the `#[cfg(test)]`-only simulated-workflow
  DAG;
- no durable `backups` table (migrations stopped at `0016`);
- no producer of `BACKUP_FAILURE` (the code only *deserialized* it at
  `workflow/execution/dispatch.rs`);
- no CLI inspection and no backup evidence in reports.

Issue #278 (Sprint 17, T9) asks for a real out-of-process backup worker, durable
records, execute-path integration ("backup before a mutation when the safety policy
requires it"), `voom backup list|show`, and backup evidence in `compliance report`,
plus conformance and recovery tests.

Two facts shape the design:

- **The production execute path is add-only today.** The real remux/transcode/audio
  workers never write their input path (they reject `overwrite=true`), and
  `artifact/commit/promote.rs` installs via hard-link + temp-remove — it never
  replaces an existing target. Destructive replace/delete/archive automation is
  deliberately disabled pending the safety model (design doc §Security And Safety).
  So "backup-before-mutation" in V1 means *a defensive copy of the source bytes
  taken before a mutating operation consumes them*, not "before an in-place
  overwrite" (there is none yet).
- **The safety-policy subsystem (T12/#281) does not exist.** There is no
  `safety_policies` table and no policy read point in the execute path. #278 does not
  block on T12; T12 will later supply the durable "backup required" trigger. Until
  then the gate needs a clear, explicit, documented trigger.

## Decision

**1. Backup is a real out-of-process worker (`voom-backup-worker`), modeled on
`voom-verify-artifact-worker`.** It speaks the existing HTTP/NDJSON worker protocol
(ADR 0002), handles `OperationKind::BackUpFile`, copies the source file to a
destination directory on the local filesystem (the V1 target), computes size and a
BLAKE3 checksum while copying, `fsync`s the copy and its parent directory before
reporting success (a `verified` record means the bytes are on stable storage), and
emits one `Progress` frame plus a terminal `Result`/`Error` frame. The request
carries a fully-qualified `destination_path`; if it already exists with a matching
size+checksum the worker treats it as an idempotent success rather than a clobber
error. It is a bundled `WorkerKind::Local` subprocess launched next
to the running binary (override `VOOM_BACKUP_WORKER_BIN`), dispatched directly with a
`BackUpFileDispatcher` trait exactly as verify-artifact is — not scheduled through a
ticket/lease/worker-row. No new `OperationKind`, `WorkerKind`, `FailureClass`, or
`ErrorCode` is needed; all already exist.

**2. Typed wire contract lives in `voom-worker-protocol`** (`operations/backup.rs`),
matching the verify-artifact convention: `BackUpFileRequest { source_path,
destination_dir }` and `BackUpFileResult { status, provider, provider_version,
destination_path, size_bytes, checksum }`, both `#[serde(deny_unknown_fields)]`,
evolving additively (ADR 0013). These are wire types (never persisted to a DB
column), so they are not added to `payload-contract-scope.txt`.

**3. Durable records: a new `backups` table (migration `0018`) with a
`SqliteBackupRepo`.** Records are scalar-only (no JSON column). A backup is written
as `pending` before the copy and transitioned to `verified` or `failed` after, so a
crash mid-backup leaves a recoverable `pending` row (`finished_at IS NULL`). The
record carries `source_file_version_id`, `job_id`, `ticket_id`, the destination,
size, checksum, provider, status, and — on failure — `failure_class`/`error_code`/
`message`. `BackupId` is a database-generated ROWID newtype in `voom-core`.

**4. Execute-path gate: an explicit `--backup-root <DIR>` option threaded through
`ComplianceExecutionOptions`.** When set, a shared helper backs up the source of
every mutating operation (remux, transcode-video, transcode-audio, extract-audio)
*after source selection and before the worker dispatch*. Because the phase-barrier
coordinator requeues tickets on retriable failures — and `BackupFailure` is itself
retriable — the helper is **idempotent**: it short-circuits when a `verified` backup
already exists for `(ticket_id, source_file_version_id)` (a partial-unique index is
the durable backstop), and the destination path is namespaced by
`source_file_version_id` so distinct same-basename sources never collide. Without
this, a transient upstream retry would re-run the backup and clobber-fail. The gate
is **fail-closed**:
a backup failure writes a `failed` backup record and returns
`VoomError::BackupFailure`, which aborts that operation and is recorded on the
ticket's failure with `FailureClass::BackupFailure`. This is the first real
`BACKUP_FAILURE` producer. Presence of the option (`Option<PathBuf>`) is the trigger:
`Some(dir)` enables backup and names the destination; `None` skips it.

**5. Backup evidence is attached at the control-plane report layer, not in the pure
planner.** `voom_plan::generate_compliance_report` stays pure (plan-only, so
`report_hash` is unaffected). `ComplianceReportData` gains a `backups:
Vec<BackupEvidence>` field populated by reading `SqliteBackupRepo` for the file
versions resolved from the report's durable input set (not from plan node fields).
Evidence reflects durable state: empty before any backup runs, populated after an
execute run with `--backup-root`.

**6. `voom backup list [--limit] [--status] | show --backup-id` inspection**, read-side
(`ControlPlane::open`, never migrates — ADR 0003), ordered `created_at ASC, id ASC`
(deterministic, matching the existing list idiom), emitting the standard JSON
envelope.

## Consequences

- A real `BACKUP_FAILURE` producer exists and is exercised end to end.
- The backup destination and "whether to back up" are driven by an explicit CLI
  option, **not** a durable policy. **Coupling left for T12 (#281):** replace the
  `--backup-root` option trigger with a durable safety-policy read
  (`backup_required` + destination) at the same execute-path call site. This is a
  documented follow-up, not a blocker.
- Backup is a synchronous side-effect inside each `execute_*_core`, not a planned DAG
  phase. The `#[cfg(test)]`-only simulated-workflow `backup` node and its
  `fake-backup-store` conformance fixture are left untouched.
- Migration `0018` is used (not the next-free `0017`) by cross-agent coordination:
  the concurrent #287 owns `0017`. This leaves a transient numbering gap on this
  branch until #287 merges; the hand-rolled `MIGRATOR` and the strictly-increasing
  guard tolerate gaps.
- No new event variants are added to `voom-events`. The durable `backups` row is the
  source of truth; the operation's existing failure event carries the
  `BackupFailure` class when the gate aborts. Dedicated backup lifecycle events are a
  possible future refinement.
- V1 has **no backup retention/cleanup**: copies accumulate under the operator's
  `--backup-root`, and a full volume surfaces (fail-closed) as a `BackupFailure` that
  aborts the mutation. The operator owns the backup volume. Retention/pruning is a
  documented future concern.

## Considered & rejected

- **Add `BackUpFile` as a planned `voom_plan` phase / `PlanOperationKind`.** Rejected
  for V1: it is a much larger change to the planner and phase-barrier coordinator (a
  hotspot), and production plans have no backup node. A synchronous
  before-mutation side-effect is simpler and matches where the source path is
  resolved.
- **Reuse `artifact_locations.kind='backup'` / `file_locations.kind='backup_path'`
  instead of a new table.** Rejected: those tables model artifact/version locations,
  not backup operation outcomes (status, checksum, failure, provider, timing). A
  purpose-built append-style table is clearer and gives the CLI/report a direct
  source.
- **Always back up before every mutation (no trigger).** Rejected: the current
  pipeline is add-only, so unconditional backups would waste I/O and storage; the
  explicit trigger keeps V1 opt-in until T12's policy exists.
- **Put backup evidence inside `voom_plan::generate_compliance_report`.** Rejected:
  it would make the pure planner read the DB and would fold execution state into
  `report_hash`. Attaching at `ComplianceReportData` keeps the planner pure and the
  hash stable.
- **Add backup lifecycle events to `voom-events`.** Deferred: it widens the durable
  payload-contract surface (owned partly by #287) for observability the `backups`
  table already provides.
