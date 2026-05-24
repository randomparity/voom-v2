---
name: voom-sprint-8-design
description: Sprint 8 design for thin remote execution APIs, remote synthetic runner proof, lease heartbeat/recovery, and synthetic artifact access plans.
status: proposed
date: 2026-05-24
sprint: 8
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-23-voom-sprint-7-design.md
---

# VOOM Sprint 8 - Remote Synthetic Execution

## 1. Purpose

Sprint 8 proves remote synthetic execution through the real control-plane
boundary. A remote-capable synthetic runner authenticates as a Sprint 7 node,
operates only workers linked to that node, acquires durable ticket leases,
heartbeats while executing, completes or fails leases through the control
plane, and leaves enough artifact-access evidence for operators and tests to
inspect what happened.

Sprint 8 is not the general REST API milestone. It introduces only the HTTP
surface required to prove remote execution. API breadth, scheduler scoring,
concurrency explanations, and locality/cost decisions stay in Sprint 9.

## 2. Scope

Sprint 8 delivers:

- Thin node-authenticated HTTP execution routes for remote node heartbeat,
  lease acquisition, lease heartbeat, lease completion, and lease failure.
- A deterministic ready-ticket acquisition path for an authenticated
  node-linked worker.
- A remote synthetic fixture setup path that registers node-aware workers with
  both capabilities and execution grants before the runner starts.
- A remote synthetic runner launched by tests or CLI, not by a daemon.
- Remote synthetic worker integration tests that execute durable tickets over
  HTTP.
- Lease heartbeat and stale recovery coverage for lost remote runners.
- Node stale recovery coverage using the Sprint 7 node heartbeat state.
- A typed artifact access plan model for synthetic remote dispatches.
- Synthetic artifact access fixtures for remote inputs and outputs.
- Durable artifact access evidence showing which selected plan a remote
  dispatch consumed or rejected.
- Closeout documentation tying remote lease lifecycle, recovery, and artifact
  access planning to the architecture.

Sprint 8 explicitly does not deliver:

- A broad public REST API.
- Scheduler scoring, locality/cost ranking, or decision logs.
- Node-level or worker-level concurrency limits beyond existing lease checks.
- Real artifact transfer.
- Production object storage.
- Production TLS, certificates, or token rotation.
- Production-safe remote networking over untrusted networks.
- A daemonized remote node agent.
- Web UI node or execution views.
- Real media workers.

## 3. Architecture

The Sprint 8 execution path is:

```text
remote synthetic runner
  -> node-token authenticated HTTP execution API
  -> ControlPlane lease / ticket / worker / node use cases
  -> SQLite durable state + audit events
  -> synthetic worker protocol dispatch
  -> typed result with artifact access plan evidence
```

`voom-api` owns the HTTP route layer. `voom-control-plane` owns validation and
durable state transitions. `voom-store` owns any new persistence required for
artifact access plans. `voom-fakes` owns the remote synthetic runner and the
synthetic provider behavior used by integration tests.

The HTTP route layer must call control-plane use cases. It must not bypass
repositories directly, create an event-driven work router, or embed scheduler
policy that belongs in Sprint 9.

Because production TLS and certificate management are out of scope, Sprint 8's
bearer-token HTTP execution surface is not production-safe on untrusted
networks. Supported Sprint 8 deployments are loopback, the integration-test
harness, or an explicitly trusted isolated network used only to prove synthetic
remote execution. Documentation and closeout must not present this transport as
ready for production remote workers.

## 4. Remote Execution API

The HTTP execution API is node-authenticated and worker-scoped. Each mutating
request presents the Sprint 7 node token with:

```text
Authorization: Bearer <token>
```

Each request identifies the `node_id` either in the route or request body.
Worker-scoped routes also identify `worker_id`. The control plane verifies:

- the node exists;
- the node token matches;
- the node is not retired;
- for acquire, the node heartbeat is fresh enough for work acquisition;
- for worker-scoped routes, the worker exists;
- for worker-scoped routes, `workers.node_id == node_id`;
- for worker-scoped routes, the worker is not retired;
- for acquire, the worker advertises the ticket operation;
- for acquire, the worker has a grant allowing the ticket operation;
- for lease heartbeat, complete, and fail, lease ownership matches the worker.

Minimal routes:

```text
POST /v1/execution/lease/acquire
POST /v1/execution/node/{node_id}/heartbeat
POST /v1/execution/lease/{lease_id}/heartbeat
POST /v1/execution/lease/{lease_id}/complete
POST /v1/execution/lease/{lease_id}/fail
```

`acquire` is not Sprint 9's scheduler. It selects the next ready ticket
matching one of the authenticated worker's advertised capabilities and granted
operations using deterministic ordering:

1. priority;
2. `next_eligible_at`;
3. ticket id.

If no work is available, `acquire` returns an explicit non-error idle outcome.
Remote runners must be able to poll without treating "no work available" as a
failure.

The acquire response includes:

- lease id;
- ticket id;
- worker id;
- operation kind;
- dispatch payload;
- lease TTL;
- recommended heartbeat cadence;
- selected artifact access plan.

Every mutating route requires `X-Voom-Idempotency-Key`. The key is scoped to
the authenticated node, route, and worker when the route is worker-scoped. It
is paired with a request hash and rejected when the same key is reused with a
different request body. A same-key retry of the same request replays the
original successful or failed route outcome without performing the state
transition a second time.

Runners generate a fresh key for each new logical mutation. They reuse that key
only when retrying the same mutation after a timeout, disconnect, or lost
response. Reusing the same key for later heartbeat ticks must replay the old
heartbeat and therefore must not be used as the normal heartbeat cadence.

This is required for remote execution because response loss is not evidence
that the control plane failed to commit. In particular:

- retrying `acquire` after a lost response must return the original lease
  instead of acquiring a second ticket or abandoning the first lease until
  expiry;
- retrying node heartbeat after a lost response must return the original
  heartbeat outcome instead of emitting a second heartbeat event;
- retrying `complete` or `fail` after a lost response must return the original
  terminal outcome instead of surfacing a confusing terminal-state conflict;
- retrying `heartbeat` with the same key must not emit events and must not be
  treated as a protocol error.

Lease heartbeat produces no audit event. Heartbeats are high-volume observable
state, recorded through `last_heartbeat_at` and `expires_at`. Missed heartbeats
also produce no event by themselves. Audit events are emitted only when the
control plane takes a durable recovery action, such as lease expiry, ticket
requeue/failure, or node stale marking.

Complete and fail reuse the existing control-plane lease paths so ticket
success, retry, dependency promotion, and failure events stay identical to
local execution.

## 5. Artifact Access Plans

Sprint 8 introduces artifact access plans as durable, typed execution
contracts. A remote worker never receives an implicit path assumption. It
receives an explicit plan for how inputs are visible and where outputs are
expected.

The initial model uses synthetic access modes only:

- `shared_mount`
- `control_plane_placeholder`
- `staged_output_placeholder`

The model is stored in a dedicated artifact access plan table or an equivalent
first-class repository owned by `voom-store`. It must be queryable by lease,
ticket, worker, node, and access mode so Sprint 9 can build deterministic
locality/cost scoring fixtures without mining opaque JSON blobs. Each record
carries:

- lease id;
- ticket id;
- worker id;
- node id;
- input artifact handle references;
- output artifact handle expectations;
- selected access mode;
- status: `selected`, `consumed`, `rejected`, or `failed`;
- structured reason/evidence;
- created and updated timestamps.

The acquire response includes the selected access plan. The synthetic runner
validates that every selected access mode is compatible with the worker's
advertised `artifact_access` capability. It does not read or write real media
bytes.

A successful synthetic operation returns typed evidence such as:

```json
{
  "artifact_access": {
    "inputs_consumed": ["handle:input:1"],
    "outputs_declared": ["handle:output:1"],
    "mode": "shared_mount",
    "validated": true
  }
}
```

If no compatible access mode exists, the runner fails the lease visibly. The
default classification is retriable while Sprint 9 scheduler scoring is not yet
responsible for avoiding every bad match. Malformed plan data or policy-invalid
plan data is terminal.

Real path translation, file transfer, checksums over real bytes, object-store
credentials, cleanup, and production staging directories are out of scope.

## 6. Remote Synthetic Runner

The remote synthetic runner is a test/CLI-launched process, not a daemon. It
accepts:

- control-plane base URL;
- node id;
- token source;
- worker identity;
- polling limits;
- optional idle timeout.

The runner uses an already registered node-aware worker. Tests that need a
self-contained fixture register the node and worker through existing local
control-plane setup before launching the remote runner, including the execution
grants required by the remote acquire path. HTTP worker registration and broad
CLI grant-authoring remain out of scope for Sprint 8.

The runner loop is:

```text
heartbeat node
poll acquire
  no work -> sleep/backoff until idle limit
  lease -> dispatch synthetic worker operation
           heartbeat lease while operation runs
           complete or fail lease
repeat
```

The runner dispatches synthetic operations through the existing worker protocol
path. It must not special-case remote execution into a separate in-process
provider path. Built-in and future third-party workers should still share one
out-of-process execution contract.

## 7. Recovery

Recovery remains control-plane owned. Sprint 8 adds a recovery path that tests
and CLI can invoke, and that later daemon sprints can reuse:

```text
mark stale nodes
expire due execution leases
```

A node becoming stale does not directly route work. It is a health fact. Held
leases recover through lease expiry, which emits the existing durable
lease/ticket recovery events.

Sprint 8 does not mutate worker status when a node becomes stale. The worker
row remains durable identity. Sprint 9 scheduler scoring can treat
`node.status = stale` as ineligible or low-score without needing worker revival
semantics. A stale node cannot acquire new remote work. If the node heartbeats
successfully again, its non-retired workers may acquire future work.

Recovery acceptance cases:

- a runner that stops heartbeating its lease eventually has the lease expired
  and the ticket requeued or failed according to attempts;
- a runner that stops heartbeating its node eventually has the node marked
  stale;
- a stale node cannot acquire new remote work;
- a reactivated node can acquire future work if its workers remain valid;
- recovery emits state-transition events, not missed-heartbeat events.

## 8. Error Handling

Remote execution routes preserve the public error-code contract. Use existing
codes unless implementation proves a new code is necessary.

- Missing node, worker, ticket, or lease: `NOT_FOUND`.
- Token mismatch, retired node, stale node on acquire, worker/node mismatch,
  retired worker, lease/worker mismatch, or invalid state transition:
  `CONFLICT`.
- Malformed route input or mutually invalid client arguments: `BAD_ARGS`.
- Database or unexpected internal failures: existing runtime/internal codes.

`acquire` with no matching ready work is not an error. It returns an explicit
idle outcome.

## 9. Testing And Acceptance

Sprint 8 acceptance is durable and externally inspectable:

- A node-authenticated remote worker can acquire a ready synthetic ticket over
  HTTP.
- A node can heartbeat remotely over HTTP and repeated same-key heartbeat
  retries do not emit duplicate heartbeat events.
- A worker cannot acquire work for a node it does not belong to.
- Invalid node tokens reject acquire, heartbeat, complete, and fail.
- Retired or stale nodes cannot acquire work.
- Lease heartbeat extends the remote lease without emitting heartbeat events.
- Completing a remote lease uses the existing success path:
  `lease.released`, `ticket.succeeded`, and dependency promotion.
- Failing a remote lease uses the existing failure/retry path.
- A stopped runner's lease expires and requeues or fails the ticket according
  to `max_attempts`.
- A stopped node becomes stale through the existing node stale transition.
- Every remote dispatch records a selected artifact access plan.
- The synthetic runner validates artifact access plans against advertised
  capability.
- The remote acquire path requires both advertised capability and an execution
  grant for the selected ticket operation.
- The remote synthetic fixture setup creates at least one worker with matching
  capabilities and grants, proving the acquire path can run without weakening
  the grant check.
- Incompatible artifact access fails visibly and does not mark the ticket
  succeeded.
- Idle polling returns a non-error "no work available" outcome.
- Same-key/same-body retries for node heartbeat, acquire, lease heartbeat,
  complete, and fail replay the original route outcome.
- Same-key/different-body retries for node heartbeat, acquire, lease heartbeat,
  complete, and fail are rejected without mutating node, lease, or ticket state.

Required verification:

```text
cargo test -p voom-api
cargo test -p voom-control-plane remote
cargo test -p voom-store artifact_access
cargo test -p voom-fakes remote
just ci
```

Exact test names may shift during implementation, but targeted route,
control-plane, store, and fake-runner tests plus full `just ci` are required.

## 10. Closeout Matrix

Sprint 8 closeout must record evidence for:

- node-token authenticated remote execution routes;
- explicit non-production transport boundary for bearer-token HTTP;
- worker-to-node ownership enforcement;
- remote node heartbeat plus lease acquire, heartbeat, complete, and fail
  behavior;
- remote execution route idempotency and duplicate-key rejection;
- worker capability and grant enforcement during acquire;
- synthetic worker setup with explicit execution grants;
- remote runner execution of synthetic durable tickets;
- stale lease recovery;
- stale node recovery;
- no audit events for individual missed heartbeats;
- artifact access plan persistence;
- synthetic artifact access validation;
- explicit deferral of scheduler scoring and broad API hardening to Sprint 9.

The sprint is not complete until this evidence is repeatable from tests and
the closeout document links each acceptance item to a verification command or
fixture.
