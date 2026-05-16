---
status: accepted
date: 2026-05-15
deciders: [VOOM core]
---

# 0002 — All providers are out-of-process workers from day one

## Context

Plugin systems that allow both in-process and out-of-process execution
develop two divergent code paths, two security models, and two failure
modes. Subtle bugs accumulate where in-process behavior diverges from
out-of-process behavior.

## Decision

Every provider — built-in or third-party — runs as an out-of-process worker
speaking the same versioned HTTP/JSON protocol from Sprint 2 onward. No
in-process fast path exists. Workers receive `ArtifactHandle`s rather than
raw paths; large bytes move via artifact backends, not through the control
protocol.

## Consequences

- Built-in providers face the same crash, timeout, malformed-result, and
  trust constraints as third-party providers, which means chaos-tested
  reliability is uniform.
- Sprint 0 ships the empty `voom-worker-protocol` crate so the boundary is
  visible from day one even before the wire format lands in Sprint 2.
- Same-machine workers pay a small IPC overhead. Acceptable.
- Capability grants are explicit and enforced by the host regardless of
  worker origin.

## Alternatives Considered

- **In-process built-ins, out-of-process plugins.** Rejected: two code
  paths, two security models, ongoing divergence risk.
- **Embedded Lua/WASM plugins.** Rejected for v1: increases attack surface
  and language complexity before the core protocol is proven.
