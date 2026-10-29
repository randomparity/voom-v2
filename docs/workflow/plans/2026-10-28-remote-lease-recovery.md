# Issue 783 implementation plan

Design: ../specs/2026-10-28-remote-lease-recovery-design.md.
ADR: ../../adr/0118-automatic-lease-recovery-and-scan-terminality.md.
Goal: recover remote lease/scan truth after heartbeat loss without post-fence work.
Architecture: explicit API lifecycle supervision calls control-plane recovery;
caller-owned transactions and store capabilities reconcile scan state. Node fencing
remains local and is diagnosed honestly. Stack: workspace Rust/tokio/sqlx/axum.
Scope q783-bea55c28; L denominator 1000.
Expected implementation size: 700–1000 changed lines (L) — three transactional/service/diagnostic units and their boundary tests

## Global Constraints

Use the existing workspace Rust/tokio/sqlx/axum stack; add no external packages
or versions. Node diagnostics may use existing workspace tracing and
tracing-subscriber dependencies.
SQL belongs to voom-store; transactions are caller-owned through voom_store::tx.
Preserve typed IDs, checked persisted values and existing public error/wire contracts.
Real SQLite tests use real Tokio time and ManualClock for domain time.
Unit tests use sibling _test.rs files. Run routine guardrails through just.
No audio-policy, polling/TTL configuration, automatic resumption or fencing weakening.


## File and ownership map

- voom-store repo scan/ticket modules: SQL, checked typed projections and bounded
  recovery candidates (C2/C4); existing store transaction API retained.
- voom-control-plane scan recovery module and leases/remote_execution recovery:
  routing linkage validation, caller-owned transactions and lifecycle events
  (C2/C4); existing expire_due/remote_recover remain compatible explicit entry points.
- voom-api recovery module/lib export/main assembly: explicitly polled service
  future and process supervision (C1/C4); router/read-side construction stays passive.
- voom-node-agent runtime and sibling tests: honest local fence outcome/diagnostic
  (C3); byte cancellation, capacity and acknowledged terminality remain distinct.
- Corresponding sibling/integration tests, this spec/ADR/plan and an operator runbook.
No schema migration, new operation name, new wire field or audio-policy change.

## Task 1 — Durable scan failure recovery

Files: crates/voom-control-plane/src/scan/recovery.rs and recovery_test.rs (new),
scan/mod.rs, cases/execution/leases.rs, cases/execution/remote_execution/recover.rs;
crates/voom-store/src/repo/execution/tickets.rs and tickets_test.rs.
Interfaces:
- New SqliteTicketRepo::failed_scan_run_candidates_in_tx(&self,
  tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<Ticket>, VoomError>; bounded
  active-or-malformed canonical routing candidates in stable ticket order.
- New ControlPlane::fail_scan_for_failed_ticket_in_tx(&self,
  tx: &mut Transaction<'_, Sqlite>, ticket: &Ticket, now: OffsetDateTime)
  -> Result<(), VoomError>; validates typed linkage, preserves unrelated/terminal
  work and appends the existing failed lifecycle fact for active linked scans.
- New ControlPlane::recover_failed_scan_runs(&self, now: OffsetDateTime)
  -> Result<(), VoomError>; repairs one bounded residual unit through the same helper.
- Existing public remote_recover(now) and expire_due(now) signatures stay unchanged;
  they consume these helpers. Existing store get_in_tx and terminalize_in_tx stay.


Implement store projections and control-plane recovery together; they are one
transactional unit. First reproduce a healthy owner/long scan deadline with an
expired exhausted routing lease, then the existing already-failed ticket orphan.
Add typed linkage validation and preserve existing generic-ticket behavior.
Close newly expired scan sessions inside the lease-expiry transaction after its
current facts. Reconcile old orphan pairs in bounded deterministic units after
existing remote_recover phases. Preserve succeeded/terminal sessions and requeued
work, and validate corruption before classification. No SQL in production CP.
Verification inventory (each item Mode: focused-test):
- CP scan/remote recovery tests: expected red is Running scan after expired ticket;
  green command cargo test -p voom-control-plane --all-features scan_recovery.
- Repeat/reopen and concurrent recovery: expected red is missing convergence;
  green same filter; verify one failed lifecycle fact and existing fact order.
- Store/CP malformed-ID, wrong-root/owner and corrupt-row cases: expected red is
  absent checked association or partial mutation; green cargo test -p voom-store
  scan_recovery and CP filter above. Protect generic scan-library payloads.
- Inject an event failure and prove expiry/ticket/scan rollback as one unit.
- Mode: focused-test, scan_recovery_skips_terminal_history_before_candidate_cap:
  seed more than a batch of old terminal-session tickets before an active orphan;
  expected red is starvation, green same store/CP scan_recovery filters. Assert
  candidate cap and multiple live-batch progress with scan_recovery_drains_multiple_live_batches.

## Task 2 — API-owned periodic recovery

Files: crates/voom-api/src/recovery.rs and recovery_test.rs (new), lib.rs, main.rs,
main_test.rs; directly covering API integration test and operator runbook.
Interfaces:
- New voom_api::recovery::run(control_plane: ControlPlane) -> impl Future<Output=()>;
  it runs only when explicitly polled by the server lifetime.
- New private main helper combines a termination Future<Output=Result<(),
  StartupDiagnostic>> with a recovery Future<Output=()> and returns
  Result<(), StartupDiagnostic>. Unexpected recovery return is INTERNAL.
- Existing RunningServer::shutdown_on<F,S>(self, signal:F)->Result<S,ServerError>
  remains the listener owner. No task spawned by router_with_control_plane.


Use existing public remote_recover signature, so this task can be built independently
of Task 1. Add a polled recovery loop with immediate first pass, five-second spacing,
thirty-second pass timeout and bounded safe logging. Compose it into actual main
termination/server lifecycle without detached tasks or constructor side effects.
Add operator documentation describing automatic recovery, failure diagnostics,
and passive library embedding.
Verification inventory (each item Mode: focused-test):
- Real API process or service assembly with expired lease and live owner: expected
  red is held lease without a workflow executor; green cargo test -p voom-api recovery.
- Termination/listener stop/unexpected recovery return: expected red is missing
  service composition/supervision; same green filter. Use explicit notifications
  for cancellation ordering and prove no recovery future survives its owner.
- Pass timeout/error and cadence: fake-only paused-time test proves no overlap or
  catch-up burst; real SQLite tests never pause Tokio time.

## Task 3 — Local fence diagnostics

Files: crates/voom-node-agent/src/runtime.rs and runtime_test.rs, Cargo.toml,
src/main.rs and main_test.rs.
Interfaces: existing LeaseOutcome::Fenced distinguishes local fencing;
Confirmed remote completion/failure settlement paths retain their behavior.
Remove the private TerminalElsewhere variant, whose only producer was the
misclassified heartbeat fence; do not invent a remote observer to keep it alive. Existing
run_lease and settle_lease signatures stay; no wire enum/fields are added.


Initialize fixed WARN JSON stderr logging in binary startup only; library
construction stays passive. Promote existing workspace instrumentation dependencies
without adding configuration or changing CLI/stdout/error contracts.
Keep fencing/cancellation behavior and distinguish it from confirmed remote
terminality using existing outcomes. Add safe structured warning with typed IDs
for the heartbeat fence and the completion-settlement fence race.
Verification inventory (each item Mode: focused-test):
- Node fake-client hanging-heartbeat test: expected red is no local-fence event;
  green cargo test -p voom-node-agent local_fence. Preserve zero late fail/complete
  acknowledgement, dispatch cancellation and returned permit.
- Direct binary startup-assembly subprocess coverage: expected red is absent JSON
  stderr diagnostic; green cargo test -p voom-node-agent --bin voom-node-agent
  local_fence_startup. Actual fence emission is covered separately by runtime tests.
- Retain existing TTL, shutdown, terminal retry and worker-capacity tests;
  green cargo test -p voom-node-agent --lib.

## Assembled validation

Use .agent/sdd/env.sh for isolated debug0/nonincremental build configuration.
Baseline: just test on clean main; final just fmt-check, just lint and just ci.
Focused tests are appropriate at each changed trust boundary; docs need no prose
snapshot tests. Existing opt-in ignored tests remain disclosed, not silently run
or ungated. Commit through installed hooks, independent branch/security reviews,
simplification, PR/CI and merge-ready handoff follow the quest workflow.

## Step order and rollback

Within each task: write the named boundary regression, run its stated command
and retain the expected red, implement the task's interfaces/behavior above, run
the same command green and its relevant sibling suite, then commit through hooks.
Event/transaction failure must roll back scan and lease/ticket mutations. Service
shutdown drops recovery before returning; tests reap listeners/processes. No
manual live-database SQL or media retry is part of this plan. Documentation prose
uses Mode: task-test-not-applicable: no executable consumer judges its wording;
ADR-index/link checks and human review validate format and correspondence.
