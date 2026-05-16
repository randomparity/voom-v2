# VOOM — Video Orchestration Operations Manager

A control-plane-first Rust application for managing video libraries through
policy-driven planning, durable job execution, and out-of-process providers.

This is the Sprint 0 skeleton: an empty-but-real workspace with the
engineering guardrails every later sprint inherits. Domain logic lands in
Sprint 1+.

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
| `voom-core` | Shared domain types: `VoomError`, `VersionInfo`, `Config`, IDs. |
| `voom-store` | SQLite pool, migrations, repositories. |
| `voom-control-plane` | App-services layer wrapping `voom-store`. |
| `voom-api` | axum HTTP router (no server binary yet). |
| `voom-cli` | `voom` binary with `version` / `health` / `init` subcommands. |
| `voom-events` / `voom-policy` / `voom-plan` / `voom-scheduler` / `voom-artifact` / `voom-worker-protocol` | Reserved for later sprints. |

## Design and decisions

- Spec: `docs/specs/voom-control-plane-design.md`
- Sprint 0 design: `docs/superpowers/specs/2026-05-15-voom-sprint-0-design.md`
- ADRs: `docs/adr/`
- Release runbook: `docs/release-process.md`

## License

Apache-2.0.
