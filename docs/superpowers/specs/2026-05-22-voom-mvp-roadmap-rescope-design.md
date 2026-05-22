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
- Verification commands: exact commands required for closeout.
- Closeout documentation: the spec or acceptance matrix that records
  release readiness.

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

### Policy And Planning

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 3 | Define the policy domain model and media snapshot inputs. | Synthetic media snapshots, identity evidence, bundle targets, quality profiles, issue inputs, and policy-domain fixtures are represented without a parser. |
| Sprint 4 | Add the policy parser, validator, and compiled policy model. | Valid policies compile deterministically; invalid policies produce stable machine-readable errors. |
| Sprint 5 | Generate plan DAGs and support dry-run inspection. | Non-compliant synthetic inputs produce deterministic plan DAG JSON with dependency and priority metadata. |
| Sprint 6 | Produce compliance reports and execute synthetic policy plans. | Reports explain compliance/noncompliance, issue creation is durable, and synthetic policy plans execute through the Sprint 2 worker path. |

### Remote Scheduling

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 7 | Add node registry and authenticated worker registration. | Nodes and remote-capable workers register durably with authenticated identity and inspectable health state. |
| Sprint 8 | Add remote worker leases, heartbeats, and stale recovery. | Remote synthetic workers execute leased tickets; lost workers/nodes trigger stale lease recovery. |
| Sprint 9 | Add scheduler scoring for capability, health, locality, cost, and concurrency. | Scheduler choices are explainable, deterministic under fixtures, and respect node/worker concurrency limits. |

### Real Media CLI

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 10 | Add real ingest and the `ffprobe` worker. | CLI scan creates file assets, versions, locations, hashes, and media snapshots using the out-of-process provider protocol. |
| Sprint 11 | Add staged artifact commit and verification worker. | Generated artifacts are staged, verified, committed, audited, and recoverable on failure. |
| Sprint 12 | Add the FFmpeg transcode worker. | One policy-driven transcode path runs through the provider protocol and staged artifact commit flow. |
| Sprint 13 | Add the MKVToolNix remux / track-edit worker. | One remux or track-edit path runs through the provider protocol and staged artifact commit flow. |
| Sprint 14 | Add backup worker, sidecar ingest, and full CLI media workflow. | CLI can scan, evaluate, plan, execute, inspect reports, show bundles/sidecars, and preserve the out-of-process provider boundary. |

### Daemon

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 15 | Add filesystem watcher, file stability rules, scan sessions, and reconciliation. | Adds, modifications, removals, renames, and debounce windows produce correct durable state. |
| Sprint 16 | Add background scheduler loop, scheduling windows, and dynamic throttles. | Queued work runs continuously and scheduling windows/throttles affect leasing without changing policy results. |
| Sprint 17 | Add daemon recovery loops for issues, external sync, use lease cleanup, and restart recovery. | The daemon recovers from restart, updates issue lifecycle, cleans stale use leases, and runs external-system health/sync jobs. |

### Web UI

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 18 | Add the read-only operations console. | Activity, queue, jobs, tickets, workers, nodes, provider capabilities, and recent events are inspectable through the UI. |
| Sprint 19 | Add library and file detail views. | File assets, versions, locations, media snapshots, identity evidence, media work, variants, bundles, and sidecars are inspectable. |
| Sprint 20 | Add policy and reporting views. | Compliance, issues, quality scores, external sync/path mappings, use leases, retention, and blocked-operation reports are inspectable. |
| Sprint 21 | Add UI action flows and live event streaming. | UI actions use the same API as CLI/daemon workflows and live state updates are delivered through the event stream. |

### Plugin SDK

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 22 | Add plugin manifest, package layout, and schema registration. | Namespaced operation and result schemas can be registered, validated, and referenced by the policy compiler. |
| Sprint 23 | Add provider SDK examples and conformance runner. | Example providers use the SDK and pass the public conformance suite. |
| Sprint 24 | Add compatibility checks, provider-author docs, and a sample third-party provider. | Version compatibility is enforced and a sample third-party provider can be installed, validated, and documented. |

### Hardening And Release

| Sprint | Goal | Acceptance focus |
|---|---|---|
| Sprint 25 | Add safety gates. | Approval gates, rollback flows, backup policies, and destructive-operation controls are durable and testable. |
| Sprint 26 | Add observability. | Metrics, trace IDs, scheduler decision logs, identity/variant/issue/external-sync/use-lease reports, and failure routing explanations are inspectable. |
| Sprint 27 | Prepare production release. | Packaging, upgrade tests, security review, sample policies, user docs, provider docs, benchmark gates, release process, and backup/restore docs are ready. |

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
