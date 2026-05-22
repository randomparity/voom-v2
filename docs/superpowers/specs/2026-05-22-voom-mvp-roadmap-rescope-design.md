---
name: voom-mvp-roadmap-rescope-design
description: Rescope the VOOM MVP roadmap from ten oversized sprints into smaller delivery sprints with consistent acceptance gates and milestone bands.
status: proposed
date: 2026-05-22
branch: feat/sprint-2
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-2-closeout-acceptance-plan.md
---

# VOOM MVP Roadmap Rescope

## 1. Purpose

The original architectural roadmap in
`docs/specs/voom-control-plane-design.md` describes the MVP as ten
two-week sprints. Sprint 2 showed that those sprint containers are too
large: foundational infrastructure, worker behavior, conformance,
durable workflow proof, chaos coverage, and benchmark reporting all
landed under one sprint label.

This rescope keeps the original MVP destination but splits future work
into smaller delivery sprints. Each sprint should prove one architectural
promise, leave behind automated verification, and close with explicit
documentation.

## 2. Roadmap Rules

Every future sprint in the architectural spec should use the same shape:

- Goal: one sentence describing the architectural promise.
- Deliverables: three to seven concrete artifacts.
- Explicitly out of scope: named deferrals to later sprints.
- Acceptance criteria: externally visible behavior or durable evidence.
- Verification expectations: command families and evidence categories.
- Closeout documentation: the spec or acceptance matrix that records
  release readiness.

Future sprint overview entries may name command families before code
exists, but each sprint's own design or closeout spec must finalize the
exact commands before implementation begins.

Sprint boundaries are not feature buckets. A sprint is complete only
when its acceptance evidence is present and repeatable.

## 3. Historical Baseline

The completed and current sprint history remains intact:

- Sprint 0: workspace skeleton and engineering guardrails.
- Sprint 1: durable control-plane state.
- Sprint 2: synthetic worker protocol, fake providers, conformance, and
  durable workflow closeout.

The roadmap update should not rewrite those histories. It may add notes
that Sprint 2 was delivered as seven documented phases and that future
roadmap items are smaller by design.

## 4. Revised MVP Sprint Map

### Foundation

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 0 | Workspace skeleton and engineering guardrails. | Empty app initializes, CLI health/version JSON works, and local CI-equivalent checks pass. |
| Sprint 1 | Durable control-plane state. | Jobs, tickets, leases, repositories, events, identity records, artifacts, bundles, issues, quality scores, and use leases are durable and tested. |
| Sprint 2 | Synthetic worker protocol and durable workflow closeout. | Protocol, fake providers, conformance, chaos, benchmark reporting, and `WorkflowExecutor` closeout pass the Sprint 2 acceptance matrix. |

Sprint 0 through Sprint 2 inherit their detailed deliverables,
verification commands, and closeout documentation from existing Sprint
0, Sprint 1, Sprint 2, and Sprint 2 closeout specs. The future roadmap
below uses the normalized shape expected for Sprint 3 onward.

### Policy And Planning

#### Sprint 3: Policy Domain Model And Snapshot Inputs

- Goal: define policy-domain data structures before introducing a
  parser.
- Deliverables: media snapshot input model, identity-evidence inputs,
  bundle target model, quality profile selection model, issue input
  model, and deterministic fixtures for compliant and noncompliant
  synthetic media.
- Explicitly out of scope: policy text grammar, plan DAG generation,
  CLI plan commands, and synthetic execution.
- Acceptance focus: fixtures can express the policy inputs required by
  the original CLI/Web/daemon MVP requirements without depending on a
  parser.
- Verification expectations: policy-model unit tests, fixture
  round-trip tests, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 3 design and acceptance matrix mapping
  each original policy input requirement to a model or fixture.

#### Sprint 4: Policy Parser, Validator, And Compiled Model

- Goal: compile policy text into the Sprint 3 domain model with stable
  diagnostics.
- Deliverables: core media policy grammar, parser, validator, compiled
  policy model, golden valid-policy fixtures, golden invalid-policy
  diagnostics, and schema/version notes for policy files.
- Explicitly out of scope: plan DAG generation, scheduling priority
  execution, CLI run commands, plugin-defined operations, and UI
  editing.
- Acceptance focus: valid policies compile deterministically and invalid
  policies produce stable machine-readable errors.
- Verification expectations: parser/validator unit tests, golden
  diagnostic tests, fixture compatibility tests, documentation
  placeholder scan, and `just ci`.
- Closeout documentation: Sprint 4 design and acceptance matrix covering
  grammar scope, diagnostics, and compatibility rules.

#### Sprint 5: Plan DAG Generation And Dry-Run CLI

- Goal: turn compiled policy results into inspectable execution plans.
- Deliverables: plan DAG generation, phase dependency handling,
  scheduling priority metadata, dry-run / plan-only CLI JSON
  inspection command, stable plan schema fixtures, deterministic
  machine-readable CLI errors, and event/state inspection needed to
  debug plan creation.
- Explicitly out of scope: executing plans, compliance report UI,
  real-media providers, daemon scheduling, and remote workers.
- Acceptance focus: noncompliant synthetic inputs produce deterministic
  plan DAG JSON that agent workflows can inspect without running work.
- Verification expectations: plan-generation tests, CLI golden-output
  tests for dry-run and plan-only modes, schema fixture tests,
  documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 5 design and acceptance matrix tying
  CLI plan behavior to stable JSON schemas and error envelopes.

#### Sprint 6: Compliance Reports And Synthetic Policy Execution

- Goal: close the policy milestone by reporting and running synthetic
  policy plans.
- Deliverables: compliance report model, issue creation for
  noncompliance, synthetic plan execution command, compliance report CLI,
  event/state inspection for executed plans, report JSON fixtures, and
  synthetic execution through the Sprint 2 worker path.
- Explicitly out of scope: real media workers, remote execution,
  daemon loops, Web UI reporting, and plugin-defined policy operations.
- Acceptance focus: reports explain compliance/noncompliance, durable
  issues are created, and synthetic policy plans execute through
  `WorkflowExecutor`.
- Verification expectations: report unit tests, issue-creation tests,
  synthetic execution integration tests, CLI golden-output tests,
  documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 6 closeout matrix for policy/report
  behavior, CLI JSON contracts, issue creation, and synthetic execution.

### Remote Scheduling

#### Sprint 7: Node Registry And Authenticated Registration

- Goal: make remote-capable workers and nodes durable, authenticated
  entities.
- Deliverables: `nodes` registry, worker-to-node relationship, node
  heartbeat state, authenticated worker registration, node/worker
  inspection commands, and registration audit events.
- Explicitly out of scope: remote ticket leasing, artifact access plans,
  scheduler scoring, TLS production hardening, and real media workers.
- Acceptance focus: nodes and remote-capable workers register durably
  with inspectable identity and health state.
- Verification expectations: migration/repository tests, registration
  integration tests, CLI/API inspection golden tests, documentation
  placeholder scan, and `just ci`.
- Closeout documentation: Sprint 7 design and acceptance matrix covering
  node identity, registration, heartbeat state, and inspection surfaces.

#### Sprint 8: Remote Leases, Heartbeats, Recovery, And Artifact Access Plans

- Goal: execute synthetic work remotely with enough artifact-access
  planning to make remote execution explicit.
- Deliverables: remote worker lease acquisition, remote heartbeat path,
  stale remote lease recovery, remote synthetic worker integration tests,
  artifact handle access plan model, and synthetic artifact-access
  fixtures for remote inputs/outputs.
- Explicitly out of scope: scheduler locality/cost scoring, real remote
  artifact transfer, production object storage, and media workers.
- Acceptance focus: remote synthetic workers execute leased tickets,
  lost workers/nodes recover cleanly, and each remote dispatch records
  how the worker is expected to access artifacts.
- Verification expectations: remote lease integration tests, stale
  recovery tests, artifact-access fixture tests, documentation
  placeholder scan, and `just ci`.
- Closeout documentation: Sprint 8 closeout matrix for remote lease
  lifecycle, recovery, and artifact access planning.

#### Sprint 9: Scheduler Scoring

- Goal: make scheduling decisions explainable across capability, health,
  locality, cost, and concurrency.
- Deliverables: scheduler scoring model, node-level concurrency limits,
  worker-level concurrency limits, locality/cost scoring using artifact
  access plans, decision logs, deterministic scoring fixtures, and
  remote-node scheduler integration tests.
- Explicitly out of scope: real media execution, daemon scheduling
  windows, production metrics endpoint, and UI scheduler controls.
- Acceptance focus: scheduler choices are deterministic under fixtures,
  explainable to operators, and respect node/worker concurrency limits.
- Verification expectations: scoring unit tests, concurrency integration
  tests, scheduler decision-log fixture tests, documentation placeholder
  scan, and `just ci`.
- Closeout documentation: Sprint 9 acceptance matrix tying each scoring
  factor to fixtures, logs, and scheduler behavior.

### Real Media CLI

#### Sprint 10: Real Ingest And FFprobe Worker

- Goal: introduce the first real media input path while preserving the
  provider boundary.
- Deliverables: real library scan command, hashing/location ingest,
  `ffprobe` worker, media snapshot persistence, file asset/version
  updates, and scan/report CLI JSON fixtures.
- Explicitly out of scope: staged artifact mutation, transcoding,
  remuxing, backup, daemon watching, and remote media transfer.
- Acceptance focus: CLI scan creates file assets, versions, locations,
  hashes, and media snapshots through an out-of-process provider.
- Verification expectations: ingest tests, `ffprobe` worker conformance
  tests, CLI golden-output tests, small fixture-media integration tests,
  documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 10 closeout matrix for scan, ingest,
  snapshot, and provider-boundary behavior.

#### Sprint 11: Staged Artifact Commit And Verification Worker

- Goal: make media mutations recoverable before adding mutation workers.
- Deliverables: staged artifact commit flow, verification worker,
  commit audit events, rollback/recovery tests, artifact verification
  report, and CLI inspection for staged/committed artifacts.
- Explicitly out of scope: transcode/remux workers, backup policy,
  daemon cleanup, and production rollback UX.
- Acceptance focus: generated artifacts are staged, verified, committed,
  audited, and recoverable on failure.
- Verification expectations: staged-commit tests, verification worker
  conformance tests, rollback/recovery tests, CLI golden-output tests,
  documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 11 acceptance matrix for artifact
  staging, verification, commit, audit, and recovery.

#### Sprint 12: FFmpeg Transcode Worker

- Goal: run one policy-driven transcode path through the durable media
  mutation flow.
- Deliverables: FFmpeg worker for one transcode path, transcode payload
  schema, progress mapping, output verification hook, staged commit
  integration, and CLI execution/report fixtures.
- Explicitly out of scope: multiple codec ladders, remux/track editing,
  backup, daemon scheduling, and UI media controls.
- Acceptance focus: one transcode plan runs through the provider
  protocol and staged artifact commit flow.
- Verification expectations: worker conformance tests, transcode
  fixture-media integration tests, staged-commit integration tests, CLI
  golden-output tests, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 12 closeout matrix for transcode
  execution, progress, verification, and commit behavior.

#### Sprint 13: MKVToolNix Remux / Track-Edit Worker

- Goal: run one remux or track-edit path through the durable media
  mutation flow.
- Deliverables: MKVToolNix worker, remux/track-edit payload schema,
  progress mapping, output verification hook, staged commit integration,
  and CLI execution/report fixtures.
- Explicitly out of scope: broad container editing, backup, sidecar
  ingest, daemon scheduling, and UI media controls.
- Acceptance focus: one remux or track-edit plan runs through the
  provider protocol and staged artifact commit flow.
- Verification expectations: worker conformance tests, remux fixture
  integration tests, staged-commit integration tests, CLI golden-output
  tests, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 13 closeout matrix for remux/track-edit
  execution, progress, verification, and commit behavior.

#### Sprint 14: Backup, Sidecar Ingest, And Full Real-Media CLI Workflow

- Goal: complete the real-media CLI milestone without introducing
  daemon or UI ownership.
- Deliverables: backup worker, sidecar asset ingest for one generated or
  external asset type, full scan/evaluate/plan/run/report workflow for
  real media, bundle/sidecar CLI views, backup report, and real-media
  workflow fixtures.
- Explicitly out of scope: filesystem watcher, background daemon loop,
  Web UI, plugin SDK, and production packaging.
- Acceptance focus: CLI can scan, evaluate, plan, execute, inspect
  reports, show bundles/sidecars, and preserve the out-of-process
  provider boundary for real media.
- Verification expectations: full CLI workflow integration tests,
  backup worker conformance tests, sidecar ingest tests, CLI golden
  tests, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 14 real-media CLI closeout matrix.

### Daemon

#### Sprint 15: Watcher, Stability Rules, Scan Sessions, And Reconciliation

- Goal: turn one-shot scan behavior into continuous durable library
  observation.
- Deliverables: filesystem watcher, file stability/debounce rules, scan
  sessions, reconciliation for adds/modifications/removals/renames, and
  daemon status API for scan activity.
- Explicitly out of scope: background work scheduling, dynamic
  throttles, external sync loops, and UI event streaming.
- Acceptance focus: library changes produce correct durable state after
  stability windows.
- Verification expectations: watcher fixture tests, reconciliation
  integration tests, daemon status tests, documentation placeholder
  scan, and `just ci`.
- Closeout documentation: Sprint 15 closeout matrix for watch,
  debounce, session, and reconciliation behavior.

#### Sprint 16: Background Scheduler Loop, Windows, And Throttles

- Goal: run queued work continuously under operator scheduling
  constraints.
- Deliverables: background scheduler loop, scheduling windows, dynamic
  throttles, lease-loop observability, daemon control/status surfaces,
  and restart-safe loop state.
- Explicitly out of scope: issue lifecycle loops, external sync loops,
  use lease cleanup loop, Web UI controls, and production metrics.
- Acceptance focus: queued work runs continuously and windows/throttles
  affect leasing without changing policy results.
- Verification expectations: scheduler-loop integration tests, window
  and throttle tests, restart tests, documentation placeholder scan, and
  `just ci`.
- Closeout documentation: Sprint 16 closeout matrix for daemon
  scheduling, throttles, and restart-safe leasing.

#### Sprint 17: Daemon Recovery And Lifecycle Loops

- Goal: close the daemon MVP with recovery and lifecycle maintenance.
- Deliverables: issue lifecycle update loop, external-system health/sync
  loop, runtime use lease cleanup loop, stale recovery loop, restart
  recovery tests, and event streaming for API/UI clients.
- Explicitly out of scope: Web UI implementation, plugin SDK, approval
  gates, and production observability dashboards.
- Acceptance focus: the daemon recovers from restart, updates issues,
  cleans stale use leases, and runs external-system health/sync jobs.
- Verification expectations: lifecycle-loop integration tests, restart
  recovery tests, event-stream tests, documentation placeholder scan, and
  `just ci`.
- Closeout documentation: Sprint 17 daemon MVP closeout matrix.

### Web UI

#### Sprint 18: Read-Only Operations Console

- Goal: make operational state visible without adding UI mutations.
- Deliverables: activity dashboard, queue/ticket views, job/lease views,
  worker/node health views, provider capability views, recent events,
  and API-backed loading/error states.
- Explicitly out of scope: UI actions, live streaming, library detail
  depth, policy reporting depth, and plugin management.
- Acceptance focus: operators can see what is running, waiting, failed,
  and why.
- Verification expectations: API route tests, UI component/route tests,
  accessibility smoke checks, documentation placeholder scan, and
  `just ci`.
- Closeout documentation: Sprint 18 UI operations-console closeout
  matrix.

#### Sprint 19: Library And File Detail Views

- Goal: expose durable media identity and artifact history in the UI.
- Deliverables: library browser, file detail view, media snapshot view,
  file asset/version/location history, identity evidence timeline,
  media work/variant views, and bundle/sidecar view.
- Explicitly out of scope: UI mutation flows, live streaming, policy
  report dashboards, and plugin management.
- Acceptance focus: users can inspect a file asset's versions,
  locations, evidence, media work, variants, bundles, and sidecars.
- Verification expectations: API route tests, UI route/component tests,
  fixture data tests, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 19 UI library/file closeout matrix.

#### Sprint 20: Policy And Reporting Views

- Goal: expose policy and operational reports without adding action
  workflows.
- Deliverables: compliance report view, issue board, quality score and
  retention views, external-system sync/path mapping views, active use
  lease indicators, and blocked-operation reports.
- Explicitly out of scope: UI actions, live event streaming, plugin
  management, and production analytics dashboards.
- Acceptance focus: users can inspect compliance, issues, quality,
  external sync, retention, use leases, and blocked operations.
- Verification expectations: API route tests, UI route/component tests,
  report fixture tests, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 20 UI reporting closeout matrix.

#### Sprint 21: UI Actions And Live Event Streaming

- Goal: complete the Web UI MVP with actions and live updates.
- Deliverables: UI action flows backed by existing APIs, live event
  stream integration, action audit visibility, optimistic-state rollback
  behavior for failed actions, and end-to-end UI workflow tests.
- Explicitly out of scope: new backend workflow semantics, plugin
  marketplace UI, production packaging, and broad role-based access
  control.
- Acceptance focus: UI actions use the same API as CLI/daemon workflows
  and live state updates are delivered through the event stream.
- Verification expectations: API route tests, UI end-to-end tests,
  event-stream tests, failed-action tests, documentation placeholder
  scan, and `just ci`.
- Closeout documentation: Sprint 21 Web UI MVP closeout matrix.

### Plugin SDK

#### Sprint 22: Plugin Manifest, Layout, And Schema Registration

- Goal: define the minimum stable plugin package and schema model.
- Deliverables: plugin package layout, provider manifest, operation
  schema registration, result schema registration, compatibility fields,
  policy-compiler references to registered schemas, and schema fixture
  tests.
- Explicitly out of scope: SDK examples, distribution/marketplace,
  package signing, and UI plugin management.
- Acceptance focus: namespaced operation and result schemas can be
  registered, validated, and referenced by the policy compiler.
- Verification expectations: manifest parser tests, schema validation
  tests, policy-compiler integration tests, documentation placeholder
  scan, and `just ci`.
- Closeout documentation: Sprint 22 plugin schema closeout matrix.

#### Sprint 23: Provider SDK Examples And Conformance Runner

- Goal: make third-party provider development testable.
- Deliverables: SDK examples, identity provider example,
  external-system provider example, quality scorer example, conformance
  runner for plugin providers, and provider author quickstart.
- Explicitly out of scope: compatibility enforcement, marketplace
  distribution, package signing, and production install flows.
- Acceptance focus: example providers use the SDK and pass the public
  conformance suite.
- Verification expectations: SDK example tests, plugin conformance
  tests, documentation example checks, documentation placeholder scan,
  and `just ci`.
- Closeout documentation: Sprint 23 provider SDK closeout matrix.

#### Sprint 24: Compatibility, Docs, And Sample Third-Party Provider

- Goal: close the plugin MVP with compatibility and installable example
  behavior.
- Deliverables: compatibility/version checks, sample third-party
  provider, provider-author documentation, install/validate workflow,
  compatibility error fixtures, and plugin upgrade notes.
- Explicitly out of scope: public marketplace, package signing policy,
  production sandboxing, and UI plugin management.
- Acceptance focus: version compatibility is enforced and a sample
  third-party provider can be installed, validated, and documented.
- Verification expectations: compatibility tests, sample provider
  integration tests, install/validate workflow tests, documentation
  placeholder scan, and `just ci`.
- Closeout documentation: Sprint 24 Plugin SDK MVP closeout matrix.

### Hardening And Release

#### Sprint 25: Safety Gates

- Goal: make destructive or expensive operations explicitly
  controllable.
- Deliverables: approval gates, rollback flows, backup policies,
  destructive-operation controls, richer verification policies, safety
  audit events, and safety report fixtures.
- Explicitly out of scope: production packaging, broad observability
  dashboards, public security review, and marketplace trust metadata.
- Acceptance focus: common destructive operations can require approval
  and can be rolled back or explained from durable evidence.
- Verification expectations: approval-gate tests, rollback tests,
  backup-policy tests, safety report tests, documentation placeholder
  scan, and `just ci`.
- Closeout documentation: Sprint 25 safety closeout matrix.

#### Sprint 26: Observability

- Goal: make routing, failure, and lifecycle behavior inspectable.
- Deliverables: metrics endpoint, trace ID propagation across
  plan/ticket/worker/artifact/event records, scheduler decision logs,
  lifecycle report suite covering identity evidence, variant retention,
  issues, external sync, and use lease blocking, failure/routing
  explanation outputs, and observability fixture tests.
- Explicitly out of scope: release packaging, security review, plugin
  marketplace observability, and production SLO policy.
- Acceptance focus: operators can inspect why work was routed, paused,
  retried, blocked, failed, or cleaned up.
- Verification expectations: metrics tests, trace propagation tests,
  report fixture tests, scheduler decision-log tests, documentation
  placeholder scan, and `just ci`.
- Closeout documentation: Sprint 26 observability closeout matrix.

#### Sprint 27: Production Readiness

- Goal: prepare VOOM for a production candidate release.
- Deliverables: installation packaging, upgrade and migration test
  suite, security review, sample policies, release documentation set
  covering user docs, provider-author docs, release process, and
  backup/restore, benchmark gates, and release-candidate checklist.
- Explicitly out of scope: new MVP feature areas, major schema
  redesigns, new worker classes, and new UI product surfaces.
- Acceptance focus: a fresh user can install, configure, scan, plan,
  execute, monitor, recover, and upgrade using released artifacts and
  documentation.
- Verification expectations: package/install tests, migration upgrade
  tests, benchmark gate runs, documentation checks, release-process dry
  run, documentation placeholder scan, and `just ci`.
- Closeout documentation: Sprint 27 production readiness checklist and
  release-candidate acceptance matrix.

## 5. Migration Strategy

The architectural spec should replace the existing ten-sprint roadmap
with the revised sprint map above. Existing Sprint 0, Sprint 1, and
Sprint 2 detail should be preserved, because those names correspond to
actual branch history and design documents.

For Sprint 3 onward, the implementation plan should rewrite the roadmap
section rather than patch individual bullets. That avoids mixed numbering
where old Sprint 5 deliverables still refer to new Sprint 10-14 work.

The intermediate milestones should become milestone bands:

- Foundation: Sprint 2 complete.
- Policy and planning MVP: Sprint 6 complete.
- Remote scheduling MVP: Sprint 9 complete.
- Real media CLI MVP: Sprint 14 complete.
- Daemon MVP: Sprint 17 complete.
- Web UI MVP: Sprint 21 complete.
- Plugin SDK MVP: Sprint 24 complete.
- Production candidate: Sprint 27 complete.

## 6. Acceptance Criteria

This roadmap rescope is complete when:

- `docs/specs/voom-control-plane-design.md` no longer describes the
  future MVP as ten oversized sprints;
- Sprint 0-2 history remains recognizable and aligned with existing
  design docs;
- future work is split into Sprint 3 through Sprint 27 with consistent
  goals, deliverables, out-of-scope notes, acceptance criteria, and
  verification expectations;
- intermediate milestones match the revised sprint bands;
- no future sprint combines parser/compiler, execution, reporting,
  remote scheduling, real media workers, daemon loops, UI, plugin SDK,
  and production hardening into one untestable container;
- documentation-only verification passes.

## 7. Out Of Scope

- Implementing Sprint 3 or later runtime behavior.
- Renaming existing branch history or completed Sprint 0-2 documents.
- Changing Rust crates, database migrations, tests, or CI commands.
- Deciding exact policy grammar syntax, remote TLS details, UI layout, or
  plugin packaging format. Those belong to the owning future sprint
  specs.
