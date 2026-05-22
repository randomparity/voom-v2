# VOOM — Video Orchestration Operations Manager

A control-plane-first Rust application for managing video libraries through
policy-driven planning, durable job execution, and out-of-process providers.

The current workspace contains the Sprint 1 durable control-plane
foundation plus Sprint 2 synthetic worker protocol, fake-provider,
conformance, and durable workflow closeout surfaces. Real media tooling,
remote-node TLS registration, daemon mode, and UI work remain later
sprints.

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
| `voom-store` | SQLite pool, migrations, repositories, durable jobs, tickets, leases, identity, and bundle state. |
| `voom-control-plane` | App-services layer and Sprint 2 `WorkflowExecutor` scheduler closeout path. |
| `voom-api` | axum HTTP router (no server binary yet). |
| `voom-cli` | `voom` binary with `version` / `health` / `init` subcommands. |
| `voom-worker-protocol` | Versioned HTTP/JSON worker protocol, credentials, NDJSON progress codec, and loopback transport. |
| `voom-conformance` | Black-box worker protocol conformance harness and `echo-worker`. |
| `voom-fake-support` / `voom-fakes` | Shared fake-provider runtime plus Sprint 2 fake, chaos, and benchmark worker binaries. |
| `voom-scheduler` | Worker selection boundary used by the Sprint 2 workflow executor and extended in later scheduling sprints. |
| `voom-events` / `voom-policy` / `voom-plan` / `voom-artifact` | Reserved or partial surfaces for later sprints. |

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
