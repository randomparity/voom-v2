# Scheduling policy and safety policy CRUD (T12, #281)

Status: draft
Related: ADR 0028, parent #269 (Workstream C), spec
`docs/specs/voom-control-plane-design.md` → *Security And Safety*, *Policy Model*.
Depends on: T9 backup worker (#278, merged — supplies the backup-before-mutation
execute hook this spec drives).

## Problem

The pre-daemon safety baseline (design spec → *Security And Safety*) requires
operators to configure and inspect, from the CLI, durable **scheduling policy**
and **safety policy** records before any daemon loop may auto-schedule real media
mutation. Today neither record type exists: no tables, no repositories, no CLI,
and `compliance execute` reads no safety configuration. The spec's schema
inventory already reserves `scheduling_policies` and `safety_policies`.

The safety policy is the gate every future automation decision consults. Its
fields (design spec, verbatim):

- which operation kinds the daemon may auto-execute;
- whether backup is required before a mutation;
- whether approval is required before execution or commit;
- which commit modes are allowed (`add_only` before replace/delete/archive);
- what verification level is required before commit;
- whether failed, partial, or recovery-required records block later automation.

Read semantics are **fail-closed**: "If a policy is missing, too old, or lacks a
field required by an operation, the daemon records a blocked issue instead of
guessing." No synthesized defaults.

## Goals

1. Durable `scheduling_policies` and `safety_policies` tables (migration 0020).
2. Two voom-store repositories with create / get / list / update / delete.
3. `voom scheduling-policy …` and `voom safety-policy …` CLI CRUD, each emitting
   the standard JSON envelope.
4. `compliance execute` **reads** a named safety policy (opt-in
   `--safety-policy <slug>`) and enforces it fail-closed at the hooks that
   already exist: the #278 backup-before-mutation gate, the plan's verify step,
   the add-only commit path, and durable failed / recovery-required records.
5. Fail-closed tests: a missing or stale safety policy, or one that forbids the
   run, produces a blocked issue and a non-zero exit — never a synthesized
   default and never a dispatch.

## Non-goals

- No daemon, watcher, or scheduler loop. Scheduling policy has **no reader** yet;
  it is durable configuration a future daemon will consume. Only CRUD is in
  scope for it.
- No approval subsystem. `approval_required = true` is enforced as an
  unconditional block (there is no approval-grant path pre-daemon).
- No change to the default (`--safety-policy` absent) `compliance execute`
  behavior. The gate is opt-in so existing manual operator flows and their
  snapshots are unaffected. A future daemon always passes a policy.
- No `retention_policies` / `external_system_policies` / `issue_policies`
  (separate reserved tables, other tickets).

## Schema (migration 0020)

Both tables are `STRICT`, keyed by an integer id with a unique `slug`, and carry
a `schema_version` for fail-closed staleness detection plus `created_at` /
`updated_at`.

`scheduling_policies` — the parseable, enforceable subset of the design spec's
`schedule` example (`priority`, `copy_window`, `large_jobs night_only`,
`pause_when node.health == degraded`). Worker-preference (`prefer local_gpu_for`)
and budget (`cloud_egress_budget`) forms are omitted as speculative — no
subsystem consumes them and adding them would be phantom configuration.

| column | type | notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `slug` | TEXT UNIQUE | stable name |
| `display_name` | TEXT | |
| `schema_version` | INTEGER | current = 1 |
| `priority` | TEXT | `newest_first` \| `oldest_first` \| `smallest_first` \| `largest_first` |
| `copy_window` | TEXT NULL | `HH:MM-HH:MM`, validated on write |
| `large_jobs_night_only` | INTEGER | 0/1 |
| `pause_on_degraded_node` | INTEGER | 0/1 |
| `created_at` / `updated_at` | TEXT | ISO-8601 |

`safety_policies`:

| column | type | notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `slug` | TEXT UNIQUE | stable name |
| `display_name` | TEXT | |
| `schema_version` | INTEGER | current = 1; a row whose version ≠ the binary's current version is **stale** ⇒ fail-closed |
| `auto_execute_operations` | TEXT | JSON array of `OperationKind` wire strings; empty ⇒ nothing may auto-execute |
| `backup_required` | INTEGER | 0/1 |
| `approval_required` | INTEGER | 0/1 |
| `allowed_commit_modes` | TEXT | JSON array of commit-mode strings (`add_only`/`replace`/`delete`/`archive`); empty ⇒ no commit permitted |
| `verification_level` | TEXT | `none` \| `quick_decode` \| `full` |
| `block_on_failed_records` | INTEGER | 0/1 |
| `block_on_recovery_required_records` | INTEGER | 0/1 |
| `created_at` / `updated_at` | TEXT | ISO-8601 |

The two JSON-array columns hold arrays of scalar enum strings (not structs), so
the ADR-0013 `deny_unknown_fields` payload contract does not apply; the repo
validates every element against the enum vocabulary on write and rejects unknown
tokens (fail-loud), so the DB never holds an invalid value.

"partial records" from the spec's phrasing is folded into the two boolean fields:
there is no durable record status distinct from *failed* (failed backup / failed
commit) and *recovery-required* (a commit left in the recovery-required state); a
partially-completed run leaves one or both, so the two fields cover the intent
without a phantom third field.

## The safety gate (compliance execute)

`ComplianceExecutionOptions` gains `safety_policy_slug: Option<String>` and
`backup_root: Option<PathBuf>` (the latter already exists). The CLI exposes
`--safety-policy <slug>` and `--backup-root <path>` on `compliance execute`.

When `safety_policy_slug` is `Some`, before any dispatch
`execute_compliance_policy_with_options` evaluates the policy against the
generated plan and options and collects **blocks**. It fail-closes on:

1. **Missing** — no policy with that slug ⇒ block.
2. **Stale** — `row.schema_version != SAFETY_POLICY_SCHEMA_VERSION` ⇒ block.
3. **Approval required** — `approval_required` ⇒ block (no approval path exists).
4. **Commit mode** — `add_only ∉ allowed_commit_modes` ⇒ block (the execute path
   commits add-only; a policy that does not permit `add_only` forbids it).
5. **Backup required** — `backup_required && backup_root.is_none()` ⇒ block. When
   backup is required *and* a `--backup-root` is supplied, the run proceeds and
   the existing #278 backup-before-mutation gate performs the backups. This is
   the wire from safety policy to the T9 hook.
6. **Verification** — `verification_level != none` and the plan contains no
   `verify_artifact` node ⇒ block.
7. **Auto-execute** — any planned **mutating** operation whose `OperationKind`
   is not in `auto_execute_operations` ⇒ block (one per operation).
8. **Failed records** — `block_on_failed_records` and a failed backup exists for
   any file version the plan targets ⇒ block.
9. **Recovery-required records** — `block_on_recovery_required_records` and a
   commit record in the recovery-required state exists for any targeted file
   version ⇒ block.

On one or more blocks the gate opens a durable `policy_noncompliant` issue
(status `open`, dedupe key `safety_blocked:v1:policy=<slug>:pv=<id>:is=<id>`)
whose body enumerates the block reasons, then returns
`VoomError::PolicyValidationError`. `execute` surfaces exit code 2 and emits the
error envelope. Nothing is dispatched. When there are zero blocks the run
proceeds exactly as today, with `backup_root` threaded through.

The gate reads only: it opens the issue in its own transaction (mirroring the
existing issue-application transaction boundary) and never mutates policy rows or
media.

## Testing

Store repos (unit, sibling `_test.rs`): create → get/list round-trip for every
field; slug-unique conflict; update replaces all mutable fields and bumps
`updated_at`; delete; unknown-operation / unknown-commit-mode / bad
`verification_level` / bad `copy_window` rejected on write; empty-array columns
round-trip.

Safety gate (control-plane, sibling `_test.rs`): each block reason triggers a
block and opens exactly one issue and dispatches nothing — missing policy, stale
`schema_version`, `approval_required`, `add_only` excluded, `backup_required`
with no root, verification required with no verify node, a planned operation not
in `auto_execute_operations`, a failed backup with `block_on_failed_records`, a
recovery-required commit with `block_on_recovery_required_records`. Positive
path: a permissive policy whose fields all pass runs to the same outcome as no
policy, and `backup_required` + `backup_root` threads the root through.

CLI (insta envelopes): create/list/show/update/delete for both commands; unknown
slug on show/delete is `NOT_FOUND`; a safety-blocked `execute` emits the error
envelope with the block reasons; a permissive `execute` is unchanged.

Migration count assertion bumps 18 → 19; health/init snapshots that carry
`migration_count` regenerate.
