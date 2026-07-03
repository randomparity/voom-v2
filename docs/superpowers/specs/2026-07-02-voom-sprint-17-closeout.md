---
name: voom-sprint-17-closeout
description: Sprint 17 closeout for the Real Media CLI milestone — the daemon-readiness matrix mapping every Sprint 18-20 daemon input, policy, action, and recovery path to an existing CLI/API command and a golden/e2e test in the merged tree, plus the pre-daemon safety baseline and the explicitly deferred (daemon-owned) behaviors with their owning future sprint.
status: complete
date: 2026-07-02
sprint: 17
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-06-05-voom-sprint-17-slice-operator-execution-design.md
  - docs/superpowers/specs/2026-06-05-voom-sprint-16-closeout.md
  - docs/runbooks/README.md
  - docs/runbooks/operator-real-media-execution.md
  - docs/runbooks/migration-rollback.md
  - docs/adr/0017-verify-artifact-dsl-operation.md
  - docs/adr/0018-terminal-failure-issue-auto-open.md
  - docs/adr/0019-commit-gate-lineage-commit-check.md
  - docs/adr/0025-backup-worker-and-backup-before-mutation-gate.md
  - docs/adr/0027-library-root-and-scan-configuration.md
  - docs/adr/0028-scheduling-and-safety-policy-crud.md
  - docs/adr/0029-external-system-registration-health-and-sync.md
  - docs/adr/0030-issue-action-cli.md
  - docs/adr/0031-keyset-cursor-pagination.md
  - docs/adr/0032-video-and-quality-scoring-profile-management.md
---

# VOOM Sprint 17 Closeout — Real Media CLI Milestone Daemon-Readiness Matrix

> **Status: milestone gate MET.** Every durable state family, daemon input,
> policy, and recovery path the Sprint 18-20 daemon will consume has an explicit
> CLI creation or inspection path with stable JSON envelopes and a golden or
> end-to-end test in the merged tree. No daemon-consumed durable state family
> lacks a CLI path. The items listed under [Deferred work](#deferred-work) are
> daemon-*owned* automation loops and future mutation modes — not missing CLI
> surface — which is exactly the boundary the spec draws (the daemon "must not
> become the first interface for creating configuration, mutating durable
> control-plane state, approving or recovering work, or explaining why work is
> blocked", `docs/specs/voom-control-plane-design.md` §"CLI Milestone And Daemon
> Readiness Requirements").

This is the closeout the spec demands before any daemon sprint may start: an
explicit table "mapping every Sprint 18-20 daemon input, policy, action, and
recovery path to an existing CLI/API command and test"
(`docs/specs/voom-control-plane-design.md`, Sprint 17 roadmap entry, closeout
documentation bullet). It is the forcing function that proves no daemon-consumed
state family was missed by Sprint 17's tasks (#270-#288).

## How this matrix was verified

Every command cited below is a real variant of the `Command` enum in
`crates/voom-cli/src/cli.rs` (the shipped `voom` surface); every test name is a
real `#[test]`/`#[tokio::test]` function verified present in the merged tree.
Envelope tests (`*_envelope.rs`) assert the CLI JSON contract; `_flow` / `_e2e`
tests drive the multi-process operator topology; safety-gate and commit-gate
tests prove fail-closed behavior. Where a daemon behavior has no CLI enforcement
yet (a loop the daemon will own), the row cites the CLI surface the daemon reads
and the [Deferred work](#deferred-work) entry names the owning sprint.

The three matrices below correspond to the three clauses of the requirement:

1. **Durable state families** — every family listed in the Daemon MVP
   requirements the daemon "must consume" (inputs and policies).
2. **Daemon MVP behaviors** — every action the daemon MVP "must support",
   mapped to the CLI/API surface it consumes and the sprint that owns the
   automation.
3. **Pre-daemon safety baseline** — every condition the daemon must treat as
   blocked work rather than a default to work around.

## 1. Durable state families → CLI create/manage → inspect → test → recovery

Rows are the durable families the Daemon MVP requirements name as
daemon-consumed: "library roots, scan configuration, policies, input sets,
scheduling policy, safety policy, external-system mappings, node/worker grants,
manual locks, issues, artifacts, backups, reports, and recovery records."

| Durable state family | CLI create / manage | CLI inspect | Golden / e2e test (real, in tree) | Recovery / durability story |
|---|---|---|---|---|
| Library roots | `library add`, `library root add\|update\|enable\|disable\|remove` | `library root list\|show`, `library list\|show` | `crates/voom-cli/tests/library_envelope.rs`: `root_add_outputs_record`, `root_add_overlapping_is_conflict`, `scan_root_disabled_is_blocked` | Durable rows; disabling a root/library refuses scans (`BLOCKED`) instead of scanning; re-enable restores. ADR 0027. |
| Scan configuration | `library root add\|update` fields: `--scan-mode` (`watch_enabled` etc.), `--include/exclude-glob`, `--extension`, `--stability-seconds`, `--debounce-seconds`, `--symlink-policy`, `--hidden-file-policy`, `--max-depth` | `library root show` | `crates/voom-cli/tests/library_envelope.rs`: `root_add_outputs_record`, `library_list_and_update_and_disable`; `crates/voom-cli/tests/scan_envelope.rs`: `scan_directory_reports_unsupported_entries_as_skipped` | CRUD idempotent per field; the future watcher reads these rows rather than inventing config (Sprint 18). ADR 0027. |
| Policies | `policy create`, `policy version add` | `policy list`, `policy show` | `crates/voom-cli/tests/policy_envelope.rs`: `policy_create_lists_and_shows_document`, `policy_version_add_appends_new_version`, `policy_create_duplicate_slug_is_conflict` | Slug is `UNIQUE`; `policy create` is not idempotent — revise via `policy version add`. New accepted version auto-advances the current pointer. |
| Input sets | `policy input create-from-scan [--all \| --root <id> \| single-file args]` | `plan show` consumes them | `crates/voom-cli/tests/scan_envelope.rs`: `policy_input_create_from_scan_all_builds_whole_library`, `policy_input_create_from_scan_all_conflicts_with_single_file_args`, `policy_input_create_from_scan_missing_rows_is_not_found` | Derived from durable media snapshots (non-video/unprobeable files skipped with a reported count); rebuild by re-running from scan rows. |
| Scheduling policy | `scheduling-policy create\|update\|delete` | `scheduling-policy list\|show` | `crates/voom-cli/tests/scheduling_policy_envelope.rs`: `create_outputs_the_record`, `update_replaces_fields`, `delete_reports_success`, `create_rejects_bad_copy_window` | Full-replace `update`; delete by slug. Copy window validated `HH:MM-HH:MM`. ADR 0028. |
| Safety policy | `safety-policy create\|update\|delete` | `safety-policy list\|show` | `crates/voom-cli/tests/safety_policy_envelope.rs`: `create_outputs_the_record`, `update_replaces_fields`, `create_rejects_unknown_operation`; fail-closed enforcement in `crates/voom-control-plane/src/cases/policy/safety_gate_test.rs` (see [matrix 3](#3-pre-daemon-safety-baseline--cli-field--fail-closed-test)) | Full-replace `update`; enforced fail-closed at `compliance execute --safety-policy`. Store round-trips every field and fails loud on unknown operation tokens (`safety_policies_test.rs::unknown_operation_token_in_row_fails_loud_on_read`). ADR 0028. |
| External-system mappings | `external-system register`, `external-system path-mapping create\|update\|delete` | `external-system list\|show\|sync-report`, `external-system path-mapping list\|show` | `crates/voom-cli/tests/external_system_envelope.rs`: `register_outputs_record`, `path_mapping_create_and_list`, `path_mapping_delete_reports_success`, `sync_outputs_report` | Read-only sync (`sync`, `health-check`); external *writes* remain policy-gated jobs (deferred). Path mappings retire on delete. ADR 0029. |
| Node / worker grants | `node register\|heartbeat\|retire`, `worker register`, `worker run-local --kind <ffmpeg\|mkvtoolnix>` | `node list\|show`, `worker list\|show` | `crates/voom-cli/tests/node_envelope.rs`: `node_register_outputs_token_once`, `node_heartbeat_with_env_token_activates_node`, `node_retire_outputs_retired_status`; `crates/voom-cli/tests/worker_envelope.rs`: `worker_register_with_valid_node_token_outputs_node_context`; `crates/voom-control-plane/tests/local_worker_lifecycle.rs`: `start_local_worker_registers_endpoint_then_retires_on_shutdown`, `start_local_worker_self_heals_a_stale_same_name_worker` | Node tokens shown once; `run-local` records a live endpoint, self-heals a stale same-name row on restart, and retires on signal/EOF. Epoch-guarded retire. |
| Manual locks (use leases) | `lease acquire\|release\|force-release` | `lease list` | `crates/voom-cli/tests/lease_envelope.rs`: `acquire_outputs_the_lock`, `release_reports_the_terminal_lock`, `force_release_records_the_audited_override`, `list_outputs_live_locks_with_age`; commit interaction in `crates/voom-control-plane/tests/commit_use_lease_gate.rs`: `blocking_use_lease_fails_commit_before_target_is_written`, `ttl_expired_lease_does_not_block_commit` | A live blocking lock fails any overlapping commit before mutation; TTL expiry auto-clears the block; force-release records an audited override (actor + reason). |
| Issues | `issue update\|resolve\|suppress\|accept` | `issue list\|show` (filter + keyset page) | `crates/voom-cli/tests/issue_envelope.rs`: `resolve_transitions_to_resolved`, `update_overrides_priority`, `suppress_sets_horizon`, `accept_transitions_to_accepted`, `list_paginates_with_limit_and_after_id` | Terminal-failure and policy issues auto-open (ADR 0018); operator transitions are durable and audited; suppress sets a bounded horizon. ADR 0030. |
| Artifacts | `artifact stage-copy\|verify\|commit\|recover-commit` | `artifact list\|show` (filter by state) | `crates/voom-cli/tests/artifact_envelope.rs`: `artifact_full_flow_outputs_committed_envelopes`, `artifact_list_and_show_cover_all_inspection_states`, `artifact_failure_envelopes_are_actionable`; `crates/voom-control-plane/tests/staged_artifact_flow.rs`: `scan_stage_verify_commit_flow_persists_committed_artifact` | Add-only promotion (`promote_staged_add_only`) — never overwrites; a destination collision fails the run. Commit left `recovery_required` is re-drivable. |
| Backups | produced by the backup worker during `compliance execute --backup-root` (safety-gated) | `backup list\|show` (filter by status, keyset page) | `crates/voom-cli/tests/backup_envelope.rs`: `backup_list_outputs_records`, `backup_list_filters_by_status`, `backup_show_outputs_record` | Durable `pending`/`verified`/`failed` records; a latest-failed backup blocks mutation via the safety gate (matrix 3). ADR 0025. |
| Reports | `compliance report` (preview: `--policy-version-id` + `--input-set-id`; durable: `--job-id`), `compliance apply`, `compliance execute` | `compliance report --job-id` re-reads the durable per-phase chain | `crates/voom-cli/tests/compliance_envelope.rs`: `report_outputs_compliance_report_envelope`, `apply_outputs_report_and_issue_summary`, `report_unknown_job_id_uses_not_found`; `crates/voom-cli/tests/multi_phase_flow.rs`: `multi_phase_execute_then_report_by_job_id` | Preview is deterministic (goldened); durable job report re-reads the recorded phase chain after a run (or a resumed run's last recorded summary). |
| Recovery records | `artifact recover-commit`, `artifact list --state recovery_required` | `artifact show`, `compliance report --job-id` | `crates/voom-control-plane/tests/recover_commit_gate.rs`: `clean_recovery_redrive_completes_and_records_evaluated_leases`, `blocking_lease_acquired_during_recovery_blocks_redrive`; `crates/voom-control-plane/tests/staged_artifact_flow.rs`: `commit_rejections_and_recovery_visibility_are_inspectable` | Re-drive re-evaluates the commit safety gate (a blocking lease acquired during recovery blocks the re-drive; ADR 0019). Partial runs resume via the Sprint 16 per-file-phase path. |

## 2. Daemon MVP behaviors → consumed CLI/API surface → test → owning sprint

Rows are the behaviors the Daemon MVP "must support". The daemon does not exist
yet; each row proves the daemon has a durable, tested CLI/API surface to read and
names the sprint that owns the automation loop.

| Daemon MVP behavior | CLI/API surface it consumes | Proving test (real, in tree) | Automation owned by |
|---|---|---|---|
| Continuous library monitoring | Library roots with `--scan-mode watch_enabled` | `library_envelope.rs::root_add_outputs_record` | Watcher: Sprint 18 |
| File stability / debounce rules | `library root --stability-seconds / --debounce-seconds` | `library_envelope.rs::root_add_outputs_record`, `library_list_and_update_and_disable` | Enforcement: Sprint 18 |
| Scan reconciliation | `scan --root <id>` + library roots | `library_envelope.rs::scan_root_disabled_is_blocked`; `scan_envelope.rs::scan_directory_reports_unsupported_entries_as_skipped` | Reconciliation logic: Sprint 18 |
| Background scheduling | Scheduling policy + scheduler decisions | `scheduling_policy_envelope.rs::create_outputs_the_record`; `scheduler_envelope.rs::scheduler_decisions_show_outputs_full_explanation` | Scheduler loop: Sprint 19 |
| Issue lifecycle updates | `issue update\|resolve\|suppress\|accept` | `issue_envelope.rs::resolve_transitions_to_resolved`, `update_overrides_priority` | Automation: Sprints 19-20 |
| External-system health and sync jobs | `external-system health-check\|sync\|sync-report` | `external_system_envelope.rs::health_check_records_unknown_for_system_without_mappings`, `sync_outputs_report` | Scheduled sync jobs (+ external writes): deferred |
| Runtime use-lease cleanup | `lease list` (age) + TTL expiry | `lease_envelope.rs::list_outputs_live_locks_with_age`; `commit_use_lease_gate.rs::ttl_expired_lease_does_not_block_commit` | Cleanup loop: Sprints 18-20 |
| Remote worker heartbeats | `node heartbeat`, `worker register` | `node_envelope.rs::node_heartbeat_with_env_token_activates_node`, `node_heartbeat_with_bad_token_returns_conflict_envelope` | Remote nodes / node-token auth: Sprints 19-20 |
| Stale lease recovery | `scheduler leases list --state expired`, `artifact recover-commit` | `inspection_envelope.rs::scheduler_leases_list_and_show`; `recover_commit_gate.rs::clean_recovery_redrive_completes_and_records_evaluated_leases` | Recovery loop: Sprint 20 |
| Dynamic throttles | Scheduling policy `--pause-on-degraded-node`, `--large-jobs-night-only` | `scheduling_policy_envelope.rs::update_replaces_fields` | Enforcement: Sprint 19 |
| Scheduled copy windows | Scheduling policy `--copy-window HH:MM-HH:MM` | `scheduling_policy_envelope.rs::create_rejects_bad_copy_window` | Enforcement: Sprint 19 |
| Crash recovery | `artifact recover-commit`, resume via `compliance execute` | `recover_commit_gate.rs::clean_recovery_redrive_completes_and_records_evaluated_leases`; `staged_artifact_flow.rs::commit_rejections_and_recovery_visibility_are_inspectable` | Automation: Sprint 20 |
| Event streaming for UI/API clients | `event list` (kind/subject/time-window filter, keyset page), `event show` | `inspection_envelope.rs::event_list_filters_by_time_window`, `event_show_reads_one_by_id`, `event_list_rejects_bad_timestamp_as_bad_args` | Live streaming API + Web UI: deferred (Web UI MVP) |

## 3. Pre-daemon safety baseline → CLI field → fail-closed test

The Daemon MVP requirements bound auto-execution: "A daemon may only schedule
operation kinds explicitly allowed by durable safety policy, and it must treat
destructive replace/delete/archive, missing backup requirements, unmet approval
requirements, unresolved recovery-required commits, and stale policy versions as
blocked work rather than as defaults to work around." Every clause has a durable
safety-policy field and a fail-closed test in
`crates/voom-control-plane/src/cases/policy/safety_gate_test.rs`.

| Safety-baseline clause | Safety-policy field (`voom safety-policy …`) | Fail-closed test (`safety_gate_test.rs`) |
|---|---|---|
| Only allowed operation kinds auto-execute | `--auto-execute-operation <token>` (repeatable) | `operation_not_in_allowlist_blocks`, `permissive_policy_produces_no_blocks` |
| Destructive replace/delete/archive blocked | `--allowed-commit-mode <mode>` | `add_only_not_allowed_blocks` (see the [deferred](#deferred-work) note: commit path is add-only only) |
| Missing backup requirements blocked | `--backup-required` | `backup_required_blocks_only_without_a_root`, `latest_failed_backup_blocks_and_a_later_verified_clears_it` |
| Unmet approval requirements blocked | `--approval-required` | `approval_required_blocks` |
| Unresolved recovery-required commits blocked | `--block-on-recovery-required-records` | `recovery_required_record_blocks_when_policy_sets_flag` |
| Stale policy versions blocked | (schema-version currency check) | `stale_schema_version_is_the_sole_block` |
| Verification level enforced | `--verification-level <none\|quick_decode\|full>` | `verification_required_without_verify_node_blocks` |
| Gate opens/clears a durable blocked issue | `compliance execute --safety-policy <slug>` | `enforce_opens_a_blocked_issue_and_errors`, `enforce_resolves_the_blocked_issue_once_the_policy_permits`, `missing_policy_blocks`, `latest_failed_backup_blocks_and_a_later_verified_clears_it` |

## Concurrency posture

The daemon adds continuous multi-process access. The store now opens on-disk
pools in **WAL mode** with a 30s `busy_timeout`
(`crates/voom-store/src/pool.rs`), so an operator (or the future daemon) can read
committed snapshots while another process writes — reads never block the running
writer. The operator execution e2e test exercises this: it issues a concurrent
`voom worker list` against the live database while `compliance execute` runs
(`crates/voom-cli/tests/operator_execution_e2e.rs::operator_runs_real_media_pipeline_through_cli`).
The store-wide WAL switch that the Sprint 17 slice design deferred has therefore
landed, and is no longer owed.

## Deferred work

Everything below is out of scope for the Real Media CLI milestone and is a
daemon-*owned* behavior or a future mutation mode — not a missing CLI creation
path. Each is recorded with its owning future sprint so the milestone gate stays
honest.

**Sprint 18 (Watcher, stability rules, scan sessions, reconciliation):**
- The filesystem watcher itself; scan sessions; stability/debounce *enforcement*;
  reconciliation logic for adds/modifications/removals/renames. The CLI ships the
  durable config (library roots + scan configuration) the watcher will read.

**Sprint 19 (Background scheduling):**
- The background scheduler loop; dynamic-throttle *enforcement*
  (`pause_on_degraded_node`, `large_jobs_night_only`); scheduled copy-window
  *enforcement*. The CLI ships scheduling-policy CRUD and scheduler-decision
  inspection.

**Sprint 20 (Recovery and lease automation):**
- Stale-lease recovery loop; runtime use-lease cleanup loop; crash-recovery
  automation; remote-worker heartbeat monitoring; remote nodes and node-token
  auth for local workers. The CLI ships `scheduler leases`, `lease`,
  `artifact recover-commit`, and `node heartbeat`.

**Beyond the daemon MVP / not yet owned:**
- **External-system writes.** `external-system sync` is read-only; external
  writes remain policy-gated jobs, not a direct CLI write path
  (`docs/specs/voom-control-plane-design.md`, Real Media CLI milestone).
- **Replace / delete / archive commit modes.** The safety policy vocabulary can
  *name* these commit modes (`--allowed-commit-mode replace|delete|archive`) and
  the safety gate blocks work whose mode is not allowed, but the mutation/commit
  path is **add-only only** (`promote_staged_add_only`; no `CommitMode::Replace/
  Delete/Archive` execution path exists outside `crates/voom-cli/src/cli.rs`).
  This is consistent with the daemon safety baseline, which treats destructive
  modes as blocked work — but the *execution* of those modes is a future mutation
  sprint, not shipped here.
- **Retention / quality-score-driven retention actions.** Named in the Web UI MVP
  (retention views) and future roadmap; no execution path yet.
- **Live event-streaming API and the Web UI console.** `event list/show` provide
  the durable, keyset-paginated read surface; a push/streaming API and the UI are
  the Web UI MVP.

## Runbook index

The operator-facing procedures that exercise this surface are indexed in
`docs/runbooks/README.md`, which ties each runbook to the matrix above:

- `docs/runbooks/operator-real-media-execution.md` — the end-to-end operator
  procedure (init → run-local workers → scan → policy author → whole-scan input →
  execute → inspect/recover), covering the node/worker-grant, policy, input-set,
  artifact, report, and recovery rows of matrix 1 and the `run-local` stdout
  contract.
- `docs/runbooks/migration-rollback.md` — the schema-rollback and dirty-migration
  recovery procedure (up-only migrations, binary-before-DB ordering, WAL-aware
  snapshot restore), covering the durability/recovery story the daemon relies on
  before it automates any mutation.

## Milestone gate assertion

The Real Media CLI milestone is **closed**. The evidence:

1. Every durable state family the daemon will consume (matrix 1) has a CLI
   create/manage path, a CLI inspect path, a golden or e2e test, and a stated
   recovery/durability story.
2. Every daemon MVP behavior (matrix 2) reads a durable, tested CLI/API surface;
   the only thing missing is the automation loop, and each loop's owning sprint
   is named.
3. Every clause of the pre-daemon safety baseline (matrix 3) has a durable
   safety-policy field and a fail-closed test.
4. No daemon-consumed durable state family was found to lack a CLI path. The
   [deferred](#deferred-work) items are daemon-owned automation and future
   mutation modes, consistent with the spec's rule that the daemon must not be
   the first interface for creating or repairing durable state.

Sprint 18 may begin.
