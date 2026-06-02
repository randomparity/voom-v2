# VOOM — Video Orchestration Operations Manager

A control-plane-first Rust application for managing video libraries through
policy-driven planning, durable job execution, and out-of-process providers.

The current workspace contains the durable SQLite control plane, policy
parsing and planning, compliance reports and execution, scan/identity
ingest, artifact staging/verification/commit flows, scheduler scoring and
decision inspection, node and worker lifecycle APIs, the worker protocol,
local media worker binaries, conformance tooling, and the agent-facing CLI.
Daemon mode, remote-node TLS transport, and UI work remain later surfaces.

## Getting started

```bash
# One-shot bootstrap: verify toolchain, install hooks, warm cache.
just setup

# Run all checks identical to CI.
just ci

# Smoke-test the CLI end-to-end against an ephemeral database.
just smoke
```

## Workspace map

| Crate | Purpose |
|---|---|
| `voom-core` | Shared domain types, IDs, error codes, and failure classes. |
| `voom-events` | Event envelope, subject, kind, assertion, and payload vocabulary shared by store and control-plane writes. |
| `voom-policy` | VOOM policy DSL parser, AST, validation, compilation, fixtures, and video profile settings. |
| `voom-plan` | Deterministic planning, phase planning, compliance report generation, diagnostics, and plan hashing. |
| `voom-store` | SQLite pool, migrations, repositories, durable jobs, tickets, leases, identity, artifacts, bundles, nodes, workers, scheduler decisions, policies, and summaries. |
| `voom-scheduler` | Scheduler scoring, candidate models, score reasons, decisions, and worker selection helpers. |
| `voom-worker-protocol` | Versioned HTTP/JSON worker protocol, credentials, operation payloads, NDJSON progress codec, and loopback transport. |
| `voom-control-plane` | App-services layer for health, scan, policy inputs, plan generation, compliance execution, artifact orchestration, node/worker lifecycle, scheduler inspection, and durable workflow execution. |
| `voom-api` | axum HTTP router and app state wiring (no server binary yet). |
| `voom-cli` | `voom` binary for `version`, `health`, `init`, `scan`, `plan`, `policy input`, `compliance`, `node`, `profile`, `worker`, `scheduler decisions`, and `artifact` commands. |
| `voom-conformance` | Black-box worker protocol conformance harness and `echo-worker`. |
| `voom-test-support` | Shared integration-test support for control-plane and worker flows. |
| `voom-fake-support` / `voom-fakes` | Shared fake-provider runtime plus fake, chaos, and benchmark worker binaries. |
| `voom-ffprobe-worker` | Local ffprobe-backed media probe worker. |
| `voom-ffmpeg-worker` | Local ffmpeg-backed video transcode and audio extraction/transcode worker. |
| `voom-mkvtoolnix-worker` | Local mkvtoolnix-backed remux worker. |
| `voom-verify-artifact-worker` | Local artifact verification worker. |

Reserved crate:

| Crate | Purpose |
|---|---|
| `voom-artifact` | Future home for reusable artifact domain types; runtime artifact orchestration lives in `voom-control-plane::artifact` today. |

## Design and decisions

- Spec: `docs/specs/voom-control-plane-design.md`
- Sprint 0 design: `docs/superpowers/specs/2026-05-15-voom-sprint-0-design.md`
- Sprint 1 design: `docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md`
- Sprint 2 overview: `docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md`
- Sprint 2 closeout acceptance: `docs/superpowers/specs/2026-05-22-voom-sprint-2-closeout-acceptance-plan.md`
- ADRs: `docs/adr/`
- Release runbook: `docs/release-process.md`

## License

Apache-2.0.
