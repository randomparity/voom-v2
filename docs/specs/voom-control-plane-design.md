# VOOM - Video Orchestration Operations Manager

## Purpose

This document specifies a from-first-principles architecture for a Rust-based
video library manager. The product manages video libraries through policy-driven
planning, durable job execution, out-of-process providers, remote nodes, and
agent-friendly interfaces. It supports CLI workflows, daemon operation, a web
interface, remote workers, plugin extensibility, and production-grade observability.

## Product Influences

TDarr demonstrates the value of distributed transcoding nodes, worker pools,
health checks, and operational controls around scan/watch behavior. Its model
shows that node scheduling and resource specialization must be early design
concerns, not later deployment details.

FileFlows demonstrates the power of reusable processing flows, branches, and
pluggable processing nodes. Its visual-flow model is flexible, but this design
chooses declarative policies and typed plans because they are easier to validate,
review, diff, test, and operate through CLI or agents.

Unmanic demonstrates a useful staged processing lifecycle: detect work, process
through worker plugins, use a cache, and then post-process. This design borrows
the lifecycle discipline and cache/staging safety, while replacing plugin-stack
execution with durable tickets and typed plans.

VOOM-legacy demonstrates the right product neighborhood: Rust, declarative media
policies, FFmpeg/MKVToolNix execution, SQLite state, CLI, web UI, and plugin
extensibility. This design deliberately avoids making an event bus the primary
work-routing mechanism. Work is routed through durable jobs and leases; events
record facts.

Reference URLs:

- TDarr: <https://docs.tdarr.io/docs/>
- FileFlows: <https://fileflows.com/docs>
- Unmanic: <https://docs.unmanic.app/docs/using_unmanic/getting_started/>
- VOOM: <https://github.com/randomparity/voom>

## Selected Architecture

Use a distributed control-plane-first architecture.

The control plane owns durable coordination. It stores policies, plans, jobs,
leases, nodes, artifacts, events, approvals, and audit history. It never directly
executes media operations. Every provider, including bundled providers, runs
out of process and speaks the same versioned worker protocol.

The core lifecycle is:

```text
Policy -> Plan DAG -> Durable Tickets -> Scheduler Leases -> Worker Results -> Host Commit
                                      |
                                      v
                              Append-only Events
```

Jobs and tickets are the source of execution truth. Events are append-only facts
for audit, metrics, UI updates, debugging, and optional reactive behavior. Events
do not claim, lease, or execute primary work.

This architecture is intentionally more explicit than a pure event bus. It keeps
CLI, daemon, API, web UI, local workers, remote nodes, synthetic workers, and
future plugins on one execution path.

## Design Principles

- Durable jobs route work; events record facts.
- All providers are out-of-process workers from day one.
- Built-in workers and third-party workers use the same protocol.
- Remote nodes are an early milestone, not a future migration.
- Synthetic providers are first-class contract clients and test infrastructure.
- Policies describe desired media outcomes.
- Scheduling policies describe operational preferences.
- Safety policies describe approval, backup, verification, and rollback rules.
- Artifact handles abstract over local paths, shared mounts, object stores, and
  staged files.
- The host owns final commit by default. Workers produce staged artifacts.
- The first executable milestone proves the control plane without real media
  tools.

## Non-Goals For The First Product Line

- A visual workflow engine as the primary policy model.
- Event-bus-based work claiming.
- In-process executor fast paths.
- Worker-specific special cases in the scheduler.
- Direct original-file mutation by default.
- Plugin-defined arbitrary untyped JSON operations in the first DSL.
- A mandatory external database for home deployments.

## Core Components

### Control Plane

The control plane is the durable source of truth. It owns:

- SQLite database and migrations.
- In-memory SQLite test mode using the same schema and repositories.
- Library configuration.
- Policy registry.
- Job queue and leases.
- Node registry.
- Artifact catalog.
- Event log.
- Approval and safety state.
- REST API.
- CLI command handlers.
- Web UI backend.

The control plane should expose clean storage boundaries for jobs, leases,
events, artifacts, policies, nodes, and library state. SQLite is the default
database, but the schema and transaction model should preserve a credible path
to Postgres if a future deployment profile needs it.

### Policy Compiler

The policy compiler parses declarative media policies into a validated phase
DAG. V1 uses a small fixed operation vocabulary:

- scan library
- probe file
- hash file
- back up file
- remux/containerize
- transcode video
- edit tracks
- extract audio
- transcribe audio
- verify artifact
- commit artifact
- delete artifact

Later plugin packages may register namespaced typed operation schemas, such as
`whisper.transcribe_audio` or `acme.detect_commercials`. The compiler validates
those operations against registered schemas. Extensibility must stay typed,
inspectable, and reject invalid policies before execution.

### Planner

The planner compares a stored `MediaSnapshot` to a compiled policy and emits:

- `ComplianceReport`: whether the file satisfies the desired state and why.
- `ExecutionPlan`: a full phase DAG of durable tickets.
- Resource estimates: CPU, GPU, disk, network, expected duration, and temporary
  storage.
- Artifact expectations: inputs, outputs, checksums when known, durability, and
  commit targets.
- Safety gates: backup required, approval required, verification required, and
  rollback behavior.

The planner builds the full DAG upfront so the scheduler can reason about
resources and future dependency unlocks. The system revalidates at phase
boundaries and supports bounded replanning when produced artifacts change
downstream assumptions.

### Scheduler

The scheduler leases ready tickets to workers. It considers:

- worker capabilities and grants
- node health and heartbeat freshness
- ticket priority
- dependency unlock order
- artifact locality
- storage and transfer cost
- measured throughput
- concurrency limits
- dynamic throttles
- scheduling windows
- safety requirements
- user policy overrides

The scheduler uses full-plan visibility for lookahead but does not permanently
bind every ticket to a node at plan creation. It leases dynamically at ticket
boundaries so nodes stay busy and failures can be handled without changing the
media policy.

### Artifact Resolver

Workers receive logical `ArtifactHandle`s, not raw assumptions about where bytes
live. The artifact resolver turns handles into access plans based on worker
capabilities and system policy.

An artifact handle includes:

- artifact identity
- media identity
- version
- size
- checksum when known
- privacy class
- durability class
- allowed access modes
- mutability
- source lineage

The resolver ranks viable placements using:

- same-node locality
- shared mount availability
- object-store availability
- measured throughput
- latency
- current congestion
- monetary cost
- egress cost
- storage class
- safety constraints
- user-defined limits

The fastest, closest, least expensive safe backing store should be selected.
User policy may override the default optimizer.

### Worker Runtime

Every provider runs as an out-of-process worker. The worker protocol is
network-capable from day one, even when workers run on the same machine.

Workers:

- register with the control plane
- advertise capabilities
- receive grants from the host
- heartbeat
- accept leases
- resolve artifact access plans
- stream structured logs and progress
- produce staged artifacts
- return typed results
- report failures with actionable categories

The control protocol should optimize for human and agent inspectability:
versioned HTTP plus JSON for commands and responses, with NDJSON or SSE for
progress streams. Large media bytes move through artifact handles, not through
the control protocol.

### Event Log

The event log records facts:

- library scan started
- file discovered
- file missing
- file modified
- probe completed
- policy evaluated
- plan created
- ticket ready
- ticket leased
- worker heartbeat missed
- artifact produced
- artifact verified
- commit completed
- job failed
- job completed

Events feed UI, metrics, audit, debugging, and optional reactive plugins. They
do not replace durable jobs or leases.

### Interfaces

All interfaces are clients of the same control plane.

The CLI must be agent-friendly:

- JSON input and output for all core commands.
- Dry-run mode.
- Plan inspection.
- Stable error codes.
- Machine-readable diagnostics.
- Human-readable table/plain modes where useful.

The daemon continuously monitors libraries, schedules jobs, manages remote
nodes, applies throttles, and recovers from crashes.

The web UI shows activity, queue state, library contents, compliance status,
plans, node health, provider capabilities, and library statistics over time.

## Worker Trust And Capability Grants

Workers advertise what they can do. The host grants what they are allowed to do.
The scheduler must use both.

Example:

```text
worker: basement-gpu-01
advertises:
  operations: probe_file, transcode_video, verify_artifact
  codecs: h264, hevc, av1
  hardware: nvidia_nvenc
  artifact_access: shared_mount, http

grants:
  can_execute: transcode_video, probe_file, verify_artifact
  can_access: library.movies.read, staging.local.write
  cannot_access: originals.write, backups.delete
  max_parallel:
    transcode_video: 2
    probe_file: 8
```

Original-file write access is never implicit. Default execution produces staged
artifacts. The host verifies and commits.

In-place mutation is exceptional. It requires:

- explicit worker grant
- backup first
- pre-mutation snapshot
- post-mutation snapshot
- audit event
- rollback metadata
- policy permission

## Policy Model

Media policy describes desired file outcomes. Scheduling policy describes
operational behavior. Safety policy describes approval, backup, verification,
and rollback requirements. Node policy describes what workers may access or
execute.

Example media policy shape:

```text
policy "english-x265-mkv" {
  phase containerize {
    container mkv
  }

  phase transcode {
    depends_on: [containerize]
    video codec hevc {
      encoder auto
      quality crf 20
    }
  }

  phase audio {
    depends_on: [transcode]
    keep audio where language == "eng" and not commentary
  }

  phase verify {
    depends_on: [audio]
    require quick_decode
  }
}
```

Example scheduling policy shape:

```text
schedule "home-library-default" {
  priority newest_first
  prefer local_gpu_for transcode_video
  copy_window "00:00-08:00"
  large_jobs night_only
  cloud_egress_budget "5 USD/day"
  pause_when node.health == degraded
}
```

The policy language will use a block-oriented text format that can be parsed,
formatted, diffed, and validated without executing worker code. Its required
property is that policies compile to a typed phase DAG with explicit operations,
dependencies, guards, inputs, outputs, and safety gates.

## Primary Workflow

A library change follows one common lifecycle:

```text
Library watcher or CLI scan
  -> ScanLibrary job
  -> ProbeFile / HashFile jobs
  -> MediaSnapshot stored
  -> EvaluatePolicy job
  -> ComplianceReport + ExecutionPlan
  -> tickets become ready as dependencies unlock
  -> scheduler leases tickets to workers
  -> workers produce staged artifacts and structured results
  -> host verifies, records events, and advances the plan
  -> host commits final artifact or records failure/rollback
```

For a multi-phase policy like "containerize to MKV, transcode to x265, strip
non-English audio, verify," the planner emits a full DAG upfront. The scheduler
uses that DAG for resource lookahead and dynamically leases ready work.

Phase boundaries are revalidation points. The system checks whether produced
artifacts still satisfy assumptions such as track IDs, codecs, duration, file
size, checksums, and health-check results. If assumptions changed, bounded
replanning updates downstream tickets while preserving the audit trail.

## Synthetic Provider Suite

Synthetic providers are first-class provider packages. They validate the
architecture before real media tools are introduced and remain part of the
ongoing test suite.

Required synthetic providers:

- `fake-scanner`: emits deterministic file discovery scenarios.
- `fake-prober`: returns canned media snapshots.
- `fake-transcoder`: simulates duration, progress, output size, codec changes,
  and failures.
- `fake-remuxer`: simulates container and track mutations.
- `fake-backup-store`: simulates local and object-store backup behavior.
- `fake-health-checker`: returns pass, fail, and degraded results.
- `fake-object-store`: simulates upload/download, egress cost, latency, and
  corruption.
- `fake-transcriber`: simulates transcript and subtitle generation.
- `chaos-worker`: crashes, stalls, corrupts output, misses heartbeats, returns
  malformed results, and exceeds deadlines.
- `benchmark-worker`: measures scheduler throughput without media tools.

These providers are not test doubles hidden inside unit tests. They are normal
workers that speak the real protocol and can be used by CLI, daemon, API, web
UI, integration tests, benchmarks, and demos.

## Data Storage

The default database is SQLite on disk. Tests use in-memory SQLite with the same
migrations and repository code.

Initial storage areas:

- `libraries`
- `library_roots`
- `media_files`
- `media_snapshots`
- `policies`
- `compiled_policies`
- `compliance_reports`
- `execution_plans`
- `tickets`
- `ticket_dependencies`
- `leases`
- `workers`
- `worker_capabilities`
- `worker_grants`
- `artifact_handles`
- `artifact_locations`
- `artifact_lineage`
- `events`
- `approvals`
- `backups`
- `scheduling_policies`
- `safety_policies`

The schema must support crash recovery, stale lease detection, event retention,
plan auditability, and idempotent ticket execution.

## Error Handling And Recovery

Errors should be classified at the boundary where they occur:

- policy parse error
- policy validation error
- missing capability
- no eligible worker
- artifact unavailable
- artifact checksum mismatch
- worker timeout
- worker crash
- malformed worker result
- verification failure
- backup failure
- commit failure
- approval required
- user cancellation

Every failure records an event and updates durable state. Retriable failures
remain attached to tickets with attempt count, backoff, and reason. Non-retriable
failures stop the affected plan branch and surface actionable diagnostics.

Stale leases are recovered by heartbeat timeout. Partially produced artifacts
are either promoted only after verification or marked abandoned and eligible for
cleanup. Host-owned commit ensures a worker crash does not leave the control
plane believing a final mutation succeeded.

## Observability

The product should expose:

- structured logs
- append-only event log
- job and ticket status
- worker health
- queue depth
- lease age
- retry counts
- throughput by operation type
- artifact transfer time and cost
- scheduling decisions and rejected candidates
- policy compliance trends
- library statistics over time

The web UI, CLI, and API should all be able to inspect why a ticket is waiting,
why a worker was selected, and why an artifact placement was chosen.

## Security And Safety

V1 security focuses on clear local and home-network boundaries:

- worker registration requires authentication
- worker grants are explicit
- artifact access is scoped
- original-file writes are denied by default
- remote artifact URLs are time-limited where supported
- destructive actions require policy permission
- approval gates are available for risky operations
- every mutation is audited

Future plugin distribution can add package signing, marketplace trust metadata,
and stricter sandboxing. The worker protocol should not require those features
to be useful.

## CLI MVP Requirements

The CLI MVP must support:

- initialize config and database
- register synthetic workers
- scan with synthetic provider
- evaluate a policy
- show compliance report
- create an execution plan
- inspect plan JSON
- run plan with synthetic providers
- show events
- show workers and capabilities
- show jobs, tickets, leases, and artifacts
- emit JSON for every command

The CLI must be suitable for agent use: deterministic output, stable schemas,
dry-run mode, plan-only mode, and machine-readable errors.

## Daemon MVP Requirements

The daemon MVP must support:

- continuous library monitoring
- file stability/debounce rules
- scan reconciliation
- background scheduling
- remote worker heartbeats
- stale lease recovery
- dynamic throttles
- scheduled copy windows
- crash recovery
- event streaming for UI/API clients

## Web UI MVP Requirements

The web UI MVP must show:

- current activity
- queue and ticket state
- plan details
- policy compliance status
- library contents
- file detail with media snapshot
- worker/node health
- provider capabilities
- artifact locations
- recent events
- library statistics over time

The web UI is an operational console, not the architectural source of truth.
Everything it does should be possible through CLI/API.

## Sprint Roadmap

Use two-week sprints. Each sprint should prove an architectural promise and
leave behind automated tests.

### Sprint 0: Spec And Skeleton

Goal: create the Rust workspace and engineering guardrails.

Deliverables:

- Rust workspace.
- Core crate boundaries.
- SQLite migration runner.
- In-memory SQLite test harness.
- CLI shell with JSON output mode.
- Initial REST API skeleton.
- Quality gates: format, lint, type/build, tests.
- Architecture decision records for job/event split and out-of-process workers.

Exit criteria:

- Empty app starts.
- Database initializes on disk and in memory.
- CLI can print version and health JSON.
- CI-equivalent local checks pass.

### Sprint 1: Durable Control Plane MVP

Goal: implement core durable state without media tooling.

Deliverables:

- job and ticket tables
- leases with stale lease recovery
- node and worker registry
- artifact catalog
- append-only event log
- repository interfaces
- migration tests
- JSON CLI for inspecting jobs, leases, nodes, artifacts, and events

Exit criteria:

- Tests can create jobs, lease tickets, expire leases, and recover work.
- Events are recorded for all state transitions.
- In-memory SQLite tests exercise the same repositories as disk mode.

### Sprint 2: Synthetic Provider Suite MVP

Goal: prove the worker protocol and scheduler with fake providers.

Deliverables:

- versioned HTTP/JSON worker protocol
- local worker supervisor
- fake scanner
- fake prober
- fake transcoder
- fake remuxer
- fake backup store
- fake health checker
- chaos worker
- benchmark worker
- structured progress stream
- provider conformance tests

Exit criteria:

- A synthetic end-to-end plan runs through the real scheduler.
- Chaos tests cover worker crash, timeout, malformed result, and missed heartbeat.
- Benchmark worker reports scheduler throughput.

### Sprint 3: Policy DAG MVP

Goal: implement core policy-to-plan behavior.

Deliverables:

- core media policy grammar
- parser and validator
- compiled policy model
- media snapshot model
- compliance report
- plan DAG generation
- phase dependency handling
- plan dry-run and JSON inspection
- scheduling priority model

Exit criteria:

- Synthetic media snapshots can be evaluated against policies.
- Non-compliant files produce deterministic execution plans.
- Multi-phase policy plans execute with synthetic providers.

### Sprint 4: Remote Node MVP

Goal: make remote workers a real early deployment shape.

Deliverables:

- authenticated worker registration
- network worker leases
- heartbeat and health model
- remote synthetic workers
- artifact handle access plans
- locality/cost scoring
- node-level concurrency limits
- remote-node integration tests

Exit criteria:

- A remote synthetic worker can execute leased tickets.
- Scheduler chooses workers using capability, health, locality, and cost.
- Lost remote nodes trigger stale lease recovery.

### Sprint 5: CLI Media MVP

Goal: add the first real media path while preserving the provider contract.

Deliverables:

- ffprobe worker
- FFmpeg worker for one transcode path
- MKVToolNix worker for one remux/track-edit path
- backup worker
- verification worker
- staged artifact commit
- CLI scan/evaluate/plan/run commands
- JSON reports

Exit criteria:

- CLI can scan a real library path, evaluate policy compliance, create a plan,
  and execute a simple staged media change.
- No real media worker bypasses the out-of-process protocol.

### Sprint 6: Daemon MVP

Goal: run continuously and manage changing libraries.

Deliverables:

- filesystem watcher
- file stability rules
- scan sessions and reconciliation
- background scheduler loop
- scheduling windows
- dynamic throttles
- recovery on restart
- daemon status API

Exit criteria:

- Adding, modifying, and removing files produces correct durable state changes.
- The daemon recovers from restart without losing queued work.
- Scheduling windows affect ticket leasing without changing media policies.

### Sprint 7: Web UI MVP

Goal: provide a usable operational console.

Deliverables:

- activity dashboard
- queue and ticket views
- plan detail view
- library browser
- file detail view
- worker/node health view
- capability view
- event stream
- basic library statistics over time

Exit criteria:

- A user can understand what is running, waiting, failed, and why.
- UI actions use the same API as CLI/daemon workflows.

### Sprint 8: Plugin SDK And Extensible Operations

Goal: make third-party providers practical.

Deliverables:

- plugin package layout
- provider manifest
- operation schema registration
- result schema registration
- SDK examples
- conformance test runner
- compatibility/version checks
- documentation for provider authors

Exit criteria:

- A sample third-party provider registers a namespaced operation schema.
- The policy compiler validates the plugin-defined operation.
- The conformance suite verifies provider behavior.

### Sprint 9: Safety And Observability Hardening

Goal: make failure modes visible and recoverable.

Deliverables:

- approval gates
- backup policies
- rollback flows
- richer verification policies
- chaos test suite
- metrics endpoint
- trace IDs across plan, ticket, worker, artifact, and event records
- scheduler decision logs
- artifact cleanup

Exit criteria:

- Common destructive operations can require approval.
- Chaos tests are part of the regular verification suite.
- Operators can inspect why work was routed, paused, retried, or failed.

### Sprint 10: Production Readiness

Goal: prepare for real use and release.

Deliverables:

- installation packaging
- upgrade and migration tests
- security review
- sample policies
- user documentation
- provider author documentation
- benchmark gates
- release process
- backup/restore documentation

Exit criteria:

- A fresh user can install, configure, scan, plan, execute, monitor, and recover.
- Migrations are tested across released schema versions.
- Release artifacts and docs are ready for production users.

## Intermediate Milestones

- Control Plane MVP: Sprint 1 complete.
- Synthetic Worker MVP: Sprint 2 complete.
- Policy/CLI MVP: Sprint 3 complete.
- Remote Node MVP: Sprint 4 complete.
- Real Media CLI MVP: Sprint 5 complete.
- Daemon MVP: Sprint 6 complete.
- Web UI MVP: Sprint 7 complete.
- Extensible Plugin MVP: Sprint 8 complete.
- Production Candidate: Sprint 10 complete.

## Spec Review Notes

This spec intentionally keeps exact Rust crate names, DSL grammar details, API
schemas, and database column definitions for the implementation plan. The design
decisions fixed here are the architectural boundaries: durable jobs over
work-events, out-of-process workers, early remote nodes, artifact handles with
cost-aware placement, synthetic providers as the first test spine, and separate
media/scheduling/safety/node policies.
