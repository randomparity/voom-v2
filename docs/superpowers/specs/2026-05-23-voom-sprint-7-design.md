---
name: voom-sprint-7-design
description: Sprint 7 design for durable node identity, token-backed registration, heartbeat state, and node-aware worker inspection.
status: proposed
date: 2026-05-23
sprint: 7
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-23-voom-sprint-5-design.md
  - docs/superpowers/specs/2026-05-23-voom-sprint-6-design.md
---

# VOOM Sprint 7 - Node Registry And Authenticated Registration

## 1. Purpose

Sprint 7 starts the remote-scheduling milestone by making nodes durable,
authenticated control-plane entities. It separates where providers run from
what providers execute, while keeping all execution local to the existing
control-plane paths until Sprint 8.

A node is a host or runtime identity, such as a local machine, remote host, or
synthetic test host. A worker is an executable provider identity, such as a fake
remuxer, fake prober, or future real FFmpeg worker. A node can own multiple
workers. A node-aware worker belongs to one node.

Sprint 7 proves identity, registration, heartbeat, audit, and inspection. It
does not prove remote ticket leasing or artifact access.

## 2. Scope

Sprint 7 delivers:

- A durable `nodes` registry.
- A nullable `workers.node_id` migration for node-aware workers without
  breaking existing worker rows.
- A `NodeRepo` with register, verify token, heartbeat, mark stale, retire,
  get, and list operations.
- Node-aware worker registration that verifies a node registration token and
  links the worker to that node.
- Node heartbeat state with TTL-based stale transitions.
- Registration and heartbeat audit events.
- CLI commands for node registration, heartbeat, list, show, and retire.
- CLI commands for worker registration, list, and show with node context.
- Golden CLI-envelope tests for node and worker inspection behavior.
- Closeout acceptance documentation tying node identity, auth, heartbeat, and
  inspection surfaces to the architecture.

Sprint 7 explicitly does not deliver:

- HTTP registration or heartbeat routes.
- Remote ticket leasing.
- Remote worker dispatch.
- Artifact access plans.
- Scheduler scoring, locality, or cost decisions.
- Node-level concurrency limits.
- Production TLS or certificate management.
- Token rotation.
- Real media workers.
- Daemon heartbeat loops.
- Web UI node views.

HTTP stays deferred to Sprint 8. Sprint 7 still designs the durable auth model
that Sprint 8 will use.

## 3. Architecture

Sprint 7 adds node identity beside the existing worker identity:

```text
Node:   durable host/runtime identity
        local | remote | synthetic
        auth token hash + heartbeat status

Worker: durable provider identity
        synthetic | local | remote
        capabilities + grants + optional node_id
```

`voom-store` owns `nodes` persistence and the `workers.node_id` migration.
`voom-control-plane` owns registration composition, token verification, event
emission, and CLI-facing use cases. `voom-cli` owns JSON-envelope command
dispatch and user-facing argument validation. `voom-worker-protocol` is not
expanded in Sprint 7 because HTTP registration is deferred.

The existing direct worker registration path remains available for internal
tests and legacy synthetic setup. New Sprint 7 CLI worker registration must use
the node-aware path. That keeps migration safe while making the public path
match the remote-capable architecture.

Capabilities and grants remain worker-scoped. Node-level policy, scheduling
windows, and concurrency controls are intentionally deferred to Sprint 9.

## 4. Data Model

Sprint 7 adds a `nodes` table:

- `id`
- `name`
- `kind`: `local`, `remote`, or `synthetic`
- `status`: `registered`, `active`, `stale`, or `retired`
- `registered_at`
- `last_seen_at`
- `retired_at`
- `heartbeat_ttl_seconds`
- `auth_token_hash`
- `auth_token_hint`
- `metadata`
- `epoch`

`name` is unique. `metadata` is JSON and defaults to `{}`. `retired_at` is set
only when `status = retired`. `heartbeat_ttl_seconds` must be positive.

Sprint 7 also adds `workers.node_id` as a nullable foreign key to `nodes(id)`.
The column is nullable because existing workers predate nodes and must remain
listable after migration. New node-aware registration flows set `node_id`.

Worker list and show projections include node context when present:

- `node_id`
- `node_name`
- `node_kind`
- `node_status`
- `node_last_seen_at`

Migrated or internally-created legacy workers may return `node: null`. New CLI
worker registration rejects missing node identity.

## 5. Authentication

Node registration returns a plaintext registration token exactly once in the
command response. The database stores only:

- `auth_token_hash`: a one-way hash of the token.
- `auth_token_hint`: a short non-secret suffix or label for operator
  diagnostics.

No list or show command exposes the plaintext token or token hash. The token is
accepted only by control-plane use cases that need to prove node possession:

- node heartbeat;
- node-aware worker registration.

Sprint 7 uses a deterministic, testable token generator and hasher abstraction
at the control-plane boundary so tests can assert behavior without depending on
OS randomness. Production token generation uses secure randomness and produces
at least 256 bits of random token material. The plaintext token format is:

```text
voom-node-v1.<base64url-random-token>
```

The stored hash format includes a version and domain separator:

```text
voom-node-token-sha256-v1:<hex-sha256("voom-node-token-v1:" + token)>
```

The hash is not a password hash. It is acceptable here because the token is a
high-entropy bearer secret generated by VOOM, not a user-memorable secret.
Verification must use constant-time comparison. Tests may inject fixed tokens
and fixed hashes through the abstraction; production callers must not use the
test generator.

Bearer tokens should not be required on the command line because process
arguments and shell history are commonly observable. CLI commands that
authenticate with a node token read it from an explicit token source:

- `--token-file <path>` reads the token from a local file;
- `--token-env <name>` reads the token from an environment variable;
- `--token-stdin` reads the token from standard input.

These options are mutually exclusive. `--token-file` is the recommended
operator path. Tests may use `--token-env` with an injected deterministic token.

Authentication failures return an existing public error code. Use `NOT_FOUND`
when the node id is absent, `CONFLICT` when the token does not verify or the
node status rejects the requested operation, and `BAD_ARGS` for malformed CLI
input. Do not add a public error code unless implementation proves the existing
vocabulary cannot express the failure.

Token rotation is deferred. If a token is lost during Sprint 7, the operator
registers a replacement node and retires the old one.

## 6. Node Lifecycle

Node status transitions are:

```text
registered --heartbeat--> active
registered --ttl elapsed--> stale
active     --ttl elapsed--> stale
stale      --heartbeat--> active
registered --retire-----> retired
active     --retire-----> retired
stale      --retire-----> retired
```

Retired is terminal for Sprint 7. A retired node rejects heartbeat and
node-aware worker registration.

`register_node` creates a node with `status = registered`, sets
`registered_at = last_seen_at = now`, stores the token hash and hint, and emits
`node.registered`.

`heartbeat_node` verifies the token, updates `last_seen_at`, sets
`status = active`, increments `epoch`, and emits `node.heartbeat_recorded`.
Heartbeat from a stale node is allowed and reactivates it because Sprint 7 has
not introduced remote lease recovery yet.

`mark_stale_nodes(now)` finds non-retired nodes whose `last_seen_at +
heartbeat_ttl_seconds` is before or equal to `now`, sets `status = stale`,
increments `epoch`, and emits `node.marked_stale` once per changed node. It is
idempotent for already stale nodes.

`retire_node` uses an expected epoch, sets `status = retired`, sets
`retired_at`, increments `epoch`, and emits `node.retired`. Retiring a node
does not automatically retire its workers in Sprint 7. Worker lifecycle remains
explicit to avoid hidden state changes before remote lease semantics exist.

## 7. Node-Aware Worker Registration

`register_worker_for_node` is the public Sprint 7 worker registration path. It
takes:

- node id;
- plaintext node token;
- worker name;
- worker kind;
- one or more advertised capabilities;
- optional initial grants for control-plane tests and future callers.

The use case verifies the node token and checks node liveness. A node in
`registered` or `active` status may register workers only while its heartbeat
TTL is still fresh at the control-plane clock value used by the transaction. A
`stale` or `retired` node must be rejected; the operator must heartbeat a stale
node back to `active` before registering more workers. On success, the use case
creates the worker with `workers.node_id = node.id`, records capabilities and
grants, and emits worker events inside the same transaction.

The event sequence for a worker registration with capabilities is:

- `worker.registered`;
- `worker.linked_to_node`;
- one `worker.capability_recorded` per capability;
- one `worker.grant_recorded` per grant.

If any insert or event append fails, the transaction rolls back and no partial
worker registration remains.

Sprint 7 does not require `register_worker_for_node` to mutate node heartbeat
state. Node possession is proven by the token, but liveness remains the
heartbeat command's job.

## 8. Events

Sprint 7 extends the event vocabulary minimally:

- `SubjectType::Node` with wire value `node`.
- `node.registered`
- `node.heartbeat_recorded`
- `node.marked_stale`
- `node.retired`
- `worker.linked_to_node`

Node event payloads include the node id and enough state to audit the change.
`worker.linked_to_node` includes worker id and node id.

The event log remains audit-only. Node events do not route work, trigger
leases, or replace the durable node and worker tables.

## 9. CLI Surface

All Sprint 7 commands preserve the existing CLI envelope contract: exactly one
JSON object on stdout, logs on stderr, and stable public error codes.

Node commands:

```text
voom node register --name <name> --kind <local|remote|synthetic> \
  [--heartbeat-ttl-seconds <seconds>]

voom node heartbeat --node-id <id> \
  (--token-file <path> | --token-env <name> | --token-stdin)

voom node list [--status <registered|active|stale|retired>]

voom node show --node-id <id>

voom node retire --node-id <id> --expected-epoch <epoch>
```

`node register` is the only command that returns the plaintext token. It also
returns token hint, node id, status, and heartbeat TTL. `node list` and
`node show` never expose the token hash.

Worker commands:

```text
voom worker register --node-id <id> --name <name> \
  --kind <local|remote|synthetic> --capability <operation> \
  (--token-file <path> | --token-env <name> | --token-stdin)

voom worker list [--status <registered|active|stale|retired>]

voom worker show --worker-id <id>
```

`--capability` is repeatable. Sprint 7 CLI registration records the operation
name with empty codec, hardware, artifact access, and extra capability fields.
Richer capability and grant authoring can remain control-plane/test-only until
the worker protocol needs it.

Worker list and show include a nested `node` object when the worker is linked.
Legacy workers return `node: null`.

## 10. Testing And Acceptance

Sprint 7 acceptance is durable and externally inspectable:

- A node can be registered and inspected.
- A node token is returned once and is never exposed by list/show.
- A valid token can heartbeat a node.
- An invalid token cannot heartbeat a node or register a worker.
- Stale marking is idempotent and emits one event per changed node.
- A stale node rejects worker registration until it heartbeats successfully.
- A retired node rejects heartbeat and worker registration.
- A worker can be registered through a node and inspected with node context.
- Legacy workers with `node_id = null` remain listable.
- Node and worker registration audit events are transactional.

Required verification:

- Migration tests for `nodes`, `workers.node_id`, JSON checks, and foreign keys.
- Repository tests for node register, get, list, heartbeat, mark stale, retire,
  token verification, and token non-disclosure.
- Control-plane tests for node registration, node-aware worker registration,
  event emission, and rollback on failure.
- CLI golden tests for node register, heartbeat, list, show, retire, worker
  register, worker list, worker show, and auth failure envelopes.
- Documentation placeholder scan.
- `just ci`.

## 11. Closeout Matrix

Sprint 7 closeout must record evidence for:

- node identity schema and migration behavior;
- token storage and non-disclosure;
- heartbeat and stale-state behavior;
- node-aware worker registration;
- worker inspection with node context;
- event audit coverage;
- explicit deferral of HTTP registration to Sprint 8.

The sprint is not complete until this evidence is repeatable from tests and
CLI golden outputs.
