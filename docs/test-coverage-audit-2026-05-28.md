# VOOM Workspace Test-Coverage Audit

## 1. Executive Summary

This audit confirmed **177 findings** across the VOOM Rust workspace after adversarial verification (each finding was independently re-checked against the source and the full workspace test suite; 28 candidates were refuted as already-covered, misread, or speculative).

### Counts by kind

| Kind | Count |
|------|-------|
| coverage_gap | 123 |
| missing_sequence | 36 |
| bug | 18 |
| **Total** | **177** |

### Counts by severity

| Severity | Count |
|----------|-------|
| high | 65 |
| medium | 91 |
| low | 21 |
| **Total** | **177** |

### Where coverage is weakest

The weakest coverage clusters in three areas. First, **worker-honesty and safety guards** in the control-plane dispatch validators (transcode, remux, audio, scan) — the checksum-mismatch, source-changed-during-execution, unsupported-container, and verification-failure branches are systematically untested across every media operation, even though they are the last in-process defense before an artifact is committed. Second, **multi-step lifecycle sequences** — lease expiry/requeue/re-acquire, force-release recovery, diamond-dependency promotion, terminal-failure stranding, and the full remote-execution idempotency-replay loop are tested only as isolated steps, never as the end-to-end sequences that production hits on every worker restart. Third, **error-code contract paths** — `NotFound` vs `Conflict` vs `Internal` distinctions on missing/retired/terminal rows are largely unexercised at both the repo and CLI/API envelope layers, and several of these (`expire_due` not resetting `next_eligible_at`, `workers::retire_in_tx` returning `Conflict` instead of `NotFound`, `ControlPlane::open` collapsing `TooNew`/`Dirty` into `DbPartialSchema`, `IssueSeverity::parse` emitting `DbUnreachable`) are confirmed bugs, not just gaps.

The 18 confirmed bugs are concentrated in the control-plane workflow executor (policy-node ID expansion deadlock), the audio extract post-commit recovery asymmetry, the worker-protocol idempotency cache (second-terminal corruption, unbounded growth), schema-state error mapping, and the `DuplicateIdempotencyKey` misclassification in worker dispatch.

## 2. Top 10 Highest-Leverage Tests to Add

1. **`expand_successful_ticket_handles_policy_node_ids`** — `voom-control-plane` — Build a two-node policy workflow where node B depends on node A via `apply_plan_dependencies`; run `submit_and_run` and assert B becomes ready after A succeeds. Catches the confirmed wildcard-arm deadlock bug in `executor.rs:730` where policy-node IDs are silently dropped and the job live-locks.

2. **`expire_requeue_then_complete_succeeds_and_emits_full_event_sequence`** — `voom-store` — Acquire, expire via `expire_due`, re-acquire, release; assert ticket succeeds with correct attempt count and the full ordered event log. Exercises the confirmed `next_eligible_at`-not-reset bug (`leases.rs:978`) plus attempt-counter accounting across requeue.

3. **`diamond_dependency_child_promoted_exactly_once_after_both_parents_succeed`** — `voom-control-plane` — C depends on A and B; release A (C stays pending), release B (C promoted once). Validates the `mark_ready_if_unblocked_in_tx` unsucceeded-count predicate at the ControlPlane layer where events are emitted, and the paired terminal-failure stranding case.

4. **`test_extract_post_commit_succeeded_event_failure_returns_ok_with_context`** — `voom-control-plane` — Inject a failing `record_extract_succeeded` after a successful sidecar commit; assert `Ok` with `commit_recovery_required` and zero false `audio_extract_failed` events. Catches the confirmed asymmetry bug (`audio/mod.rs:629`) where a committed extraction is reported as a failure.

5. **`remote_recover_then_reacquire_picks_up_requeued_ticket`** — `voom-control-plane` — Full remote loop: `remote_acquire` → `remote_recover` (expire) → `remote_acquire` with new key → `remote_complete`; assert attempt=2 and exactly one new lease. Covers the production-critical worker-restart recovery sequence the idempotency layer must not replay incorrectly.

6. **`test_control_plane_open_rejects_too_new_schema`** (and `_dirty`) — `voom-control-plane` — Inject a synthetic future migration row, open `ControlPlane`, assert `DbSchemaTooNew` (and `DbDirtyMigration` for `success=0`). Catches the confirmed bug at `lib.rs:165` that misroutes operators to `voom init` instead of a binary upgrade.

7. **`test_streaming_writer_rejects_second_terminal_frame`** — `voom-worker-protocol` — Write two terminal frames to `StreamingFrameWriter`; assert the second returns `MalformedFrame` and `cached_body` does not grow. Catches the confirmed idempotency-cache corruption bug at `http.rs:124`.

8. **`validate_result_rejects_source_changed_during_execution`** (transcode + remux) — `voom-control-plane` — Construct a result where `input_pre != input_post`; assert `ArtifactChecksumMismatch`. Closes the systematically-untested TOCTOU worker-honesty guard shared across both media-validation paths.

9. **`remote_acquire_second_worker_gets_no_candidate_when_first_wins_ticket`** — `voom-control-plane` — Two workers race for one ready ticket; assert only one lease is created and the loser gets `Idle`/`NoCandidate`. Verifies the SQLite `WHERE state='ready'` UPDATE gate that is the sole concurrency barrier.

10. **`probe_file_malformed_payload_returns_domain_error_not_protocol_error`** — `voom-ffprobe-worker` — Send a malformed `ProbeFile` payload; assert `Ok` with a terminal `Error` frame, not `Err(ProtocolError::InvalidPayload)`. Catches the confirmed contract divergence (`ffprobe.rs:192`) where ffprobe returns HTTP 400 and bypasses idempotency caching, unlike the other three workers.

## 3. Detailed Findings

---

## HIGH SEVERITY

### cp-cases (voom-control-plane)

**`crates/voom-control-plane/src/cases/remote_execution.rs:529` — coverage_gap**
The `worker_active >= worker_limit` capacity guard (TOCTOU safety valve between the scorer's advisory read and the lease write) is never exercised; every test uses `limit: 1` with a single lease. The analogous `NodeCapacityFull` branch is tested. An untested re-read could allow double-dispatch past `max_parallel` if regressed.
*Test:* `remote_acquire_blocks_second_lease_when_worker_capacity_exhausted` — acquire one lease, issue a second `remote_acquire`, assert `NoCandidate` with `reason_code == "worker_capacity_full"`.

**`crates/voom-control-plane/src/cases/tickets.rs:104` — coverage_gap**
`record_pre_lease_ticket_failure` `NotFound` path is never triggered; all tests use valid IDs. `NOT_FOUND` is a public error-code contract; a deleted/nonexistent ticket should get a clear response, not a panic.
*Test:* `pre_lease_failure_rejects_nonexistent_ticket` — call with an uninserted `TicketId`, assert `VoomError::NotFound`.

**`crates/voom-control-plane/src/cases/leases.rs:295` — missing_sequence**
The `remote_fail` terminal path (max_attempts exhausted → `TicketState::Failed`) is never tested end-to-end; all fixtures use `max_attempts: 2`, so the first fail always lands in the retriable arm. The `TicketFailedTerminal` event path through the remote protocol is uncovered.
*Test:* `remote_fail_emits_ticket_failed_terminal_when_max_attempts_exhausted` — `max_attempts: 1`, `remote_acquire`, `remote_fail` with `WorkerTimeout`; assert `TicketFailedTerminal` emitted and `TicketFailedRetriable` absent.

### cp-workflow (voom-control-plane)

**`crates/voom-control-plane/src/workflow/executor.rs:730` — bug**
`expand_successful_ticket` matches only hard-coded default-CI node IDs; policy-bridge nodes (`policy-node_*` prefix) hit the `_ => {}` wildcard, so no downstream expansion runs. Dependent tickets stay permanently `pending`, live-locking the job (`workflow_finished` false but no ready tickets), and the executor fails it with `Internal` rather than surfacing the bug.
*Test:* `expand_successful_ticket_handles_policy_node_ids` — two-node policy workflow with B depending on A via `apply_plan_dependencies`; assert B becomes ready after A succeeds.

**`crates/voom-control-plane/src/workflow/executor.rs:1709` — coverage_gap**
`source_bundle_id_for_file_version` returns `Config("...not a bundle member")` when the JOIN finds no row, but the only extract dispatch test always creates a bundle first. A real file without bundle membership fails at dispatch time (consuming a retry) instead of at ticket creation.
*Test:* `extract_audio_fails_when_source_has_no_bundle_membership` — policy ExtractAudio against a file_version with no `asset_bundle_members` row; assert `ConfigInvalid` and the lease is failed.

### cp-artifact (voom-control-plane)

**`crates/voom-control-plane/src/artifact/commit.rs:364` — coverage_gap**
`prepare_commit_in_tx` checks `source.retired_at.is_some()` and returns a PreMutation `Config` error, but no test retires the source FileVersion before commit. A version can legitimately be retired between staging and commit.
*Test:* `test_commit_rejects_retired_source_file_version` — stage+verify, `retire_file_version_in_tx`, then commit; assert `ConfigInvalid` with a pre-mutation report and no commit record.

**`crates/voom-control-plane/src/artifact/commit.rs:555` — coverage_gap**
`live_staging_location_in_tx` returns `Config` for zero staging rows, but no test retires the only staging location without replacement. Concurrent cleanup can produce this state.
*Test:* `test_commit_rejects_zero_staging_locations` — stage+verify, retire the staging location, commit; assert `ConfigInvalid` with `pre_mutation_report.is_some()` and no commit rows.

### cp-audio (voom-control-plane)

**`crates/voom-control-plane/src/audio/mod.rs:629` — bug**
`commit_verified_extract_audio` propagates `record_extract_succeeded` errors with `?` after the sidecar commit has already succeeded. The error bubbles to `execute_extract_audio_with_dispatchers`, which records a false `audio_extract_failed` event. This is asymmetric with the transcode path (`mod.rs:341`), which has an `if let Err ... return Ok(recovery)` guard. Retries re-commit to an existing target and produce more false failures.
*Test:* `test_extract_post_commit_succeeded_event_failure_returns_ok_with_context` — make `record_extract_succeeded` fail after a successful commit; assert `Ok` with `commit_recovery_required` and zero `audio_extract_failed` events.

**`crates/voom-control-plane/src/audio/source.rs` — coverage_gap**
`audio/source.rs` has no `#[cfg(test)]` block and no `source_test.rs`. Seven error branches in `select_source`/`read_media_snapshot` are uncovered: retired file_version, location belonging to a different file_version, retired explicit location, non-local explicit location, zero/multiple live local locations, and snapshot for a different file_version_id. The analogous remux and transcode `source_test.rs` files exist and cover these.
*Test:* `test_audio_select_source_rejects_retired_version`, `test_audio_select_source_ambiguous_locations`, `test_audio_read_media_snapshot_wrong_file_version` — mirror `transcode/source_test.rs`.

**`crates/voom-control-plane/src/audio/mod.rs:552` — coverage_gap**
The extract verification-failure path (`status != Succeeded` → `VerificationFailure`) has no test with a real staging/worker dispatcher; the only extract test fails before verification. The transcode counterpart (`late_transcode_failure_event_keeps_attempt_context...`) proved this non-trivial.
*Test:* `test_extract_late_failure_keeps_attempt_context` — `WritingExtractDispatcher` + `MismatchedVerifyDispatcher`; assert `VerificationFailure` and that the failed event carries `artifact_handle_id`, `artifact_location_id`, `staging_path`, `selected_stream`, `provider`.

### cp-scan (voom-control-plane)

**`crates/voom-control-plane/src/scan/persist.rs:149` — bug**
`verify_probe_facts` AND's four conditions, but the only failure test (`FakePlan::DriftOnPath`) sets only `post_probe.content_hash`. The pre_probe size/hash worker-honesty guards and the post_probe size guard are untested. A loosening of the AND to a subset check would pass all existing tests.
*Test:* `test_verify_probe_facts_pre_probe_hash_mismatch` (and size variants) — supply mismatched pre_probe facts with matching post_probe; assert `content_drift()`.

**`crates/voom-control-plane/src/scan/persist_test.rs:26` — coverage_gap**
Every `persist_scanned_media_snapshot` unit test passes `&[]` sidecars. `ensure_primary_bundle` (`persist.rs:318`) and `persist_sidecar` (`persist.rs:430`) are untested at the unit level, including the `Conflict` role-mismatch (`persist.rs:330`) and sidecar-in-different-bundle (`persist.rs:459`) guards.
*Test:* `test_persist_scan_rejects_conflicting_primary_bundle_role` and `test_persist_scan_conflict_sidecar_in_different_bundle` — expect `Conflict`.

**`crates/voom-control-plane/src/scan/persist.rs:265` — coverage_gap**
`snapshot_with_stream_ids` error branches (non-object stream entry at line 275, stream missing id-and-index at line 284) are uncovered; only the happy path is tested. Called from scan, audio/commit, remux/commit; malformed worker snapshots surface as `INTERNAL`.
*Test:* `test_snapshot_with_stream_ids_non_object_stream` (`{streams: ['x']}`) and `test_snapshot_with_stream_ids_missing_index` (`{streams: [{}]}`) — expect `Config`.

### cp-transcode-core (voom-control-plane)

**`crates/voom-control-plane/src/lib.rs:165` — bug**
`open_with_pool_and_rng` returns `VoomError::Migration` (→ `DbPartialSchema`) for all non-Current `SchemaState` variants, including `TooNew` (should be `DbSchemaTooNew`) and `Dirty` (should be `DbDirtyMigration`). Operators against a too-new DB are told to run `voom init` instead of upgrading the binary.
*Test:* `test_control_plane_open_rejects_too_new_schema` — inject a future migration row, assert `DbSchemaTooNew`; repeat with `success=0` for `DbDirtyMigration`.

**`crates/voom-control-plane/src/transcode/dispatch.rs:84` — coverage_gap**
`validate_result` `input_pre != input_post` (source-changed-during-execution) branch is never reached; `WrongInputFactsTranscodeDispatcher` sets both equal, hitting the line-89 check instead. A comparison flipped to `==` would silently accept tampered sources.
*Test:* `test_validate_result_rejects_source_changed_during_execution` — `input_post.size_bytes` differs from `input_pre`; assert `ArtifactChecksumMismatch` with "changed during worker execution".

**`crates/voom-control-plane/src/transcode/mod.rs:120` — coverage_gap**
The transcode verification non-`Succeeded` path (`VerificationFailure`) is untested; `FakeVerifyDispatcher` always returns `Verified`. Audio and remux both have this test; transcode is the outlier. An inverted conditional would commit corrupted artifacts.
*Test:* `test_execute_transcode_video_rejects_failed_verification` — `FailedVerifyDispatcher`; assert `VerificationFailure` and no output file.

### store-routing (voom-store)

**`crates/voom-store/src/repo/leases.rs:978` — bug**
`process_expired_lease` requeues to `ready` without resetting `next_eligible_at`, unlike `force_release` (line 743) and `fail_retriable` (line 554). A previously failed-retriable ticket whose next lease expires keeps its stale future `next_eligible_at`; the acquire predicate (`next_eligible_at <= now`) strands it in `ready`-but-unacquirable.
*Test:* `test_expire_due_requeue_resets_next_eligible_at` — `fail_retriable` (next_eligible_at = T0+300), let the second lease expire, assert the requeued ticket is immediately acquirable.

**`crates/voom-store/src/repo/tickets.rs:432` — coverage_gap**
`TicketRepo::list_by_state` has zero coverage anywhere in the workspace. Its `LIMIT` and `ORDER BY` are unverified.
*Test:* `test_list_by_state_returns_correct_tickets_ordered` — tickets in multiple states; assert correct set in priority-desc / next_eligible_at-asc / id-asc order, and a limit of 1 returns one row.

### store-events (voom-store)

**`crates/voom-store/src/repo/events.rs:138-143` — coverage_gap**
Cursor-based pagination for both `list` and `tail` is dead from a test perspective — every call-site sets `cursor: None`. The `event_id > ?` / `event_id < ?` branches and cursor binding are untested, including the descending tail cursor.
*Test:* `test_list_paginates_correctly` — insert 5 events, walk pages of 2 via `next_cursor`, assert ASC IDs with no duplicates; mirror for `tail` DESC.

**`crates/voom-store/src/repo/events_test.rs:102` — coverage_gap**
`append_then_get_round_trips_every_m1_kind` covers 24 M1 variants; the `Event` enum has ~40+ more (Node*, Artifact*, Transcode*, Remux*, Audio*, Issue*, AssetBundle*, FileAsset/Version/Location*, IdentityEvidenceRecorded). A serde rename in any would produce unreadable rows.
*Test:* Extend (or parallel) to cover every `Event` variant with an append+get round-trip.

### store-jobs-nodes (voom-store)

**`crates/voom-store/src/repo/workers.rs:395-399` — bug**
`workers::retire_in_tx` returns `Conflict` ("row missing, wrong epoch, or already retired") for a missing worker id, whereas `nodes::retire_in_tx` calls `get_in_tx` first and returns `NotFound`. This breaks public `NOT_FOUND` vs `CONFLICT` contract symmetry consumed by the CLI envelope.
*Test:* `retire_missing_worker_returns_conflict_or_not_found` — `retire(WorkerId(99_999), 0, T0)`, assert the variant and document current behavior.

**`crates/voom-store/src/repo/jobs.rs:267-271` — coverage_gap**
`transition_open_to` returns `Conflict` ("row missing or non-open state") for a nonexistent job id, but the only test transitions an already-terminal job. Callers cannot distinguish "never existed" from "wrong state".
*Test:* `succeed_nonexistent_job_returns_conflict` — `succeed(JobId(99_999), T0)`; add parallel `fail`/`cancel` tests.

### store-artifacts (voom-store)

**`crates/voom-store/src/repo/artifacts.rs:461` — coverage_gap**
`retire_location_in_tx` already-retired `Conflict` ("not live") branch has no test; only the success path is covered. Artifact commit relies on this `Conflict` to detect races.
*Test:* `test_retire_location_already_retired` — retire twice; assert the second is `Conflict` containing "not live".

**`crates/voom-store/src/repo/artifact_access_plans.rs:273` — coverage_gap**
`mark_status_in_tx` `NotFound` branch (nonexistent plan id) is untested; only the already-terminal `Conflict` path is covered. The remote-lease completion path must return `NotFound`, not `Conflict`, for a wrong wire id.
*Test:* `test_mark_status_not_found_for_unknown_plan_id` — `mark_status(9999)`; assert `NotFound`.

### store-policy (voom-store)

**`crates/voom-store/src/repo/policies.rs:167` — coverage_gap**
`add_version` `NotFound` branch (nonexistent document_id) is untested anywhere. Callers depend on `NOT_FOUND` to distinguish a wrong document id.
*Test:* `add_version_returns_not_found_for_nonexistent_document` — `add_version(PolicyDocumentId(9999), ...)`; assert `NOT_FOUND`.

**`crates/voom-store/src/repo/issues.rs:152` — coverage_gap**
`PolicyIssueMutationKind::Unchanged` (all fields match → no DB write) is never exercised; no test calls `upsert_policy_noncompliant_in_tx` twice with identical data. The compliance handler dispatches on all four variants, affecting `skipped_count` and event emission.
*Test:* `upsert_policy_issue_returns_unchanged_when_all_fields_match` — upsert twice identical; assert `Unchanged` and unchanged epoch.

**`crates/voom-store/src/repo/issues.rs:144` — missing_sequence**
Re-opening a resolved issue via `upsert` (create → resolve → upsert-again) is untested. This is the only path that re-opens an issue (status mismatch → UPDATE sets status=open, resolved_at=NULL). A broken re-open would silently leave a regression untracked.
*Test:* `upsert_policy_issue_reopens_a_previously_resolved_issue` — assert result `Updated`, status `Open`, epoch advanced by 1.

### store-safety (voom-store)

**`crates/voom-store/src/repo/commit_safety_gate.rs:1714` — coverage_gap**
Phase A gate against an already-retired target location (`phase == Prepare && location.retired_at.is_some()` → `ClosureIncomplete`) has no test; the "missing target" test uses a nonexistent id, a different path. Removing the `retired_at` check would let Phase A walk a dead row and land a pending intent.
*Test:* `prepare_phase_a_blocked_by_already_retired_target_location` — retire the location, then `prepare_destructive_commit`; assert `BlockedByClosureIncomplete` and durable `abort_reason = 'closure_incomplete'`.

**`crates/voom-store/src/repo/commit_safety_gate.rs:3416` — coverage_gap**
`list_pending_commit_intents` `older_than` strict-`<` boundary is untested; the existing test never seeds a row exactly at the cutoff. A silent change to `<=` would not be caught.
*Test:* `list_pending_commit_intents_older_than_cutoff_is_strict_lt` — seed at exactly the cutoff (assert excluded) and one ns before (assert included).

### store-core (voom-store)

**`crates/voom-store/src/schema.rs:187` — coverage_gap**
The `probe_schema` ordered-prefix invariant (`applied_versions != expected_prefix` → `TooNew`) is never triggered; the only gap test uses unknown version numbers, hitting the earlier `unknown_version_present` branch. A DB with only version 2 applied would be misclassified as `Partial`, and `voom init` would insert version 1 after version 2 — a migration-order violation.
*Test:* `probe_returns_too_new_on_gap_in_applied_versions` — seed `_sqlx_migrations` with only version 2; assert `TooNew`.

**`crates/voom-store/src/init.rs:216` — coverage_gap**
`probe_after_failure` retry-budget-exhaustion path (returns last non-terminal state; caller emits `DbPartialSchema` via the wildcard arm) is untested, as is the entire retry loop (lines 204-214) — the core concurrency-safety mechanism.
*Test:* `probe_after_failure_returns_partial_when_state_stays_partial` — controlled pool returning `Partial`; assert the return state and resulting `DB_PARTIAL_SCHEMA`.

**`crates/voom-store/src/schema.rs:195` — coverage_gap**
`probe_schema` returning `SchemaState::Partial` is never directly asserted; the partial-state test only verifies `init_on` succeeds. `HealthPlane` maps `Partial` to a "safe to rerun init" signal; a misclassification gives wrong remediation.
*Test:* `probe_returns_partial_on_subset_of_migrations_applied` — seed half the MIGRATOR entries; assert `Partial { applied: N/2, expected }`.

### plan (voom-plan)

**`crates/voom-plan/src/audio.rs:44` — coverage_gap**
`AudioOperationPayload::try_from_execution_value` has zero tests, though it parses worker payloads. All error branches (missing/unsupported `type`, missing `target_codec`/`container`, missing/zero `source_media_snapshot_id`, invalid `filter`) are uncovered. `RemuxOperationPayload` has a full rejection-path test. A `source_media_snapshot_id` of 0 will panic the control-plane deserialization path.
*Test:* `audio_payload_rejects_invalid_contract_fields` — parameterized over each malformed payload; assert the correct `AudioPayloadError`.

**`crates/voom-plan/src/audio.rs:399` — coverage_gap**
The `TranscodeAudio`/`ExtractAudio` `NoVideo` block (`video_stream_count == 0` → `UnsupportedMediaShape`) is never tested at the planner integration level; the only zero-video test uses remux. A refactor of `selected_audio_streams` could silently break the diagnostic code.
*Test:* `transcode_audio_blocks_when_snapshot_has_no_video_streams` — assert `Blocked` and `diagnostics[0].code == UnsupportedMediaShape` with the expected message.

### policy (voom-policy)

**`crates/voom-policy/src/validate.rs:193` — coverage_gap**
`self_dependency` (`SelfDependency`) and `dependency_cycle` (`DependencyCycle` via `has_cycle`) validation paths are completely untested — the only structural correctness checks preventing circular/self-looping phases from compiling. A `has_cycle` off-by-one would silently produce bad compiled policies.
*Test:* `test_rejects_self_dependency` and `test_rejects_dependency_cycle` — assert the respective diagnostic codes.

**`crates/voom-policy/src/validate.rs:224` — coverage_gap**
`validate_run_if` (`InvalidRunIfTrigger`) has zero coverage — no test for missing/wrong trigger, and no positive test for a valid `run_if modified phase_name`. The `tokens.get(1).or_else(|| tokens.get(2))` expression (line 232) is also suspect.
*Test:* `test_rejects_run_if_without_trigger` (assert `invalid_run_if_trigger`) and `test_accepts_run_if_modified` (assert no diagnostics).

**`crates/voom-policy/src/compiled.rs:261-262` — coverage_gap**
Phase-level `skip_if` and `run_if` compilation are never verified; every fixture has both null. `phase_run_if` wraps raw text as a bare `Predicate` while `phase_skip_if` parses the condition — a deliberate asymmetry with no coverage. `CompiledPhase::run_if` is read by the planner at runtime.
*Test:* `test_compile_policy_sets_skip_if_from_skip_when` and `test_compile_policy_sets_run_if_from_run_if` — assert the compiled enum shapes.

### worker-protocol (voom-worker-protocol)

**`crates/voom-worker-protocol/src/http.rs:124` — bug**
`StreamingFrameWriter::write_frame` has no `terminal_sent` guard. A second terminal frame appends to `cached_body` and calls `complete()` again, overwriting the `Completed` cache entry with a body containing two concatenated terminal frames; all subsequent replays serve malformed NDJSON. `NdjsonWriter::emit` has this guard; `StreamingFrameWriter` does not.
*Test:* `test_streaming_writer_rejects_second_terminal_frame` — second terminal call returns `MalformedFrame` and `cached_body` does not grow.

**`crates/voom-worker-protocol/src/http.rs:808` — bug**
`IdempotencyCache::make_room` pops the oldest key; if it is `Active`, it pushes it back and breaks without evicting. Both `begin()` and `complete()` then insert unconditionally, so at capacity with all-Active entries the cache grows without bound, defeating `IDEMPOTENCY_CACHE_CAPACITY`. The server can exhaust memory silently.
*Test:* `test_idempotency_cache_does_not_exceed_capacity_under_all_active` — fill capacity-2 with two Active entries, `begin()` a third; assert `entries.len() <= 2` or document intended behavior.

### events (voom-events)

**`crates/voom-events/src/payload_test.rs:867` — bug**
`event_kind_matches_serde_tag` claims compiler-enforced exhaustiveness, but the `vec![]` literal has none — adding a variant does not break this test. It covers ~41 of 90 `Event` variants; all Transcode/Remux/Audio/M2-identity/most-M3 variants are absent. The serde-tag-drift invariant is verified for only ~45% of the vocabulary.
*Test:* `event_kind_matches_serde_tag_exhaustive` — cover all 90 variants (or a compile-time macro iterating all variants).

**`crates/voom-events/src/payload.rs:348` — coverage_gap**
`ArtifactTranscodeProgress/Succeeded/Failed` payloads have zero serde round-trip tests (only `Started` has one). `Succeeded` has required `output_container`/`output_video_codec`; `Failed` has a serde-tagged `FailureClass`. A rename or structural change surfaces only on a live DB cycle.
*Test:* `artifact_transcode_progress_succeeded_failed_payload_round_trip` — construct all three, round-trip, assert wire kind and (for Failed) the snake_case `failure_class` tag.

### cli (voom-cli)

**`crates/voom-cli/src/commands/health.rs:52` — coverage_gap**
`voom_error_hint` (maps every `ErrorCode` to an `Option<String>` hint, operator-facing contract) has no direct unit test; only one hint is observed indirectly via regex. The `DbUnreachable` hint and ~20 None-returning branches are untested.
*Test:* `test_voom_error_hint_db_unreachable_suggests_init_or_permissions` — assert expected keywords for `DbUnreachable`/`DbPartialSchema` and `None` for None-returning codes.

**`crates/voom-cli/src/commands/compliance.rs:21` — coverage_gap**
`report`/`apply`/`execute` call `ControlPlane::open` inline (bypassing `open_control_plane`) and emit an error envelope on failure, but no test runs `voom compliance report` against an uninitialized/missing DB. A wrong error code or command label in this duplicated path would not be caught.
*Test:* `compliance_report_against_uninitialized_db_emits_db_error_envelope` — empty SQLite file; assert exit 2, `command: compliance`, `error.code` in {DB_PARTIAL_SCHEMA, DB_UNREACHABLE}.

### api (voom-api)

**`crates/voom-api/src/lib.rs:224` — coverage_gap**
`voom_route_error_response` maps `NotFound` to HTTP 404, reachable from all five execution.rs call sites (e.g. complete/fail on a nonexistent lease), but the only NOT_FOUND test comes from `not_configured_response`, a separate path. The line-224 branch is never exercised through execution routes.
*Test:* `test_complete_unknown_lease_returns_404` — POST complete on lease 99999 with valid creds; assert 404, `error.code == NOT_FOUND`, `command == execution.complete`.

**`crates/voom-api/src/lib.rs:138` — coverage_gap**
The `/health` route never tests `DbUnreachable` → 503 through HTTP; existing tests cover other DB states but never delete the DB file after `HealthPlane::open`. `DB_UNREACHABLE` is the most likely production failure; a regression would yield 500 instead of 503.
*Test:* `test_health_with_deleted_db_returns_503_db_unreachable` — init, open, delete the file, GET /health; assert 503 and `DB_UNREACHABLE`.

### core (voom-core)

**`crates/voom-core/src/issue.rs:44` and `:92` — bug**
`IssueSeverity::parse` and `IssuePriority::parse` return `VoomError::Database` (→ `DbUnreachable`) on an unrecognized TEXT value. A corrupt column is data corruption, not connectivity; operators get a misleading `DB_UNREACHABLE`. `issue_test.rs` asserts only `is_err()`, never `code()`.
*Test:* `test_issue_severity_parse_unknown_emits_internal` — after changing the variant to one mapping to `Internal`, assert `error_code() == Internal`; same for `IssuePriority`.

**`crates/voom-core/src/clock.rs:8` — coverage_gap**
`format_iso8601` (shared by CLI init, health, and API for `schema_init_at`) has no tests — neither the ISO-8601 path nor the `unwrap_or_else` unix-timestamp fallback. A format-string regression would silently change wire output.
*Test:* `test_format_iso8601_normal` (UNIX_EPOCH → `1970-01-01T00:00:00Z`) and `test_format_iso8601_fallback_for_extreme_year` (numeric fallback).

### workers (voom-ffmpeg-worker / voom-ffprobe-worker)

**`crates/voom-ffprobe-worker/src/ffprobe.rs:192-195` — bug**
`handle_operation_with_config` uses `?` on the payload deserialization, propagating `ProtocolError::InvalidPayload` as an `Err` → HTTP 400, which also clears the idempotency cache. All three other workers wrap decode errors into a domain error frame at HTTP 200 (cached). This breaks the protocol contract (HTTP 200 + domain frame for soft failures) and prevents deterministic retry.
*Test:* `probe_file_malformed_payload_returns_domain_error_not_protocol_error` — payload `{"path": 12}`; assert `Ok` with a terminal `Error` frame (`MalformedWorkerResult`), not `Err(InvalidPayload)`.

**`crates/voom-ffmpeg-worker/src/handler.rs:353` — coverage_gap**
`validate_transcode_audio_contract` rejection branches (non-mkv container, codec not in {aac,opus}, empty selected_streams) are all untested — the primary guard preventing invalid work from reaching ffmpeg.
*Test:* `transcode_audio_rejects_non_mkv_container`, `transcode_audio_rejects_unsupported_codec`, `transcode_audio_rejects_empty_selected_streams` — assert `ConfigInvalid`.

**`crates/voom-ffmpeg-worker/src/handler.rs:377` — coverage_gap**
`validate_extract_audio_contract` rejection branches (non-ogg container, non-opus codec) are untested.
*Test:* `extract_audio_rejects_non_ogg_container` and `extract_audio_rejects_non_opus_codec` — assert `ConfigInvalid`.

### seq-work-lifecycle (voom-store / voom-control-plane)

**`crates/voom-store/src/repo/leases.rs:979-980` — missing_sequence**
`expire_due` (requeue) → re-acquire → `release_lease` (succeed) is never tested end-to-end. The attempt counter is incremented only by `acquire_in_tx`, not by `expire_due`; if the accounting breaks on the requeue-then-succeed path, tickets could succeed with the wrong attempt or trigger a dispatch early.
*Test:* `expire_requeue_then_complete_succeeds_and_emits_full_event_sequence` — assert final state Succeeded, attempt=2, and the exact ordered event log including `TicketRequeuedAfterLeaseExpiry.lease_id` matching the expired lease.

**`crates/voom-store/src/repo/leases.rs:739` — missing_sequence**
`force_release_lease(requeue=true)` → re-acquire → `release_lease` (succeed) is never tested; the existing test stops at the requeue state. Validates that `next_eligible_at=now`, the re-acquire bumps attempt to 2, and `TicketLeased` carries the correct attempt.
*Test:* `force_release_requeue_then_reacquire_succeeds` — assert Succeeded, `TicketLeased.attempt=2`, and `LeaseForceReleased + TicketRequeuedAfterForceRelease` precede the second `LeaseAcquired`.

**`crates/voom-control-plane/src/cases/leases.rs:189` — missing_sequence**
Diamond dependency: the existing test releases only parent_a and stops. Releasing parent_b → child promoted exactly once → acquired → succeeds is untested. A `mark_ready_if_unblocked_in_tx` count regression (LEFT JOIN vs JOIN) could promote early or strand the child.
*Test:* `diamond_dependency_child_promoted_exactly_once_after_both_parents_succeed`.

**`crates/voom-store/src/repo/tickets.rs:361` — missing_sequence**
A parent failing terminally → dependent permanently stranded in `Pending` (the count predicate treats `failed` as unsucceeded) is untested. A refactor to `NOT IN ('succeeded','failed')` would let stranded dependents run prematurely.
*Test:* `dependent_stays_pending_forever_when_parent_fails_terminal` — assert child Pending, `mark_ready_if_unblocked(child)` empty, `TicketFailedTerminal` for parent but no `TicketReady` for child.

### seq-migration-schema (voom-control-plane / voom-cli)

**`crates/voom-control-plane/src/lib.rs:493` — missing_sequence**
`HealthSnapshot::Partial` is never exercised end-to-end with a real pool; `lib_test.rs` only constructs the struct directly. The unused `migrator_through(N)` helper exists. A regression in the `health_from_pool` `Partial` arm or its `diagnostic()` would be invisible.
*Test:* `test_partial_schema_state_returns_db_partial_schema_health` — `migrator_through(N-1)` on a tempfile, open `HealthPlane`, call `health()`; assert `Partial { applied: N-1, expected: N }` and `DbPartialSchema` with a 'voom init' hint.

**`crates/voom-cli/src/commands/init.rs:26` — missing_sequence**
The `voom init` binary never emits a `DB_DIRTY_MIGRATION` envelope; only the store unit test confirms the code. No binary-level test asserts the envelope shape, hint, and exit code 2.
*Test:* `test_init_dirty_migration_emits_error_envelope` — dirty the DB, run the binary; assert exit 2, `error.code == DB_DIRTY_MIGRATION`, hint references `_sqlx_migrations` cleanup.

**`crates/voom-cli/src/commands/init.rs:26` — missing_sequence**
Same path for `DB_SCHEMA_TOO_NEW`; no binary-level test invokes `voom init` against a future-version DB.
*Test:* `test_init_too_new_emits_error_envelope` — insert a synthetic future-version row; assert exit 2, `error.code == DB_SCHEMA_TOO_NEW`.

**`crates/voom-cli/tests/health_envelope.rs:34` — missing_sequence**
The `voom health` binary never emits `DB_DIRTY_MIGRATION` or `DB_SCHEMA_TOO_NEW` envelopes; only uninitialized and corrupted-schema-meta are covered. A Dirty→TooNew mis-mapping or wrong hint would not be caught.
*Test:* `test_health_dirty_migration_envelope` and `test_health_schema_too_new_envelope` — assert exit 2 and the respective codes/hints.

### seq-worker-flow (voom-control-plane / voom-worker-protocol)

**`crates/voom-control-plane/src/artifact/worker.rs:215` — missing_sequence**
Handshake version negotiation is never tested in the launch→dispatch pipeline; the control plane never calls `client.handshake()` before dispatching, and the only handshake test runs in isolation. A worker built against a different protocol version proceeds to dispatch with a confusing crash.
*Test:* `test_handshake_version_mismatch_aborts_before_dispatch` — launch a real worker, `handshake(0)`; assert `UnsupportedProtocolVersion` and that a subsequent dispatch errors.

**`crates/voom-control-plane/src/artifact/worker.rs:372` — missing_sequence**
The progress-idle timeout (`tokio::timeout` → `WorkerTimeout`) never triggers `fail_lease` in any sequence test; the timeout arm is untested entirely. A hung worker silently holds the lease.
*Test:* `test_dispatch_progress_idle_timeout_fails_lease` — server sends `OperationResponse` but no progress frames, `progress_idle_deadline_ms=50`; assert `WorkerTimeout` and `fail_lease` transitions to retriable.

**`crates/voom-control-plane/src/artifact/worker.rs:641` and `:655` — bug**
`map_dispatch_protocol_error` routes `ProtocolError::DuplicateIdempotencyKey` (and `StaleWorkerEpoch`) to the catch-all → `MalformedWorkerResult`, a terminal failure class. A duplicate-in-flight key should be a transient conflict (`WorkerCrash`/retriable), not a terminal ticket failure; concurrent retries on lease expiry will permanently fail retriable tickets.
*Test:* `test_duplicate_idempotency_key_maps_to_worker_crash_not_malformed` — two concurrent same-key dispatches; assert the second's `VerifyWorkerError` carries `WorkerCrash`, not `MalformedWorkerResult`.

### seq-idempotency-concurrency (voom-control-plane / voom-store)

**`crates/voom-control-plane/src/cases/remote_execution.rs:375` — missing_sequence**
Two distinct workers racing for the same single ready ticket is untested; the existing test uses one worker with two tickets. The SQLite `WHERE state='ready'` UPDATE is the sole concurrency barrier — if bypassed, both workers would hold leases on one ticket.
*Test:* `remote_acquire_second_worker_gets_no_candidate_when_first_wins_ticket` — assert one lease, loser gets `Idle`/`NoCandidate`, ticket `leased`.

**`crates/voom-control-plane/src/cases/remote_execution.rs:265` — missing_sequence**
`remote_acquire` idempotency replay after the acquired lease is expired by `remote_recover` is untested. The replay returns the original `Leased` response pointing to a now-expired lease; the subsequent heartbeat/complete must return `CONFLICT`, not `INTERNAL`.
*Test:* `remote_acquire_replay_after_lease_expired_returns_stale_dispatch` — acquire L, recover, replay acquire (returns L), heartbeat L → `CONFLICT`; assert no new lease and ticket still `ready`.

**`crates/voom-control-plane/src/cases/remote_execution.rs:955` — missing_sequence**
The full `remote_fail` (retriable) → requeue → backoff → same worker re-acquires (attempt=2) loop is never tested as a unit; only individual steps. Validates backoff honoring and that the old fail idempotency key does not block the new acquire.
*Test:* `remote_fail_retriable_requeues_and_worker_reacquires_on_next_eligible` — acquire L1, fail (ArtifactUnavailable), acquire before next_eligible_at → Idle, acquire after → Leased L2 with attempt=2.

**`crates/voom-control-plane/src/cases/leases.rs:187` — missing_sequence**
Diamond dependency (C depends on A and B) at the ControlPlane layer — C promoted exactly once after both parents succeed, with events emitted — is untested. The repo unit test covers the count predicate in isolation but not the two-release ControlPlane sequence.
*Test:* `release_lease_diamond_dependency_promotes_child_exactly_once_after_both_parents` — release A (C pending, zero TicketReady), release B (C Ready, exactly one TicketReady).

---

## MEDIUM SEVERITY

### cp-cases (voom-control-plane)

**`crates/voom-control-plane/src/cases/remote_execution.rs:1590` — coverage_gap**
The `remote_node_heartbeat` retired-node rejection (live `NodeStatus::Retired` guard) is untested; only the replay-path test passes a retired state into an already-completed replay, not a live pre-reservation rejection.
*Test:* `remote_node_heartbeat_rejects_retired_node` — register, retire, heartbeat; assert `Conflict` and no idempotency record committed.

**`crates/voom-control-plane/src/cases/workers.rs:95` — coverage_gap**
`register_worker_for_node` `NotFound` path (unknown node_id) is untested; every test uses a valid node_id. Stale node_id callers should get `NOT_FOUND`, not a 500.
*Test:* `register_worker_for_node_rejects_unknown_node_id`.

**`crates/voom-control-plane/src/cases/workers.rs:115` — coverage_gap**
The `register_worker_for_node` heartbeat-expired guard (`expires_at <= now` on a still-`Registered` node) is a distinct path from the `Stale`-status branch and is untested; the existing test calls `mark_stale_nodes` first.
*Test:* `register_worker_for_node_rejects_registered_node_with_expired_heartbeat` — TTL=60s, advance clock 61s without `mark_stale_nodes`; assert `Conflict` containing "heartbeat expired".

**`crates/voom-control-plane/src/cases/tickets.rs:119` — coverage_gap**
`pre_lease_failure_ticket` attempt overflow (`checked_add(1)` → `Internal`) is untested; no test sets `attempt` to `u32::MAX`. The `Internal` code is distinct from `Config`/`Conflict`.
*Test:* `pre_lease_failure_rejects_ticket_at_max_attempt_count` — raw SQL UPDATE to `u32::MAX`, mark ready, call; assert `Internal`.

### cp-workflow (voom-control-plane)

**`crates/voom-control-plane/src/workflow/executor.rs:1884` — coverage_gap**
`handle_terminal_frame` `ProgressFrame::Result` with a non-object payload (`MalformedWorkerResult`) is untested.
*Test:* `result_frame_with_non_object_payload_fails_malformed` — emit a terminal Result with `payload = json!("not-an-object")`; assert `MalformedWorkerResult` and `held_lease_count == 0`.

**`crates/voom-control-plane/src/workflow/expansion.rs:51` — coverage_gap**
Scanner completion with an empty files list produces zero tickets; the workflow succeeds with `branch_count == 0` rather than signalling an empty scan — untested. A previously-populated library regressing to empty is indistinguishable from a full run.
*Test:* `scanner_completion_with_empty_file_list_creates_no_tickets_and_workflow_succeeds_with_zero_branches`.

**`crates/voom-control-plane/src/workflow/executor.rs:746` — missing_sequence**
No test runs the full `workflow_plan_from_compliance` → `submit_and_run` pipeline with two chained policy nodes (B depends on A). Combined with the line-730 wildcard bug, multi-step policy workflows have never been proven correct from plan to completion.
*Test:* `policy_workflow_with_chained_nodes_executes_both_in_order`.

### cp-artifact (voom-control-plane)

**`crates/voom-control-plane/src/artifact/stage.rs:284` — coverage_gap**
`record_staged_artifact` re-reads the file_version in-tx and returns `NotFound` if retired between preflight and the DB write; the `before_database_transaction` hook to inject this is never used.
*Test:* `test_stage_copy_cleans_up_when_source_version_retired_before_transaction` — hook retires the source; assert `NotFound`, staging file removed, `cleanup_succeeded` true.

**`crates/voom-control-plane/src/artifact/inspect.rs:232` — coverage_gap**
`derive_state` Pending-commit → `Staged` mapping is never exercised through `show_artifact`; existing `Staged` cases have no commit rows. A mid-commit crash leaves a pending row that must still report `Staged` for recovery tooling.
*Test:* `test_show_artifact_with_pending_commit_record_reports_staged` — insert a pending commit via `create_pending_commit_in_tx`, call `show_artifact`; assert `Staged` and `latest_commit.state == Pending`.

**`crates/voom-control-plane/src/artifact/bootstrap.rs:110` — coverage_gap**
`validate_builtin_worker` `node_id.is_some()` guard (returns `Conflict`) is untested; the conflicting-builtin test uses `node_id: None`.
*Test:* `test_builtin_worker_with_node_id_fails_loudly` — INSERT a `builtin.verify_artifact` row with a non-NULL node_id, call ensure; assert `Conflict`.

**`crates/voom-control-plane/src/artifact/inspect.rs:152` — missing_sequence**
`list_artifacts` with a state filter sets `handle_limit = None` and breaks in-memory at the limit; the only limit test uses `state: None` (SQL-limited path). The in-memory break is never verified.
*Test:* `test_list_artifacts_state_filter_respects_limit` — 5 staged + 1 committed, `list_artifacts(Staged, 2)`; assert exactly 2, newest first.

### cp-audio (voom-control-plane)

**`crates/voom-control-plane/src/audio/commit.rs:218` — coverage_gap**
`merge_audio_output_facts` has no unit test — normal merge, silent-skip on unknown `snapshot_stream_id`, and the early return on absent/non-array `streams` are all only reached via the ffmpeg-gated integration test. Diverging stream IDs silently produce an incomplete snapshot.
*Test:* `test_merge_audio_output_facts_updates_matching_streams_and_skips_unknown`.

**`crates/voom-control-plane/src/audio/mod.rs:341` — coverage_gap**
The transcode succeeded-event failure recovery branch (`if let Err ... Ok(report)` with `commit_recovery_required` and a non-zero `result_media_snapshot_id`) is untested; the only recovery-report test fails earlier via `FailingProbeDispatcher`. A refactor replacing the guard with `?` would revert to the buggy extract-style behavior undetected.
*Test:* `test_transcode_post_commit_succeeded_event_failure_returns_recovery_with_snapshot_id`.

**`crates/voom-control-plane/src/audio/dispatch.rs:423` — coverage_gap**
`bundled_ffmpeg_worker_command_from` (env override / current_exe fallback) is untested; the structurally-identical transcode version has three tests.
*Test:* `test_audio_bundled_ffmpeg_worker_command_from_prefers_env_override` — mirror the transcode tests.

**`crates/voom-control-plane/src/audio/mod_test.rs:128` — missing_sequence**
No unit test covers the complete extract success sequence (select → snapshot → plan → stage → dispatch → validate → DB row → verify → commit → succeeded event); the only extract test fails before selection. The transcode path covers most of its success sequence with mocks.
*Test:* `test_extract_success_emits_succeeded_event_with_correct_selection_context` — assert the succeeded event's `selected_stream`, `role`, `source_bundle_id`, and the bundle member row.

### cp-scan (voom-control-plane)

**`crates/voom-control-plane/src/scan/bootstrap.rs:96` — coverage_gap**
`validate_builtin_worker` `node_id.is_some()` guard for `builtin.ffprobe` is untested (the WorkerKind and non-live branches are tested).
*Test:* `test_builtin_ffprobe_with_node_id_fails_loudly` — INSERT with a non-None node_id; assert `Conflict`.

**`crates/voom-control-plane/src/scan/worker.rs:440` — coverage_gap**
`map_dispatch_protocol_error` `InvalidPayload` arm (WorkerCrash for `request:`/`body:` prefixes) and the `_` fallback (`MalformedWorkerResult`) are untested. WorkerCrash vs MalformedWorkerResult drives retry policy.
*Test:* `test_map_dispatch_protocol_error_invalid_payload_is_worker_crash` and `..._other_protocol_error_is_malformed`.

**`crates/voom-control-plane/src/scan/worker.rs:299` — coverage_gap**
`consume_probe_file_stream` three arms untested: non-Progress terminal as streaming frame (`MalformedWorkerResult`), Progress-as-terminal (`MalformedWorkerResult`), StreamEnd/Closed before terminal (`WorkerCrash`). Each carries a different public error code.
*Test:* `test_consume_probe_file_stream_stream_end_before_terminal` (and the Progress-terminal variant).

**`crates/voom-control-plane/src/scan/hash.rs:20` — coverage_gap**
`observe_candidate_file` error paths (open failure, metadata failure, read failure → `Internal`) are all untested; only the happy path is covered. A wrong code (BadArgs vs Internal) would change the CLI exit code.
*Test:* `test_observe_candidate_file_not_found` — assert `Internal` with "cannot open candidate file".

**`crates/voom-control-plane/src/scan/mod.rs:460` — coverage_gap**
`SkippedSymlink → SkippedUnsupportedExtension` mapping is never exercised through the full scan pipeline; `mod_test.rs` uses `.txt` files, not symlinks. A change to `SkippedInaccessible` would not be caught.
*Test:* `test_directory_scan_with_symlink_reports_as_skipped_unsupported`.

**`crates/voom-control-plane/src/scan/persist.rs:667` — missing_sequence**
The `AliasAttached` arm in `emit_ingest_events` is dead in the scan context (always `alias_proof: None`), but no test verifies the second-scan event sequence avoids `FileLocationAliased`. Dead code without coverage.
*Test:* `test_scan_same_file_twice_event_sequence` — assert two each of FileAssetCreated/FileVersionCreated/FileLocationRecorded, one IdentityEvidenceRecorded, no FileLocationAliased.

### cp-transcode-core (voom-control-plane)

**`crates/voom-control-plane/src/transcode/events.rs:65` — bug**
`record_succeeded` sets `staging_path = result.output.local_file_key.unwrap_or_default()`, but the ffmpeg worker always leaves `local_file_key` as `None`, so every `ArtifactTranscodeSucceeded` records an empty `staging_path`, while `ArtifactTranscodeStarted` records the real path. Audit trails are inconsistent; no test checks the field.
*Test:* `test_transcode_succeeded_event_staging_path_matches_actual_path` — assert the payload's `staging_path` ends with the real ticket/lease/file path, not empty.

**`crates/voom-control-plane/src/transcode/dispatch.rs:99` — coverage_gap**
`require_output_file_matches_result` staged-file/result checksum-mismatch branch is untested; `FakeTranscodeDispatcher` writes matching bytes. The last guard before staging.
*Test:* `test_require_output_file_matches_result_rejects_checksum_mismatch` — dispatcher writes 'real bytes' but reports another hash; assert `ArtifactChecksumMismatch` before DB writes.

**`crates/voom-control-plane/src/transcode/source.rs:22` and `:71` — coverage_gap**
`select_source` retired-version and retired-explicit-location branches (`NotFound`) are untested; the source test covers missing/wrong/non-local but never retired rows.
*Test:* `test_select_source_rejects_retired_version` and `test_select_source_rejects_retired_explicit_location`.

**`crates/voom-control-plane/src/transcode/commit.rs:90` — coverage_gap**
`transcode/commit.rs` and `events.rs` have no `*_test.rs` files; the synthetic JSON payload in `record_result_snapshot` (hard-coded keys) is unverified. The audio module has `commit_test.rs`/`events_test.rs`.
*Test:* `test_record_result_snapshot_payload_shape` — assert `payload['container'] == 'mkv'`, `payload['video_codec'] == 'hevc'`, `payload['source'] == 'transcode_video_result'`, and `source_lineage`.

**`crates/voom-control-plane/src/transcode/mod.rs:108` — missing_sequence**
No test for the partial-success state where the staged artifact row commits before verification fails — the rows remain but the flow returns an error. A retry could create a duplicate handle, violating add-only commit intent.
*Test:* `test_staged_artifact_persists_after_verification_failure` — `FailedVerifyDispatcher`; assert `VerificationFailure`, exactly one staging `artifact_handle`, zero commit records.

### store-routing (voom-store)

**`crates/voom-store/src/repo/leases.rs:372` — coverage_gap**
`heartbeat_in_tx` `Conflict` path (non-held lease) is untested; only the held-lease happy path. A caller with a stale lease_id would loop if the variant changed.
*Test:* `test_heartbeat_returns_conflict_for_non_held_lease` — acquire, release, heartbeat; assert `Conflict`.

**`crates/voom-store/src/repo/tickets.rs:356` — coverage_gap**
`mark_ready_if_unblocked_in_tx` `NotFound` (nonexistent ticket) is untested. The expansion path calls this after creating dependents; a race could trigger it.
*Test:* `test_mark_ready_if_unblocked_returns_not_found_for_missing_ticket` — `TicketId(99_999)`; assert `NotFound`.

**`crates/voom-store/src/repo/leases.rs:272` — coverage_gap**
`acquire_in_tx` zero/negative TTL `Config` guard is untested at the repo layer (only indirectly via remote_acquire). A `< 0` vs `<= 0` change would allow zero-TTL leases that expire immediately.
*Test:* `test_acquire_rejects_zero_and_negative_ttl`.

**`crates/voom-store/src/repo/tickets.rs:283` — coverage_gap**
`add_dependency_in_tx` `Conflict` on a `succeeded` or `failed` dependent is untested (only `ready`/`leased` are covered). A guard limited to {ready,leased} would corrupt the dependency graph.
*Test:* `test_add_dependency_rejects_succeeded_and_failed_dependent`.

**`crates/voom-store/src/repo/use_leases.rs:978` — coverage_gap**
`recover_stale_issuer_in_tx` `NotFound` branch (nonexistent UseLeaseId) is untested; success/Config/Conflict are covered. The operator orphan-lock-reclaim path needs a clear `NotFound`.
*Test:* `test_recover_stale_issuer_returns_not_found_for_missing_lease`.

### store-events (voom-store)

**`crates/voom-store/src/repo/events.rs:169` — bug**
On an empty cursor page, `next_cursor` is set to `page.cursor` via `.or(page.cursor)`, so a polling consumer re-issues the same cursor forever and never learns the stream is exhausted. Correct behavior is `next_cursor: None` when items are empty.
*Test:* `test_list_cursor_exhaustion` — append 2, page with limit=1, third call returns empty items AND `next_cursor: None`.

**`crates/voom-store/src/repo/events_test.rs:21` — coverage_gap**
`trace_id` round-trip is never tested with a non-None value; every test uses `trace_id: None`. A column-name or NULL-handling regression would not be caught.
*Test:* `test_trace_id_round_trips` — `Some(TraceId("abc-123"))`; assert the round-trip.

**`crates/voom-store/src/repo/events.rs:104-114` — coverage_gap**
`EventRepo::get` is never tested for a missing event_id (`Ok(None)`); a broken WHERE clause would serve stale rows.
*Test:* `test_get_returns_none_for_unknown_id` — `get(EventId(99999))`; assert `Ok(None)`.

**`crates/voom-store/src/repo/events.rs:221-226` — coverage_gap**
`inner_payload` fallback (non-`{payload: ...}` shape returns raw value) is unreachable for current variants but untested; a future unit-struct variant would silently write wrong JSON, failing `reassemble_event` on read.
*Test:* Unit-test `inner_payload` with a value lacking the `payload` key; assert it returns the input.

### store-jobs-nodes (voom-store)

**`crates/voom-store/src/repo/jobs.rs:157-167` — coverage_gap**
`jobs::get` `None` (unknown id) path is untested; a change to `fetch_one` would panic.
*Test:* `get_missing_job_returns_none`.

**`crates/voom-store/src/repo/jobs.rs:169-181` — coverage_gap**
`jobs::list_by_state` is tested only for `Open`; the terminal states (Succeeded/Failed/Cancelled) and their `as_str()` serialization are unverified. A typo would silently return an empty list.
*Test:* `list_by_state_returns_succeeded_jobs` (+ Failed, Cancelled).

**`crates/voom-store/src/repo/nodes.rs:284-287` — coverage_gap**
`nodes::heartbeat_in_tx` retired-node rejection (`Conflict`) is untested at the repo layer. Removing the guard would reactivate a retired node.
*Test:* `nodes_heartbeat_on_retired_node_returns_conflict`.

**`crates/voom-store/src/repo/nodes.rs:315-319` — coverage_gap**
`nodes::mark_stale_in_tx` candidate SELECT includes both `registered` and `active`, but the existing test seeds only `active` nodes. A registered-but-never-activated expired node becoming stale is unverified.
*Test:* `nodes_mark_stale_marks_expired_registered_node`.

**`crates/voom-store/src/repo/workers.rs:395-400` — coverage_gap**
`workers::retire_in_tx` retiring an already-retired worker (`status != 'retired'` guard) is untested; wrong-epoch is covered. Unlike nodes, there is no pre-check.
*Test:* `retire_already_retired_worker_returns_conflict`.

### store-artifacts (voom-store)

**`crates/voom-store/src/repo/artifacts.rs:1259` — coverage_gap**
`mark_commit_*_in_tx` on an already-terminal commit record (`Conflict` "not pending") is untested for any transition. Worker retries after a crash must receive a typed `Conflict`; removing the state guard would overwrite terminal records.
*Test:* `test_mark_commit_committed_on_already_committed_record` (+ the failed variant).

**`crates/voom-store/src/repo/artifacts.rs:858` — missing_sequence**
Sidecar commit re-validation rejecting a pending record when a newer verification supersedes the original (within the same tx) is untested. Stricter-than-intended semantics would spuriously reject legitimate audio sidecar commits.
*Test:* `test_sidecar_commit_rejected_after_newer_verification_supersedes_original`.

**`crates/voom-store/src/repo/artifacts.rs:853` — coverage_gap**
`record_verified_sidecar_commit_rows_in_tx` empty-`target_path` `Config` branch is untested; `create_pending_commit_in_tx` has no empty-string guard, so the path is reachable.
*Test:* `test_sidecar_commit_empty_target_path` — insert a pending record with empty target_path directly, then call with matching empty target_path; assert `Config`.

### store-policy (voom-store)

**`crates/voom-store/src/repo/policies.rs:102` — coverage_gap**
`validate_slug` rejection in `create_document_with_version` (`Config` for uppercase/spaces/empty) is untested at the Rust call site; `is_stable_token` is a copy of the one in voom-policy and could drift.
*Test:* `create_document_rejects_invalid_slug_format` — "Bad Slug" and empty string; assert `CONFIG_INVALID`.

**`crates/voom-store/src/repo/issues.rs:198` — coverage_gap**
`resolve_policy_noncompliant_by_dedupe_key_in_tx` `Ok(None)` (unknown or already-resolved key) is untested. The compliance handler must distinguish "was live and now resolved" from "was not live".
*Test:* `resolve_nonexistent_dedupe_key_returns_none` (+ already-resolved key).

**`crates/voom-store/src/repo/policies.rs:258` — coverage_gap**
`list_versions` returns `Ok(vec![])` for a nonexistent document — indistinguishable from a real empty list, undocumented and untested.
*Test:* `list_versions_for_nonexistent_document_returns_empty` — assert `Ok(vec![])` and document the contract.

**`crates/voom-store/src/repo/policy_inputs.rs:449` — coverage_gap**
`get_input_set_by_slug` `Ok(None)` (unknown slug) is untested.
*Test:* `get_input_set_by_slug_returns_none_for_unknown_slug`.

### store-safety (voom-store)

**`crates/voom-store/src/repo/remote_idempotency.rs:178` — coverage_gap**
`complete_in_tx` `rows_affected == 0` (not-reserved → `Conflict`) is untested; no test calls `complete_in_tx` against an unreserved key. The public `CONFLICT` contract.
*Test:* `complete_in_tx_with_missing_key_is_conflict`.

**`crates/voom-store/src/repo/scheduler_decisions.rs:671` — coverage_gap**
`create_or_suppress` for `NoCandidate`/`NoEligibleCandidate` + a suppression key (the suppress-and-merge clause) is untested; only `Idle` decisions are covered. A regression in the suppression predicate affecting only `NoCandidate` would not be caught.
*Test:* `no_candidate_decisions_are_suppressed_by_key` — two equivalent NoCandidate inputs with the same key; assert same id, `suppressed_count == 1`.

**`crates/voom-store/src/repo/scheduler_decisions.rs:741` — coverage_gap**
`link_selected_lease_after_empty_update_in_tx` `NotFound` branch (decision deleted out from under the caller) is untested; the idempotency-conflict path is covered.
*Test:* `link_selected_lease_not_found_after_empty_update` — create a decision, delete it via SQL, then link; assert `NotFound`.

**`crates/voom-store/src/repo/commit_safety_gate.rs:4054` — missing_sequence**
No single sequence asserts the full prepare → authorize → finalize(Applied) → Completed lifecycle with all three JSON columns (`closure_initial`, `closure_authorized`, `target_row_epochs`) consistent AND the ordered event sequence. The migration-0005 three-column CHECK invariant is the audit trail others rely on.
*Test:* `full_lifecycle_all_states_and_events` — after Completed, assert `state='completed'`, all three columns non-null, `finalized_at` non-null, and events `CommitIntentRecorded → CommitAuthorized → CommitCompleted`.

**`crates/voom-store/src/repo/scheduler_decisions.rs:453` — coverage_gap**
`SchedulerDecisionRepo::list` `node_id` and `ticket_id` filter arms are untested; only `worker_id` + `outcome` is covered. `node_id` uses `request_node_id = ? OR selected_node_id = ?` (multi-bind); a binding-order typo would return all rows.
*Test:* `list_filters_by_node_id` (+ `list_filters_by_ticket_id`).

### store-core (voom-store)

**`crates/voom-store/src/init.rs:136` — coverage_gap**
The post-migration-error `TooNew` arm (`SchemaTooNew` with "underlying error") is untested; only the pre-migration `TooNew` guard is. A concurrent peer migrating mid-flight would yield the wrong code/message.
*Test:* `init_post_migration_error_too_new_returns_schema_too_new_code`.

**`crates/voom-store/src/pool.rs:27` — coverage_gap**
`connect()/connect_or_create()` with an unparseable URL (`map_err` → `Database`/`DbUnreachable`) is untested. A URL typo yields an opaque error.
*Test:* `connect_with_unparseable_url_returns_db_unreachable` — assert `DB_UNREACHABLE` and the bad URL in the message.

### plan (voom-plan)

**`crates/voom-plan/src/planner.rs:95` — coverage_gap**
The `EmptyPolicyPhases` warning (empty `phase_order` → empty plan + warning, early return) is untested anywhere.
*Test:* `generate_plan_with_empty_phase_order_emits_warning` — assert empty nodes and one warning with `EmptyPolicyPhases`.

**`crates/voom-plan/src/planner.rs:1123` — bug**
`evaluate_remux_track_operations` returns `Ok(false)` early when there is no track operation and `has_remux_stream_fact_shape` is false, bypassing the video-presence check. A `SetContainer{mkv}` on a snapshot with `video_stream_count: 0` but no `streams` array gets a `Planned` node instead of `Blocked`, diverging from the streams-present path.
*Test:* `container_mkv_alone_blocks_when_stream_summary_has_zero_video_and_no_streams_array` — assert `Blocked` and document the correct diagnostic code.

**`crates/voom-plan/src/planner.rs:113` — coverage_gap**
A phase in `phase_order` but absent from `phases` (`InvalidPlanningRequest`, skipped with `continue`) is untested. Silently skipping a phase after a diagnostic could let callers treat a partial plan as valid.
*Test:* `generate_plan_with_phase_order_referencing_missing_phase_emits_diagnostic`.

**`crates/voom-plan/src/planner.rs:1152` — coverage_gap**
`SetDefaults` with a `First`-equivalent strategy on an absent target track type (`InsufficientSnapshotFacts`) is untested; only the `None`/`Preserve` NoOp cases are covered.
*Test:* `track_remux_set_default_first_blocks_when_target_track_kind_is_absent`.

**`crates/voom-plan/src/planner.rs:905` — coverage_gap**
`RuleMatchMode::All` with an `Unknown` condition (blocks that rule but continues evaluation) is untested; only `First` mode is. `All` differs from `First` (which breaks).
*Test:* `rules_all_unknown_condition_blocks_that_rule_but_continues_evaluation`.

**`crates/voom-plan/src/planner.rs:1195` — missing_sequence**
`RemoveTracks { filter: None }` producing a `Planned` node (remove all tracks of a kind) is untested; the only RemoveTracks test triggers a pre-check rejection. A change to `Ok(false)` would go undetected.
*Test:* `track_remux_remove_all_subtitle_tracks_plans_when_subtitle_present`.

### policy (voom-policy)

**`crates/voom-policy/src/validate.rs:310` — coverage_gap**
`DeferredExecutionOperation` (synthesize/verify) is untested at both the top-level and nested (`validate_nested_operation`, line 386) sites.
*Test:* `test_rejects_synthesize_and_verify` (+ a nested `when` variant).

**`crates/voom-policy/src/validate.rs:64` — coverage_gap**
The 1 MiB `SourceSizeExceeded` check is exercised by no test. A typo in the constant or `>` vs `>=` would break the guard silently.
*Test:* `test_rejects_source_exceeding_1mib`.

**`crates/voom-policy/src/pipeline.rs:32` — coverage_gap**
The public `validate_policy` function (distinct from `compile_policy`) has zero coverage — parse-error wrapping, validation-error path, and warnings-on-success are all unverified.
*Test:* `test_validate_policy_returns_warnings_for_valid_policy` and `test_validate_policy_fails_on_parse_error`.

**`crates/voom-policy/src/validate.rs:57` — coverage_gap**
Empty/whitespace-only policy name (`UnexpectedToken`, "policy name must not be empty") is untested; the parser accepts `policy "" { ... }`. An empty name breaks `slug()` and downstream consumers.
*Test:* `test_rejects_empty_policy_name`.

**`crates/voom-policy/src/validate.rs:433` — coverage_gap**
Misplaced track filter (`where` not directly after the target → `UnknownPhaseStatementOrOperation`) is untested.
*Test:* `test_rejects_track_filter_not_immediately_after_target` — `keep audio extra where lang in [eng]`.

**`crates/voom-policy/src/compiled.rs:455` — missing_sequence**
No test exercises a phase with `depends_on` + non-null `skip_if` + non-null `on_error` simultaneously, checking all three `CompiledPhase` fields after compile. The three fields come from three independent private functions; cross-wiring would only surface if all three are asserted together.
*Test:* `test_compile_policy_phase_full_controls`.

### worker-protocol (voom-worker-protocol)

**`crates/voom-worker-protocol/src/envelope.rs:164` — coverage_gap**
`ProtocolError::WorkerRetired` has no serde round-trip test and is never constructed in production. Its wire code `WORKER_RETIRED` would silently break on a rename/structural change.
*Test:* `protocol_error_worker_retired_round_trips`.

**`crates/voom-worker-protocol/src/remux.rs:6` — coverage_gap**
`is_supported_remux_container` (case-insensitive via `eq_ignore_ascii_case`) has no tests, unlike `is_supported_transcode_video_container` (exact equality, tested both cases). The asymmetry is unpinned.
*Test:* `remux_container_helper_accepts_case_variants` — assert 'mkv'/'MKV'/'Mkv' true, 'mp4' false.

**`crates/voom-worker-protocol/src/http.rs:435` — coverage_gap**
`enforce_version` returns `InvalidPayload` (not `UnsupportedProtocolVersion`) for a missing header; no dedicated test pins this. The conformance test sends no headers at all and accepts either variant.
*Test:* `test_missing_protocol_version_header_returns_invalid_payload`.

**`crates/voom-worker-protocol/src/http.rs:540` — missing_sequence**
No test for the streaming idempotency race where the terminal frame precedes `set_finalizer` (the double-check at http.rs:204-208). A test firing the terminal before the finalizer is set would prove the double-check prevents a missed completion.
*Test:* `test_streaming_replay_when_terminal_precedes_finalizer`.

### cli (voom-cli)

**`crates/voom-cli/src/commands/node.rs:178` — coverage_gap**
`node show` NOT_FOUND path (`Ok(None)` → exit 2) is untested; the existing test uses a valid id. Worker show and scheduler show share this gap.
*Test:* `node_show_missing_id_returns_not_found_envelope`.

**`crates/voom-cli/src/commands/worker.rs:168` — coverage_gap**
`worker show` NOT_FOUND path is untested.
*Test:* `worker_show_missing_id_returns_not_found_envelope`.

**`crates/voom-cli/src/commands/scheduler.rs:158` — coverage_gap**
`scheduler decisions show` NOT_FOUND path is untested.
*Test:* `scheduler_decisions_show_missing_id_returns_not_found`.

**`crates/voom-cli/src/commands/scan.rs:129` — coverage_gap**
`validate_explicit_path` symlink rejection (`is_symlink()` → BAD_ARGS) is untested; only unsupported-extension on a regular file is covered.
*Test:* `scan_symlink_path_is_bad_args` — exit 1, `BAD_ARGS`, message contains 'symlink'.

**`crates/voom-cli/src/commands/scan.rs:144` — coverage_gap**
`validate_explicit_path` special-file fallthrough (FIFO/device → BAD_ARGS) is untested; trivially exercisable with `mkfifo` on Linux.
*Test:* `scan_named_pipe_path_is_bad_args` (Unix).

### api (voom-api)

**`crates/voom-api/src/execution.rs:122` — coverage_gap**
`not_configured_response` is tested only for acquire; node_heartbeat, lease_heartbeat, complete, fail are uncovered. A wrong reused COMMAND constant would not be caught.
*Test:* `test_unconfigured_node_heartbeat_returns_correct_command_in_envelope` (+ the other three).

**`crates/voom-api/src/execution.rs:346` — coverage_gap**
Empty bearer token and empty idempotency-key `is_empty` guards (lines 346, 358) are never exercised; the credential test drops both headers entirely (hitting the `ok_or_else` arms instead).
*Test:* `test_acquire_rejects_empty_bearer_token` (+ empty idempotency key).

**`crates/voom-api/tests/remote_execution_route.rs:93` — coverage_gap**
The `fail` route never reaches `TicketState::Failed` (max_attempts exhausted) via HTTP; the fixture always uses `max_attempts: 2`. The terminal-failure interaction with the scheduler is untested end-to-end at the API boundary.
*Test:* `test_fail_on_last_attempt_transitions_ticket_to_failed` — `max_attempts: 1`; assert 200 and `Failed`.

**`crates/voom-api/tests/health_route.rs:39` — coverage_gap**
Health tests never assert `schema_version == "0"` or `command == "health"`; an accidental `SCHEMA_VERSION` bump or wrong command field would be a silent breaking API change.
*Test:* Add the two assertions to the initialized/uninitialized health tests.

### core (voom-core)

**`crates/voom-core/src/config.rs:95` — coverage_gap**
`Config.log_level_override` and the `VOOM_LOG_LEVEL` env path have zero tests — no override-priority, env-fallback, or default-"info" coverage; `database_url` has all three.
*Test:* `test_log_level_override_takes_priority`, `test_log_level_from_env`, `test_log_level_defaults_to_info`.

**`crates/voom-core/src/config.rs:98` — coverage_gap**
`log_format_override` priority over `VOOM_LOG_FORMAT` is untested (only env-based parsing is); an inverted `.or_else()` would silently ignore the CLI flag.
*Test:* `test_log_format_override_takes_priority_over_env`.

**`crates/voom-core/src/failure_test.rs:44` — coverage_gap**
`retriable_partition_matches_spec` omits `ProgressTimeout` (retriable) and `AmbiguousWorkerSelection` (operator-required), both Sprint-2 variants. A misclassification would pass the unit tests; only the cross-crate conformance suite catches it.
*Test:* Add both variants to the respective arrays in the existing test.

**`crates/voom-core/src/failure.rs:165` — bug**
`ProgressTimeout.into_error_code()` returns `WorkerTimeout`, and `from_error_code(WorkerTimeout)` returns `WorkerTimeout`, not `ProgressTimeout` — a lossy round-trip that is undocumented and untested. Event-replay callers silently drop `ProgressTimeout` semantics.
*Test:* `test_from_error_code_worker_timeout_returns_worker_timeout_not_progress_timeout` — assert the mapping and add a doc comment noting the intentional alias.

### workers (voom-ffmpeg-worker / voom-ffprobe-worker)

**`crates/voom-ffmpeg-worker/src/handler.rs:134` — coverage_gap**
The ffmpeg no-config dispatch path (config=None + a supported operation → `config_invalid` frame) is unreachable by any test; the only `handle_operation` test uses `ProbeFile`, rejected earlier as `UnknownOperation`.
*Test:* `transcode_video_with_missing_config_returns_config_invalid_error`.

**`crates/voom-ffmpeg-worker/src/handler.rs:201` — coverage_gap**
`overwrite=true` rejection (`ConfigInvalid`) is never tested for any of the three ffmpeg operations; the mkvtoolnix worker has this test. The guard prevents output clobbering.
*Test:* `transcode_video_overwrite_true_is_config_invalid` (+ audio and extract).

**`crates/voom-ffprobe-worker/src/observe.rs:9` — bug**
The ffprobe worker uses `tokio::fs::metadata` (follows symlinks), while ffmpeg/mkvtoolnix/verify-artifact use `symlink_metadata` + `O_NOFOLLOW`. ffprobe silently accepts a symlink-to-regular-file that the others reject as `ArtifactUnavailable`. No test exercises symlink input.
*Test:* `observe_file_facts_rejects_symlink_to_regular_file` — assert `ArtifactUnavailable`.

### seq-work-lifecycle (voom-store)

**`crates/voom-store/src/repo/leases.rs:364` — missing_sequence**
Heartbeat on an expired lease (state='expired') is untested; the WHERE `state='held'` clause is the sole guard, tested only for the happy path. There is also no ControlPlane-level heartbeat test at all. A stale worker accepted on an expired lease could create duplicate `LeaseExpired` events.
*Test:* `heartbeat_on_expired_lease_returns_conflict` — acquire, `expire_due`, heartbeat; assert `Conflict` and no side effects. Add a ControlPlane happy-path heartbeat test too.

**`crates/voom-control-plane/src/cases/leases.rs:373` — coverage_gap**
`TicketRequeuedAfterLeaseExpiry` payload cross-references (`lease_id`, `ticket_id`) are never decoded; existing tests only count events. A swapped pairing would carry invalid cross-references that audit tools would join incorrectly.
*Test:* `expire_due_requeued_event_payload_cross_references` — decode each event and verify the lease/ticket id pairing.

### seq-migration-schema (voom-api / voom-store)

**`crates/voom-api/src/lib.rs:138` — missing_sequence**
The API `/health` route never tests `DbUnreachable` (missing DB file) via HTTP; all five existing tests open `HealthPlane` successfully first. The 503 branch is dead from the test suite's view.
*Test:* `test_health_on_missing_db_path_returns_503` — assert 503 and `DB_UNREACHABLE`.

**`crates/voom-store/src/init.rs:35` — missing_sequence**
`init()`/`init_on()` against a foreign DB (unrelated tables, no `_sqlx_migrations` → `CONFIG_INVALID`) is untested at the init level; only `probe_schema` is tested directly. A refactor bypassing `probe_schema` would break the "refuse and leave unmodified" invariant.
*Test:* `test_init_refuses_foreign_database` — assert `CONFIG_INVALID` and the foreign table unmodified.

**`crates/voom-store/src/init.rs:158` — missing_sequence**
The Partial → `init()` upgrade path is never exercised on an on-disk DB with N>0 applied rows; the existing test uses the degenerate zero-applied case. `saturating_sub(before_count)` for N>0 and single event emission are unverified.
*Test:* `test_init_from_real_partial_state_reports_delta` — `migrator_through(N-2)`, init; assert `migrations_applied == 2`, `already_initialized == false`, Current, exactly one `schema.initialized` event.

---

## LOW SEVERITY

### cp-workflow (voom-control-plane)

**`crates/voom-control-plane/src/workflow/executor.rs:2020` — coverage_gap**
`apply_chaos_payload_override` with a non-object rendered payload (e.g. `Value::Null`) + a chaos `payload_mode` returns `Config`; untested (chaos tests use object payloads).
*Test:* `chaos_payload_override_rejects_non_object_payload`.

### cp-artifact (voom-control-plane)

**`crates/voom-control-plane/src/artifact/stage.rs:402` — coverage_gap**
The cleanup report's `error_code` and `message` fields are never asserted; the CLI reads them. A removed/renamed key would not be caught.
*Test:* Extend the cleanup test to assert `data['error_code'] == DbUnreachable.as_str()` and `data['message'].is_string()`.

### cp-scan (voom-control-plane)

**`crates/voom-control-plane/src/scan/discovery.rs:232` — coverage_gap**
`best_sidecar_candidate` tie-breaking (equal stem lengths → reverse-lexicographic, picks the largest path) is untested; the existing test uses different-length stems.
*Test:* `test_sidecar_tie_breaks_on_largest_candidate_path` — Movie.avi + Movie.mkv + Movie.eng.srt; assert and document the selection.

### store-routing (voom-store)

**`crates/voom-store/src/repo/leases.rs:977` — missing_sequence**
No sequence test verifies that a ticket requeued after `expire_due` with a future `next_eligible_at` cannot be immediately re-acquired. This would expose whether the `expire_due` omission (the high-severity bug above) is intentional.
*Test:* `test_acquire_after_expire_respects_prior_next_eligible_at`.

### store-artifacts (voom-store)

**`crates/voom-store/src/repo/artifact_access_plans.rs:457` — bug**
`validate_plan_coherence_in_tx` "workers id= not found" branch is dead under FK constraints (the LEFT JOIN always finds a live FK target after the equality check). Misleads maintainers.
*Test:* Document the unreachability or replace the `NotFound` branch with `Internal` to make the invariant explicit.

**`crates/voom-store/src/repo/bundles.rs:188` — coverage_gap**
`BundleRepo::get()` `None` (nonexistent bundle) is untested; a change to `fetch_one` would panic.
*Test:* `test_get_bundle_nonexistent_returns_none`.

### plan (voom-plan)

**`crates/voom-plan/src/planner.rs:1440` — coverage_gap**
`CompiledCondition::FieldExists` in `evaluate_condition` is never tested. A regression returning `Unknown` would wrongly block operations gated on a present field.
*Test:* `field_exists_condition_resolves_when_snapshot_field_is_present`.

**`crates/voom-plan/src/planner.rs:1304` — coverage_gap**
`transcode_video_shape` never validates that the target container is worker-supported (only the codec), unlike remux which rejects non-mkv. A `TranscodeVideo { container: 'mp4' }` produces a `Planned` node with an unreachable capability.
*Test:* `transcode_video_plans_when_target_container_is_mp4` — pin and document the current behavior.

### policy (voom-policy)

**`crates/voom-policy/src/compiled.rs:526` — coverage_gap**
Phase-level `on_error` compilation (vs config-level) is untested; if `error_strategy` mapped "abort" to Continue, no test would catch it for phase-level.
*Test:* `test_compile_policy_phase_on_error_abort` — assert `Some(ErrorStrategy::Abort)`.

### worker-protocol (voom-worker-protocol)

**`crates/voom-worker-protocol/src/http_test.rs:123` — coverage_gap**
`StreamingFrameWriter::finish()` is a no-op (the real cleanup is in Drop); the "call finish() after your last frame" contract is silently meaningless. Calling finish() without a terminal frame triggers an Abort only on Drop, with no warning from finish().
*Test:* `test_streaming_writer_finish_without_terminal_triggers_abort`.

### events (voom-events)

**`crates/voom-events/src/assertion_test.rs:26` — coverage_gap**
`from_str_rejects_unknown_value` checks only the message, not `matches!(err, VoomError::Database(_))` as the `EventKind` and `SubjectType` tests do. A change to `Internal` would still pass.
*Test:* Add the `matches!` assertion mirroring `kind_test.rs:341`.

### cli (voom-cli)

**`crates/voom-cli/src/commands/token_source.rs:92` — coverage_gap**
`trim_one_trailing_newline` CRLF branch is untested (only LF is covered). Windows-written token files use CRLF; a broken branch leaves a `\r` suffix causing silent auth failures.
*Test:* `trim_crlf_trailing_newline_from_token_file`.

**`crates/voom-cli/src/commands/token_source.rs:57` — coverage_gap**
`read_token` empty-after-trim BAD_ARGS path is untested; a token file of only `\n` would hit it. If removed, an empty token reaches auth as an empty string (CONFLICT instead of BAD_ARGS).
*Test:* `read_token_empty_file_returns_bad_args`.

**`crates/voom-cli/src/commands/worker.rs:185` — coverage_gap**
`emit_inspection` `Ok(None)` INTERNAL path ("missing after registration") is untested — a defensive invariant-violation path that fires if the re-read misses the just-committed write.
*Test:* `worker_register_missing_after_registration_emits_internal` (fault injection or a stub ControlPlane returning `Ok(None)`).

### api (voom-api)

**`crates/voom-api/src/execution.rs:345` — coverage_gap**
`bearer()` wrong-scheme rejection (`strip_prefix("Bearer ")` absent → "must use Bearer scheme") is only implicitly tested; the credential test drops the header entirely (hitting the earlier arm). The distinction between "missing header" and "wrong scheme" is unverified.
*Test:* `test_acquire_rejects_wrong_auth_scheme` — `Authorization: Basic abc123`; assert 400 BAD_ARGS with "Bearer scheme".

### core (voom-core)

**`crates/voom-core/src/lib.rs:38-48` — coverage_gap**
The `PROTOCOL_VERSION` range invariant (`MIN <= VERSION <= MAX`) has no assertion; drift between VERSION and the bounds would cause every worker connection to fail at runtime while all unit tests pass.
*Test:* `test_protocol_version_within_supported_range`.

### seq-worker-flow (voom-control-plane)

**`crates/voom-control-plane/src/cases/workers.rs:113` — missing_sequence**
The `register_worker_for_node` heartbeat-expiry boundary (`expires_at == now`) is not tested; the existing stale test advances 61s, missing the exact boundary. An off-by-one in the TTL check could allow registration at the expiry instant.
*Test:* `test_register_worker_for_node_rejects_at_exact_heartbeat_boundary` — advance to exactly T0+60s; assert `Conflict`.

### seq-migration-schema (voom-api)

**`crates/voom-api/src/lib.rs:115` — missing_sequence**
The API `/health` route never tests `HealthSnapshot::Partial` (applied < expected) via HTTP — the Ok-branch non-Current route, distinct from the corrupted-schema-meta Err-branch. A misroute to 200/500 or `DB_UNINITIALIZED` would misdiagnose monitoring.
*Test:* `test_health_on_partial_schema_returns_503_db_partial_schema` — `migrator_through(N-1)`, GET /health; assert 503, `DB_PARTIAL_SCHEMA`, hint contains 'voom init'.

### seq-idempotency-concurrency (voom-control-plane / voom-store)

**`crates/voom-control-plane/src/cases/remote_execution.rs:800` — missing_sequence**
No test verifies that the `remote_complete` route_key scoping (which includes the specific `lease_id`) prevents idempotency-key collisions across separate lease cycles for the same worker. The invariant is safe by construction but unverified.
*Test:* `remote_complete_idempotency_scoped_to_lease_id_prevents_cross_lease_replay`.

**`crates/voom-store/tests/lease_expire_and_recover.rs:41` — missing_sequence**
No test covers `expire_due` followed by immediate re-acquire on the same ticket through the remote-execution idempotency wrapper — the full recovery loop production hits on every worker restart.
*Test:* `remote_recover_then_reacquire_picks_up_requeued_ticket`.

**`crates/voom-store/src/repo/remote_idempotency.rs:144` — missing_sequence**
`reserve_or_replay_in_tx` `Conflict` on a concurrent in-progress key is tested only with two sequential transactions in one test; the real scenario (same transaction holding in_progress across an await, under SQLite WAL) is untested at the HTTP layer.
*Test:* `remote_acquire_concurrent_same_idempotency_key_one_succeeds_other_conflicts` — two concurrent same-key router requests via `tokio::join!`; one 200, one CONFLICT.