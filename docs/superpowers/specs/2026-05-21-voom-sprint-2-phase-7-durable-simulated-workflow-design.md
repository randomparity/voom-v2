---
name: voom-sprint-2-phase-7-durable-simulated-workflow-design
description: Sprint 2 Phase 7 design — durable simulated media workflow through the real control-plane ticket, lease, scheduler, and worker protocol path.
status: proposed
date: 2026-05-21
sprint: 2
phase: 7
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
predecessor_specs:
  - docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-6-fake-providers-conformance-closeout-design.md
scope: durable bounded-fanout synthetic workflow through process-backed fake workers, seeded timing, ticket/lease lifecycle stress, chaos closeout, and scheduler throughput reporting
---

# Sprint 2 Phase 7 — Durable Simulated Workflow

## 1. Goal

Phase 7 builds the full simulated media workflow promised after the
Phase 6 fake-provider foundation. The workflow runs through durable
control-plane jobs, tickets, leases, worker selection, process-backed
worker dispatch, progress streaming, lease heartbeats, dependency
promotion, and terminal job state.

This phase is the Sprint 2 capstone path. It should show that a
synthetic end-to-end media plan can run through the real scheduler
surface available in this repository, not merely through direct
protocol calls. It also closes the Sprint 2 exit criteria around chaos
coverage and scheduler-throughput reporting from the durable workflow
path.

For Sprint 2 closeout, "real scheduler surface" means the implemented
`WorkflowExecutor` loop: durable ticket dequeue, `SingleWorkerPerKindSelector`
selection, durable lease acquire/release/fail, process-backed protocol
dispatch, and watchdog-owned terminal state. The earlier parent spec's
standalone `LocalWorkerSupervisor`/worker-incarnation outbox remains
later-sprint design context and is not required for Phase 7 exit.

Phase 7 does not introduce a policy language, external workflow DSL,
production daemon loop, real media tools, or multi-worker scoring. The
workflow model is reusable and shaped for later high-level nodes, but
Phase 7 executes operation-backed nodes only.

## 2. Architecture

Phase 7 adds a workflow executor inside `voom-control-plane`. The
executor turns a typed `WorkflowPlan` into durable jobs, tickets,
ticket dependencies, leases, worker dispatches, and terminal job state.
It composes existing control-plane use cases for worker registration,
ticket creation, lease acquire/release/fail, dependency promotion, and
job success/failure.

Core units:

- `WorkflowPlan`: typed DAG with an id, seed, bounded fan-out
  settings, default lease TTL, default timing bounds,
  `max_in_flight_dispatches`, and nodes.
- `WorkflowNode`: enum reserved for future high-level nodes. Phase 7
  accepts only operation-backed nodes.
- `OperationNode`: node id, `OperationKind`, static payload,
  dependencies, output bindings, optional fan-out role, and optional
  timing overrides.
- `WorkflowExecutor`: validates and runs plans through
  `ControlPlane`, `WorkerSelector`, process-backed worker runtimes,
  and `voom_worker_protocol::HttpClient`.
- `WorkerRuntimeRegistry`: maps durable `WorkerId` values to local
  worker endpoint and credentials for the synthetic runtime. Durable
  registration, capabilities, and grants still use existing
  control-plane worker use cases.
- `OutputBinder`: resolves minimal upstream result references into
  downstream payload fields.
- `SeededTiming`: derives deterministic `duration_ms`,
  `progress_interval_ms`, and synthetic branch codec from
  `(workflow_seed, node_id, branch_id)`.

The default CI workflow starts all Phase 6 fake providers, registers
them as synthetic workers, runs a scanner, expands a bounded set of
files, probes and scores each file, chooses a transform path, and then
runs downstream validation/reporting operations. Local stress mode uses
the same code path and seed model with larger bounds.

## 3. Workflow Behavior

`WorkflowExecutor::submit(plan)` opens one durable job with kind
`synthetic.workflow.<plan_id>`. Before creating durable rows, it
validates:

- unique node ids;
- dependency references;
- acyclic graph shape;
- executable node types;
- supported fixed `OperationKind` values;
- bounded fan-out limits;
- output binding references;
- timing caps.

Submit-time validation checks binding references structurally. It does
not imply tickets with unresolved upstream result bindings are created
immediately.

Root operation nodes become pending tickets with payload metadata that
includes `workflow_id`, `node_id`, `branch_id`, `operation`, and the
rendered operation payload. The executor calls
`mark_ready_if_unblocked` for root tickets. Submit-time ticket creation
is allowed only for nodes whose payloads are fully renderable at submit
time. Nodes with upstream result bindings are created by the staged
transition that makes those bindings available. Fan-out branch tickets
are delayed until scanner, probe, quality, or transform transitions as
described below.

Durable workflow tickets use a fixed contract so ready tickets can be
parsed back into executable workflow operations without caller-specific
conventions. Ticket kind is
`synthetic.workflow.operation.<operation_kind>`, where
`<operation_kind>` is the serialized `OperationKind` accepted by the
worker selector. Ticket payload is a JSON object with:

- `workflow_id`: durable workflow run id;
- `plan_id`: submitted plan id;
- `node_id`: workflow node id;
- `branch_id`: `root` for non-fan-out nodes, otherwise a stable branch
  id such as `file-000`;
- `operation`: serialized `OperationKind`, matching the ticket kind
  suffix;
- `rendered_payload`: worker request payload after static fields,
  bindings, fan-out aliases, and timing controls are applied;
- `timing`: effective `duration_ms` and `progress_interval_ms` values;
- `source_file`: optional scanner file object for branch-local tickets.

The executor parses this schema before worker selection. Unknown
workflow ticket kinds, missing required fields, operation mismatches
between kind and payload, or invalid rendered payloads are deterministic
executor errors. Retries preserve `workflow_id`, `plan_id`, `node_id`,
and `branch_id`. Attempt count is not stored in the workflow payload;
the executor reads it from the durable `Ticket` row after lease acquire
increments the row attempt state. Because workflow tickets carry fully
rendered worker payloads, the executor must not create a ticket until
all bindings needed for that ticket's `rendered_payload` are available.
Missing binding inputs are deterministic workflow failures when a
transition attempts to create dependent work.

The execution loop is a bounded in-flight scheduler owned by one
workflow executor. `max_in_flight_dispatches` caps the number of leased
workflow tickets with active worker dispatches. The default CI workflow
sets the cap high enough to observe overlapping branch leases; local
stress may raise it with the same single-executor ownership invariant.
The executor fills the in-flight pool one ticket at a time. For each
candidate ticket, selection must use either a freshly rebuilt
`WorkerView` capacity view or an executor-local reservation overlay that
adds already-started in-flight dispatches to each worker's durable
`active_leases`. After a worker is selected and the lease is acquired,
the executor immediately reserves one local capacity slot for that
worker until the dispatch releases or fails the lease. The executor must
not start a dispatch when the selected worker would exceed
`WorkerView.max_parallel`, even if `max_in_flight_dispatches` has room.

The executor repeatedly:

1. Lists ready tickets.
2. Parses each workflow ticket into its durable operation contract.
3. Builds `WorkerView` candidates from registered workers,
   capabilities, grants, and active lease counts.
4. Selects one worker with `SingleWorkerPerKindSelector` using the
   parsed operation kind and the capacity view plus local reservations.
5. Acquires a durable lease.
6. Starts a per-lease dispatch task when the in-flight cap has room.

Each in-flight dispatch task sends the operation to the process-backed
worker over `HttpClient`, consumes progress frames, heartbeats the lease
on progress and on a bounded timer while the stream remains active, then
releases the lease with the terminal result or fails it with a mapped
`FailureClass`. Completion frees one in-flight slot. Existing
dependency-promotion behavior unlocks dependents after the lease
transition. The executor keeps accepting ready tickets until no ready
work, in-flight work, or staged expansion work remains.
`HttpClient::dispatch` must return after the worker accepts the
operation and before terminal completion. The executor consumes progress
while the worker operation is still running; progress frames delivered
only after the operation has already completed do not satisfy the
workflow heartbeat stress requirement.

Worker selection happens before a lease exists. If selection fails, the
executor does not call lease-fail APIs. It records a workflow-owned
pre-lease ticket failure for the ready ticket:

- `NoEligibleWorker` maps to `FailureClass::NoEligibleWorker`;
- `AmbiguousWorkerSelection` maps to
  `FailureClass::AmbiguousWorkerSelection`.

Pre-lease selection failures do not create leases and do not count as
worker dispatches. They use a durable control-plane transition,
`record_pre_lease_ticket_failure`, because lease acquisition cannot
increment attempts on this path. The transition accepts a ready ticket
id, `FailureClass`, and timestamp. It requires the ticket to still be
`ready` and to have no active lease, increments the ticket attempt,
records durable retry/failure observability, then decides requeue versus
terminal failure. `NoEligibleWorker` uses the failure taxonomy retry
policy: if the incremented attempt remains below `max_attempts`, the
ticket is requeued with backoff; if the incremented attempt reaches
`max_attempts`, the ticket terminal-fails with
`FailureClass::NoEligibleWorker`. `AmbiguousWorkerSelection` is
operator-required and terminal-fails immediately in Phase 7.
`WorkflowRunSummary` counts pre-lease failures in retry and failure
counts, but excludes them from lease and dispatch counts.

The workflow succeeds when every created ticket reaches `succeeded` and
the job is marked `succeeded`. It fails when any ticket reaches
terminal `failed`, a payload binding fails, a pre-lease selection
failure reaches terminal failed state, the executor exceeds configured
limits, or a worker failure exhausts retries.

## 4. Fan-Out And Binding

The scanner returns a list of synthetic files. Phase 7 supports bounded
fan-out over that list. CI defaults to three files. Local stress runs
may raise the cap with the same deterministic seed path.

Each selected file creates a stable branch id such as `file-000`,
`file-001`, and `file-002`. Branch-local nodes depend on the relevant
branch parents, so probe, hash, identity, quality, transform, backup,
verify, external sync, issue, and use-lease operations are durable and
observable per branch.

Scanner expansion is idempotent within Phase 7's single-executor
workflow ownership model. Exactly one active workflow executor may drive
a given workflow run at a time. Phase 7 restart and replay tests reuse
that ownership model; concurrent multi-executor workflow claiming is out
of scope. Each branch-local ticket has stable identity
`(workflow_id, branch_id, node_id)`. Expansion uses lookup-before-create
before creating tickets or dependencies. The lookup key is durable
`job_id`, ticket kind, `payload.branch_id`, and `payload.node_id`; Phase
7 may implement this as a repository helper over existing ticket rows
and JSON payload rather than a new uniqueness constraint. The same
lookup rule applies to every branch transition. Binding or expansion
errors fail the workflow deterministically.

Branch work is created in staged derived transitions so each ticket can
carry a fully rendered payload:

1. Scanner completion creates only branch-root work known from the
   scanner result: probe, hash, and identity tickets and dependencies.
2. Probe completion creates the quality ticket after `probe.codec`
   exists.
3. Quality completion creates exactly one selected transform ticket
   after `needs_transcode` exists.
4. Transform completion creates backup, external sync, issue, and
   use-lease tickets after `transform.output_path` exists.
5. Backup completion creates the verify ticket after
   `backup.local_backup_id` exists.

Expansion completion is derived state for each transition:
scanner expansion is complete when every expected probe, hash, and
identity ticket and dependency exists for the scanner result; probe
expansion is complete when the branch quality ticket and dependency
exist; quality expansion is complete when the selected transform ticket
and dependency exist; transform expansion is complete when backup,
external sync, issue, and use-lease tickets and dependencies exist for
that branch; backup expansion is complete when the branch verify ticket
and dependency exist. If the executor crashes during any expansion
transition, rerunning the same derivation creates only missing tickets or
dependencies and converges without duplicates.

After each staged transition creates tickets and dependencies, the
executor calls `mark_ready_if_unblocked` for every newly created ticket.
This explicit promotion is required because the upstream blocker may
already have reached `succeeded` before the dependent ticket existed.
Promotion is idempotent: an empty promotion result is valid when a new
ticket still has unsatisfied dependencies.

The default transform decision is deterministic and local to each
branch. Probe results include a synthetic codec derived from
`(workflow_seed, branch_id)`. The CI default workflow seed is `2`, and
the default three-file codec fixture is `file-000 -> h265`,
`file-001 -> h264`, and `file-002 -> h265`; this pins at least one
remux branch and at least one transcode branch. Local stress runs
preserve deterministic seeded codec assignment while allowing larger
fan-out. The quality scorer terminal result includes `needs_transcode`,
derived only from its request payload as
`payload.codec != "h265"` for Phase 7. The workflow binds
`quality.path <- file.path` and `quality.codec <- probe.codec` before
dispatch. When `needs_transcode` is `true`, the branch creates a
`transcode` ticket; otherwise it creates a `remux` ticket. The
unselected transform path is not created and does not appear as skipped
durable work. Downstream bindings refer to the branch-local alias
`transform`, which resolves to the selected `transcode` or `remux`
result.

For the CI default, the scanner emits exactly three selected files and
the workflow creates one scanner ticket plus ten tickets per branch:
probe, hash, identity, quality, one selected transform, backup, verify,
external sync, issue, and use-lease. A branch is complete when all
created tickets for that branch succeed. The workflow completes only
after every branch and every non-branch workflow ticket has reached a
terminal successful state.

Output binding is intentionally minimal. A node payload starts with
static JSON fields, then applies bindings from upstream terminal
results. Supported references are object fields and array indexes, plus
branch-local aliases such as `file.path`. Examples:

- `probe.path <- file.path`
- `quality.path <- file.path`
- `quality.codec <- probe.codec`
- `transcode.path <- file.path`
- `remux.path <- file.path`
- `backup.path <- transform.output_path`
- `verify.path <- backup.local_backup_id`
- `external-sync.path <- transform.output_path`
- `issue.path <- transform.output_path`
- `use-lease.path <- transform.output_path`

The default workflow pins the static payload fields required by the
Phase 6 fake-provider validation contract. This table is normative for
the built-in workflow:

| Operation | Static default fields | Bindings |
|---|---|---|
| scanner | `path = "/library"` | none |
| probe | none | `path <- file.path` |
| hash | none | `path <- file.path` |
| identity | none | `path <- file.path` |
| quality | `profile = "default"` | `path <- file.path`, `codec <- probe.codec` |
| transcode | `target_codec = "h265"` | `path <- file.path` |
| remux | `container = "mkv"` | `path <- file.path` |
| backup | none | `path <- transform.output_path` |
| verify | none | `path <- backup.local_backup_id` |
| external sync | `system = "plex"`, `action = "refresh"` | `path <- transform.output_path` |
| issue | `reason = "quality_regression"` | `path <- transform.output_path` |
| use-lease | `holder = "manual"`, `reason = "playback"` | `path <- transform.output_path` |

Rendering order is fixed: start from the operation's static default
payload, apply bindings afterward, then append timing controls. Bindings
may overwrite only their configured target fields, which lets
branch-local file paths and upstream transform outputs replace intended
fields without losing required static provider fields. Timing controls
use the reserved `duration_ms`, `progress_interval_ms`, and
scanner-only `fan_out_count` fields.

Bindings are not expressions. There are no conditionals, loops,
filters, maps, or arbitrary script hooks in Phase 7. Missing fields are
deterministic execution errors and fail the workflow.

## 5. Seeded Timing And Worker Progress

Fake-provider payloads gain optional timing controls:

- `duration_ms`;
- `progress_interval_ms`;
- scanner-only `fan_out_count`.

Existing valid payloads remain valid without these fields, so Phase 6
conformance continues to pass unchanged.

The workflow executor derives timing values and branch codec values from
the workflow seed, node id, and branch id. The same seed must produce
byte-identical operation payloads, reproducible timings, and the same
branch codec choices. A different seed should change at least one
generated timing value or branch codec choice. CI uses short bounded
durations; local stress may use larger bounds.

Fake providers validate timing fields and cap duration and frame counts.
Timed fake-provider workflow runs must use a streaming-capable response
path: the worker sends `OperationResponse` promptly, then emits progress
frames with increasing `seq` over time, then emits exactly one terminal
result frame. The current buffered
`voom_worker_protocol::HttpServer` / `OperationDispatch { body: Vec<u8> }`
path may remain valid for Phase 6 baseline conformance and immediate
responses, but it is not sufficient for timed workflow stress because it
cannot prove progress is delivered while the operation is active. Phase
7 should add streaming support to `voom-worker-protocol::HttpServer` and
`OperationDispatch` rather than implementing separate raw HTTP shims for
the eleven fake providers. Live streaming is only for first execution:
completed idempotency replay may return a cached complete buffered
response, and duplicate requests while the first stream is active are
rejected deterministically instead of attaching to the active stream or
starting duplicate work. Progress cadence is derived from
`duration_ms` and `progress_interval_ms`. Happy-path providers must emit
progress before `progress_idle_deadline_ms`; silence is reserved for
chaos-worker tests.

The fake quality scorer adds `needs_transcode` to its terminal result.
It derives that field only from its operation request by setting
`needs_transcode` to `true` when request `payload.codec` is not `h265`.
Existing score fields remain present so Phase 6 conformance coverage
continues to validate the scorer's original result shape.

The fake transcoder and fake remuxer both emit `output_path` in their
terminal results. Transform-specific fields remain present:
transcoder keeps `target_codec`, and remuxer keeps `container`.
The shared `output_path` field makes `transform.output_path` safe for
downstream bindings regardless of which transform path was selected.

## 6. Failure Handling

Worker protocol errors fail the active lease. Invalid request or
payload errors map to non-retriable failure classes. Dispatch or
transport errors before a terminal result map to retriable worker
failure classes.

Worker terminal error frames fail the lease with the frame's
`FailureClass` and `ErrorCode`.

Missed heartbeat during active workflow execution is owned by the
executor watchdog. The executor heartbeats healthy streaming leases on
progress and on a bounded timer. If the watchdog observes that the
heartbeat deadline has passed before the lease TTL expires, it fails the
active lease with `FailureClass::WorkerTimeout`. Existing `expire_due`
behavior remains a crash-recovery safety net for abandoned leases after
the executor stops and keeps the current control-plane expiry
classification.

Because the executor owns ticket lease heartbeats, fake-worker behavior
alone cannot force an active missed heartbeat. Phase 7 adds a test-only
`WorkflowChaosOptions` executor configuration with one heartbeat fault:
suppress all executor heartbeats for one selected operation or node id,
including timer-triggered and progress-triggered heartbeats, while
leaving dispatch, process monitoring, and stream reading active. The
missed-heartbeat chaos case uses the chaos worker's `stall` mode: open
stream, no progress frames, no terminal frame, and no process exit. This
keeps the worker process alive and the stream open, avoids malformed
frames and direct `expire_due`, and lets the active watchdog fail the
lease as `FailureClass::WorkerTimeout`. The missed-heartbeat fixture
must configure deadlines so the heartbeat deadline expires before
`progress_idle_deadline_ms`; it must not rely on simultaneous watchdog
expiry to beat progress-timeout classification.

Progress timeout is detected by the executor stream watchdog when no
progress frame arrives within `progress_idle_deadline_ms`.

Malformed results are detected by `NdjsonReader` or protocol decode
errors and map to `FailureClass::MalformedWorkerResult`.

Worker crash is detected when the process exits or dispatch/stream read
fails. A known process death maps to `FailureClass::WorkerCrash`;
unreachable or no-response behavior maps to `FailureClass::WorkerTimeout`.

When multiple failure signals are possible, the executor applies this
precedence for the active lease:

1. Valid terminal success result releases the lease successfully.
2. A valid terminal result or error frame received before a later exit
   observation releases or fails the lease with the frame's result,
   `FailureClass`, and `ErrorCode`.
3. Malformed protocol or result frames fail as
   `FailureClass::MalformedWorkerResult`.
4. Known process exit fails as `FailureClass::WorkerCrash` only after
   the stream reader drains remaining bytes through EOF and no complete
   terminal frame is buffered.
5. Heartbeat deadline missed by the active executor watchdog fails as
   `FailureClass::WorkerTimeout`.
6. No progress frame within `progress_idle_deadline_ms` fails as
   `FailureClass::ProgressTimeout`.
7. Dispatch timeout, connection refusal, or no response before any
   worker stream is established fails as `FailureClass::WorkerTimeout`.

The heartbeat timer must run at an interval below the lease TTL, and
progress frames also trigger heartbeats. Chaos missed-heartbeat tests
exercise the active watchdog path, not direct `expire_due`
classification. Fake-worker chaos modes cover worker crash, malformed
result, dispatch timeout, and progress timeout inputs; missed heartbeat
is induced only by executor-side heartbeat suppression paired with the
no-progress `stall` mode. Progress-emitting modes such as
`non_converging_progress` and `deadline_exceeded` remain progress-timeout
inputs. Any direct `expire_due` test covers abandoned-lease crash
recovery and asserts existing expiry behavior. Chaos tests force one
signal at a time and assert the expected `FailureClass` from this table
rather than relying on wall-clock races. If heartbeat and progress
deadlines are both overdue when the watchdog evaluates the lease,
heartbeat precedence wins and the lease fails as
`FailureClass::WorkerTimeout`, but the missed-heartbeat fixture must set
its heartbeat deadline earlier than `progress_idle_deadline_ms` so the
classification is deterministic without that tie-breaker.

Retries use existing ticket `max_attempts`, lease fail/requeue
behavior, and dependency unlocking. The workflow fails after the
relevant ticket reaches terminal `failed`.

## 7. Interfaces

Public control-plane additions:

- `WorkflowPlan`
- `WorkflowNode`
- `OperationNode`
- `OutputBinding`
- `TimingPolicy`
- `FanOutPolicy`
- `ConcurrencyPolicy`
- `WorkflowExecutor`
- `WorkerRuntimeRegistry`
- `WorkflowChaosOptions`
- `WorkflowRunSummary`
- `record_pre_lease_ticket_failure`

`WorkflowRunSummary` includes job id, ticket counts, branch count,
operation counts, retry/failure counts, elapsed runtime, dispatch
count, peak active workflow leases, and per-operation throughput.

`ConcurrencyPolicy` carries `max_in_flight_dispatches`. The value must
be at least one and no greater than the configured fan-out/workflow
limit. A value of one is valid for targeted unit tests, but the default
CI workflow uses a value greater than one so it exercises overlapping
lease heartbeat behavior.

Per-worker capacity remains internal executor state. The executor
combines `WorkerView.active_leases`, `WorkerView.max_parallel`, and
local in-flight reservations before dispatch. No new public reservation
API is added in Phase 7.

`OperationDispatch` supports both the existing buffered NDJSON body path
and a streaming frame/body source. Buffered dispatch remains acceptable
for immediate baseline operations. Timed workflow operations must use
the streaming path so first execution returns the operation response
before terminal completion and delivers progress frames during the
operation. Idempotency replay may continue to return a cached complete
buffered response for completed operations.

Streaming operations record an in-flight idempotency entry before the
first live response is exposed. While that stream is active, a request
with the same idempotency key and the same request body must not start
another operation; Phase 7 returns `DuplicateIdempotencyKey` for this
case unless an existing protocol error already better represents
"operation still in progress". A request with the same key and a
different body remains a duplicate-key rejection. After terminal
completion, the same key and same body may replay the cached complete
buffered response.

The first implementation should keep these interfaces in
`voom-control-plane`. A separate `voom-workflow` crate is deferred until
another crate needs the workflow model outside control-plane.

## 8. Test Strategy

Unit tests:

- workflow-plan validation rejects duplicate ids, missing dependencies,
  cycles, unsupported executable node types, invalid fan-out caps, and
  invalid bindings;
- workflow ticket schema validation rejects missing required fields,
  unknown workflow ticket kinds, operation mismatches between kind and
  payload, and invalid rendered payloads, and sources attempt count from
  the durable ticket row rather than payload;
- output binding resolves object fields, array indexes, branch-local
  aliases, and missing-field failures;
- output binding renders path inputs from branch-local `file.path` and
  renders `quality.codec` from probe results;
- submit-time creation skips non-root nodes with unresolved upstream
  result bindings;
- executor refuses to create a ticket with unresolved bindings when a
  staged transition attempts to create dependent work;
- staged expansion calls `mark_ready_if_unblocked` for newly created
  tickets, promotes tickets whose blockers already succeeded, and leaves
  still-blocked tickets pending;
- transform selection is deterministic from quality request
  `payload.codec != "h265"`, creates exactly one transform ticket, and
  resolves the `transform` alias to the selected result;
- seeded branch codec assignment is reproducible for the same seed, can
  vary for a different seed, and maps default seed `2` to
  `file-000 -> h265`, `file-001 -> h264`, and `file-002 -> h265`;
- the CI default workflow creates at least one remux transform and at
  least one transcode transform;
- executor concurrency never exceeds `max_in_flight_dispatches`, frees a
  slot after lease release or failure, and permits overlapping dispatch
  tasks when multiple ready tickets exist;
- two ready tickets for the same operation with one eligible worker at
  `max_parallel = 1` produce only one active dispatch at a time;
- local per-worker reservations are released after lease success or
  failure so another ready ticket can later dispatch;
- `max_in_flight_dispatches > 1` does not override per-worker
  `WorkerView.max_parallel`;
- selector failures map to `FailureClass::NoEligibleWorker` and
  `FailureClass::AmbiguousWorkerSelection`;
- pre-lease failures increment durable ticket attempt, do not create
  leases, do not increment dispatch count, require a ready ticket with
  no active lease, and record durable retry/failure observability;
- pre-lease `NoEligibleWorker` requeues while attempts remain and
  terminal-fails when attempts are exhausted;
- `AmbiguousWorkerSelection` terminal-fails immediately;
- active missed-heartbeat watchdog failures map to
  `FailureClass::WorkerTimeout`;
- `WorkflowChaosOptions` suppresses timer-triggered and
  progress-triggered heartbeats for the selected operation or node and
  leaves non-selected operations heartbeating on progress and timer
  ticks;
- suppressed heartbeat with chaos-worker `stall` mode, no process exit,
  no progress-triggered heartbeat, and no malformed frame maps to
  `FailureClass::WorkerTimeout`;
- missed-heartbeat chaos configuration sets the heartbeat deadline
  earlier than `progress_idle_deadline_ms`, producing deterministic
  `FailureClass::WorkerTimeout` without relying on simultaneous watchdog
  expiry;
- progress idle alone maps to `FailureClass::ProgressTimeout`, while
  simultaneous heartbeat and progress deadline misses map to
  `FailureClass::WorkerTimeout`;
- process exit waits for stream drain before `FailureClass::WorkerCrash`,
  and terminal frames accepted before exit observation are not
  overridden;
- the default workflow renders payloads accepted by every current
  fake-provider validation contract;
- default workflow payload rendering starts with static provider
  defaults, applies bindings afterward, then appends timing controls;
- quality payloads include bound `codec` and static
  `profile = "default"`;
- transcode payloads include `target_codec = "h265"` and remux payloads
  include `container = "mkv"`;
- external sync, issue, and use-lease payloads include required static
  fields and bind `path` from `transform.output_path`;
- fake transcoder and fake remuxer both expose `output_path` in their
  terminal results;
- scanner expansion creates stable `(workflow_id, branch_id, node_id)`
  identities, finds existing tickets by durable `job_id`, ticket kind,
  `payload.branch_id`, and `payload.node_id`, creates only
  probe, hash, and identity tickets, and repeated expansion does not
  duplicate tickets or dependencies;
- probe completion creates quality after `probe.codec` exists;
- quality completion creates only the selected transform idempotently;
- transform completion creates only backup, external sync, issue, and
  use-lease work after `transform.output_path` exists and does not create
  verify before backup succeeds;
- backup completion creates verify after `backup.local_backup_id` exists;
- scanner, probe, quality, transform, and backup expansion completion
  are derived from existing ticket and dependency presence, not mutable
  expansion markers;
- seeded timing is reproducible for the same seed, changes for a
  different seed, and respects caps;
- fake-provider timing payloads emit multiple progress frames and one
  terminal result with monotonic `seq` values;
- timed fake-provider dispatch returns before terminal completion and
  observes at least two progress frames separated by elapsed wall-clock
  time before the terminal frame;
- same idempotency key and same body during an active timed stream
  returns a deterministic duplicate or in-progress rejection and does
  not invoke the handler a second time;
- same idempotency key and different body during an active timed stream
  returns duplicate-key rejection and does not invoke the handler a
  second time;
- same idempotency key and same body after terminal completion replays
  the cached complete buffered response;
- buffered immediate fake-provider responses remain valid for Phase 6
  baseline conformance;
- fake quality-scorer results include deterministic `needs_transcode`.

Integration tests:

- happy-path durable workflow starts fake workers, registers
  capabilities, submits the default three-file workflow, runs all
  branches, and asserts job success, ticket success, lease release,
  dependency promotion, three branch summaries, thirty-one total
  dispatches, and per-operation summary counts;
- the happy-path default workflow dispatches every fake provider with a
  valid rendered payload before asserting ticket, lease, dispatch, and
  summary counts;
- chaos workflow cases run through the same executor path and cover
  worker crash, dispatch timeout, watchdog-observed missed heartbeat,
  malformed result, and progress timeout with stable failure classes
  from the parent-aligned precedence table;
- missed-heartbeat chaos uses real executor dispatch, chaos-worker
  `stall` mode, and `WorkflowChaosOptions` heartbeat suppression; it
  asserts no progress-triggered heartbeat, no process exit, no malformed
  frame, no terminal frame, and no direct `expire_due` call is
  responsible for the `WorkerTimeout` failure; the fixture's configured
  heartbeat deadline precedes `progress_idle_deadline_ms`;
- benchmark workflow case reports scheduler throughput from the
  durable execution path and asserts non-zero throughput;
- stress workflow case uses seeded varied worker durations and asserts
  that peak active workflow leases is greater than one without exceeding
  `max_in_flight_dispatches`;
- stress workflow case asserts overlapping active leases occur only when
  worker capacity permits them and no worker exceeds configured
  parallelism;
- timed workflow branch heartbeats from progress delivered during the
  active worker stream, and progress-timeout coverage distinguishes no
  live progress from buffered progress delivered after completion;
- retrying a timed streaming worker request with the same idempotency
  key while the first stream is active cannot create a second worker
  operation or second active lease dispatch;
- retry cases preserve workflow, node, and branch identity across
  attempts while incrementing durable attempt state;
- late-created quality, transform, and downstream tickets progress to
  ready without another parent release event;
- repeated scanner expansion after a simulated restart does not create
  quality, transform, or downstream work early, fills missing
  branch-root work, preserves summary counts, and runs under the
  single-executor workflow ownership invariant;
- repeated probe expansion after a simulated restart does not duplicate
  quality tickets;
- repeated quality expansion after a simulated restart does not
  duplicate selected transform tickets or create downstream work early;
- repeated transform expansion after a simulated restart does not
  duplicate backup, external sync, issue, or use-lease tickets;
- repeated backup expansion after a simulated restart does not duplicate
  verify tickets;
- the default workflow dispatches the quality scorer with codec in its
  request payload;
- remux and transcode branches both bind backup input from
  `transform.output_path`;
- backup-created verify tickets become ready without another transform
  release event;
- the default workflow seed `2` deterministically exercises both remux
  and transcode paths;
- no-worker selection failure requeues or terminal-fails according to
  retry state;
- repeated no-worker selection eventually terminal-fails instead of
  looping forever;
- workflow terminal failure occurs only after pre-lease retry exhaustion
  or immediate operator-required failure;
- ambiguous-worker selection terminal-fails and appears in workflow
  summary failure counts;
- scheduler throughput excludes pre-lease selection failures from
  dispatch totals;
- direct `expire_due` coverage treats lease expiry as abandoned-lease
  crash recovery, not active missed-heartbeat classification.

Final verification is `just ci`.

## 9. Out Of Scope

- External JSON/TOML workflow plan parser.
- Policy compiler or high-level node expansion.
- Multi-worker scoring beyond `SingleWorkerPerKindSelector`.
- Real media tooling or external services.
- Production daemon loop.
- Concurrent multi-executor workflow claiming.
- Cancellation transport.
- Unbounded fan-out.
- Database uniqueness constraints for workflow branch ticket identity.
- True random timing in CI.
