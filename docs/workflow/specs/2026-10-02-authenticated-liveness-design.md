# Authenticate policy runtime liveness (#670)

## Problem and authority

Scope q670-88ac075b, issue #670, approved campaign scope on 2026-10-02.
A successful handshake proves protocol reachability, not the registered worker's identity.
A stale endpoint rebound by another worker therefore survives the policy execution probe.

## Design

Keep registry construction and filtering in the control plane. Replace the handshake in
`probe_live_runtimes` with `runtime.client.identity(&runtime.credentials)` under the existing
500 ms timeout. `HttpClient::identity` already creates a fresh challenge and verifies proof,
protocol version, worker ID, and epoch against the registry credentials. Reuse it unchanged.
Both `live_policy_runtime_registry` and compliance execution already call this probe;
neither caller needs migration. Identity failure removes only the in-memory runtime entry.
The durable worker row and existing actionable dead-endpoint errors remain unchanged.

Alternatives: retain handshake plus identity (redundant request); implement identity checks
in the control plane (duplicates protocol ownership). Reusing the existing verified client
is the smallest correction. No new architecture decision is needed; ADR 0110 remains unused.

## Success

- A valid recorded worker remains in the live registry.
- A different worker's valid handshake and valid self-identity do not retain the stale row.
- Wrong recorded ID, epoch, or secret is rejected by the existing identity verifier.
- Unreachable and nonresponsive endpoints are filtered within the existing per-probe bound.

## Global Constraints

Use existing workspace dependencies and toolchain; no new dependency or version floor.
Preserve sibling unit-test layout and use real time for tests that open SQLite pools.
Run on the host; hosted CI supplies Linux and macOS coverage. No declared architecture targets.

## Failure model

- Actors and deployments: local operator running policy execution against registered direct
  HTTP worker endpoints; another local worker can rebind a stale endpoint.
- Invariants and assets: registry liveness must represent the recorded worker and incarnation;
  stale endpoints must not cause dispatch under another worker's identity.
- Accepted failure classes: a worker can disappear after a successful probe; existing dispatch
  authentication and execution error handling govern that later operation. Sequential per-worker
  probing retains the existing aggregate latency behavior.
- Covered elsewhere: proof construction, version/ID/epoch comparison and HTTP transport parsing
  belong to voom-worker-protocol and its identity tests; durable retirement belongs to lifecycle code.

## Threat model

- Boundary inventory: existing HTTP endpoint responses cross into the trusted runtime registry;
  no boundary is added or widened.
- Actor model: a different local worker may answer at the recorded endpoint. Registry credentials
  and the existing identity implementation are trusted; credential theft is outside this repair.
- Control per boundary: fresh challenge and verified identity bound to recorded credentials,
  with the existing 500 ms outer timeout. Failure drops the runtime without emitting credentials.
- Out of scope: hostile host compromise and post-probe process replacement are governed by
  operating-system protections and per-dispatch authentication, respectively.

## Validation

Use the real HTTP server/client in a regression that first proves a listener's valid handshake
and self-identity, then filters registries with matching and mismatched credentials. Cover ID,
epoch, and secret independently. The pre-fix handshake must incorrectly retain mismatches.
Use a bound TCP listener that never responds to prove the probe timeout, bounded externally
by two seconds; hold its listener open until the probe ends. Keep the existing unreachable test.
Run focused policy tests, source checks, lint, mandatory native hooks, then one final `just ci`.
