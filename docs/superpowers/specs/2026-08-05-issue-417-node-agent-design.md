# Issue 417: Pull-based node agent design

## Scope and authority

Issue #417 requires one production node-agent process per host. It authenticates as an
existing durable logical node, starts a new fenced incarnation on every process lifetime,
supervises configured child workers, polls leases per worker, keeps node and lease
heartbeats independent of worker progress, retries lost control-plane responses safely,
and shuts down with inspectable durable state. Accepted ADR 0050 supplies the stable
logical-node/incarnation split and the node-local worker trust boundary. ADR 0057 records
the activation, fencing, and worker-declaration decision used here.

This change proves the lifecycle with synthetic or otherwise already-supported worker
operations. Storage-root activation and resolution, scan/hash/probe ownership, node-local
media requests, distributed verification and commit intents, tool locality, and the
two-host byte-blind acceptance workflow remain owned by issues #418 through #425. It adds
no compatibility mode for unfenced remote execution.

## Success criteria

1. Starting an agent twice with the same logical-node ID preserves that ID, creates two
   distinct incarnation IDs, marks the first incarnation superseded, retires its workers,
   and rejects every subsequent mutation from the first incarnation.
2. Node heartbeats run in a task independent of polling and dispatch. Each held lease has
   its own heartbeat task that remains active while progress is silent and while a terminal
   complete/fail request is being retried.
3. A control-plane request retains one idempotency key and byte-equivalent JSON body across
   transport, timeout, and retriable server failures. An activation response lost after
   commit replays the same incarnation and worker IDs without creating more rows.
4. Child startup failure, exact-protocol mismatch, identity mismatch, child exit, exhausted
   restart attempts, control-plane outage, incarnation conflict, node staleness, and
   graceful shutdown have deterministic terminal behavior and durable evidence described
   below.
5. Every child is forced to bind an ephemeral IPv4 loopback address, and the agent verifies
   the advertised address before using it. Operation requests use the worker protocol's
   ID, epoch, bearer secret, and identity proof.
6. `voom node show` reports the current incarnation, and `voom node incarnation list`
   reports ordered historical status, timestamps, reason, and worker count without
   exposing node or worker secrets.

## Approaches considered

### Selected: atomic activation manifest

One activation request carries the new incarnation ID and the complete configured worker
manifest. The server replaces the active process lifetime and registers all workers in one
transaction. This gives retries one durable replay record and makes the response the only
mapping from configured workers to durable worker IDs.

### Rejected: incremental registration

Activating the node and registering workers one at a time reduces one transaction's size,
but a timeout leaves the agent unable to distinguish a committed worker from a missing
worker. Correct recovery requires a second list-and-reconcile protocol, deterministic
request identities for each worker, and cleanup of partial manifests.

### Rejected: promote the synthetic runner

The synthetic runner already calls acquire, heartbeat, complete, and fail, but it invokes
fake providers in process and performs a node heartbeat only as part of its polling loop.
It cannot satisfy child isolation, exact child identity, independent heartbeats, restart
fencing, or graceful retirement without becoming a different component.

## Durable model and migration 0035

`NodeIncarnationId` is a strict lowercase 32-hex-character newtype. Generation uses the OS
random source. Deserialization and persisted-row decoding reject any other length or
alphabet before applying domain classification.

Migration `0035_node_incarnations.sql` creates:

```text
node_incarnations
  incarnation_id TEXT PRIMARY KEY
  node_id INTEGER NOT NULL REFERENCES nodes(id)
  status TEXT NOT NULL: active | superseded | retired | failed
  started_at TEXT NOT NULL
  last_seen_at TEXT NOT NULL
  ended_at TEXT NULL
  end_reason TEXT NULL
```

Exactly one `active` row may exist per node, enforced by a partial unique index. Active
rows have no end fields; terminal rows have both. `nodes.active_incarnation_id` is nullable
and references the table. `workers.node_incarnation_id` is nullable for historical and
node-less rows and references the table. A worker with a non-null incarnation must have a
node, and repository decoding rejects a worker whose node and incarnation disagree.

The migration preserves existing nodes and workers. Their active-incarnation fields are
null; they remain inspectable, but remote acquire no longer treats an incarnation-less
worker as a production remote worker. No JSON payload is rewritten.

The repository owns all SQL and exposes typed transactional operations to activate,
heartbeat, end, list, count workers, bind workers to an incarnation, and retire the live
workers of an ended incarnation. All numeric conversions and incarnation decoding are
checked before status, presence, or conflict classification.

## Lifecycle and transaction ordering

### Activation

`POST /v1/execution/node/{node_id}/activate` requires the existing bearer token and
`x-voom-idempotency-key`. Its strict JSON body is:

```json
{
  "incarnation_id": "0123456789abcdef0123456789abcdef",
  "workers": [{
    "logical_name": "echo",
    "operations": ["probe_file"],
    "artifact_access": ["control_plane_placeholder"],
    "max_parallel": 1
  }]
}
```

The request is rejected when there are zero workers; more than 64 workers; a logical name
is empty, longer than 64 bytes, duplicated, or outside lowercase ASCII letters, digits,
dot, underscore, and hyphen; operations or access modes are empty or duplicated; or
parallelism is zero or greater than 256.

After token verification, the control plane begins an immediate transaction and reserves
the activation replay slot. It validates the entire manifest before mutation. It then ends
the current incarnation as `superseded`, retires that incarnation's live workers with the
existing worker retirement facts, inserts the new active incarnation, updates the node to
`active` with the new pointer and a single epoch increment, registers every declared remote
worker, records one capability per operation and one derived execute grant, appends the
incarnation activation fact, stores the strict response, and commits. Any failure rolls the
whole sequence back.

The response contains the logical node ID and epoch, incarnation ID, heartbeat TTL, and one
worker descriptor per request entry (`logical_name`, `worker_id`, `worker_epoch`). A replay
returns this exact mapping. Reusing an incarnation under another activation key conflicts,
including after it is terminal.

### Heartbeat and fencing

The existing node-heartbeat, acquire, lease-heartbeat, complete, and fail request bodies
gain a required `incarnation_id`. The old body shapes are rejected by strict deserialization.
Before reservation or mutation, the control plane verifies:

- the node token;
- an active node incarnation equal to the presented ID; and
- for worker-scoped calls, a worker whose node and incarnation equal the request.

The server prefixes the repository idempotency key with the validated incarnation ID.
This changes the internal namespace without rebuilding historical replay rows. Activation
uses its distinct route key and unprefixed process key because it creates the incarnation.

A node heartbeat updates both `nodes.last_seen_at`/epoch and the incarnation's
`last_seen_at` in one transaction, then appends the existing node-heartbeat event. It never
reactivates a stale incarnation. `mark_stale_nodes` ends an active incarnation as `failed`
with reason `heartbeat_expired`, retires its workers, clears the node pointer, and appends
the incarnation-ended fact before the existing node-stale fact.

### Deactivation

`POST /v1/execution/node/{node_id}/deactivate` accepts the incarnation ID and one reason
from `graceful_shutdown`, `child_startup_failed`, or `child_restart_exhausted`. The request
is idempotent. It ends the current incarnation as `retired` for graceful shutdown or
`failed` for child failure, retires its workers, clears the node pointer, sets a non-retired
logical node back to `registered`, stores the response, and commits. A replay is allowed
after the incarnation has ended; a different current incarnation is never changed.

Retiring a logical node through the existing operator path first ends its active
incarnation as `retired` with reason `logical_node_retired`, retires its workers, and then
records the existing node retirement in the same transaction.

## Audit and inspection

Two strict event payloads are added:

- `node.incarnation_activated`: logical node ID, incarnation ID, node epoch, and worker IDs;
- `node.incarnation_ended`: logical node ID, incarnation ID, terminal status, reason, and
  retired worker IDs.

The activation/ending facts and their worker retirement facts share the lifecycle
transaction. Existing lease terminal events remain the evidence for child dispatch crash,
malformed protocol output, timeout, cancellation, and control-plane terminal reports.

The typed node projection gains `active_incarnation_id`. `voom node show` includes it as a
nullable string. `voom node incarnation list --node-id N [--limit N]` returns newest-first
incarnations with ID, status, start/last-seen/end timestamps, end reason, and worker count.
The limit uses the existing bounded CLI conventions and deterministic
`started_at DESC, incarnation_id DESC` ordering.

## Agent configuration

`voom-node-agent --config <path>` reads one strict TOML document. Unknown fields fail.
The root fields are:

```toml
control_plane_url = "https://control.example:7443"
ca_cert = "/etc/voom/control-ca.pem" # optional; system roots otherwise
node_id = 7
poll_interval_ms = 1000
lease_ttl_seconds = 30
shutdown_grace_seconds = 10

[node_token]
source = "file"                   # file | env
path = "/run/secrets/voom-node"   # `name` replaces `path` for env

[[workers]]
name = "echo"
program = "/opt/voom/bin/echo-worker"
args = []
operations = ["probe_file"]
artifact_access = ["control_plane_placeholder"]
max_parallel = 1
```

The URL must contain only scheme/authority with no credentials, query, or fragment. HTTPS
is required unless the URL host is loopback and the operator explicitly uses `http`.
Worker programs must be absolute paths. Polling is 50 milliseconds through 60 seconds,
lease TTL is 5 through 3600 seconds, shutdown grace is 1 through 60 seconds, worker count
is 1 through 64, and worker declaration constraints match activation.

The token source is exactly one environment-variable name or file path. Empty tokens and
tokens containing line breaks after one trailing newline is removed are rejected. Secret
values use `SecretString`, never implement visible debug output, never enter request URLs,
and never appear in diagnostics. The configuration records only the reference.

## Agent process and child supervision

At process start the agent validates the complete configuration, loads the token, builds an
HTTP client with the configured trust roots and a finite request timeout, generates one
incarnation ID and activation key, and retries the identical activation request with
exponential backoff from 250 milliseconds capped at 30 seconds. HTTP 408, 429, and 5xx plus
transport failures retry; authentication, validation, conflict, and other 4xx responses are
terminal. Backoff resets after a successful response.

For every returned worker descriptor the agent generates one random worker secret and
spawns the configured program directly with argv; it never invokes a shell. The child
environment is cleared, then receives only `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`,
`VOOM_WORKER_SECRET`, and forced `VOOM_WORKER_BIND=127.0.0.1:0`. Stdout is consumed only for
the bounded readiness line; stderr is inherited for operator diagnostics; stdin remains
piped as the parent-death watchdog. The readiness line has a 4 KiB maximum and a ten-second
deadline. The advertised address must be IPv4 loopback with a nonzero port.

Before polling, the agent performs the existing exact `PROTOCOL_VERSION` handshake and
worker identity challenge. Mismatch kills and reaps the child, deactivates the incarnation
as `child_startup_failed`, and exits nonzero. No lease can be acquired before every child
has passed these checks.

One node-heartbeat task runs independently. One worker task per child checks for child exit,
polls acquire with a fresh logical request key, and sleeps the configured interval after
idle/no-candidate responses. A leased dispatch adds the artifact-access plan and the
configured advertised modes to the object payload, then sends the existing worker protocol
request with an incarnation-and-lease-derived idempotency key.

Each lease starts a separate heartbeat task at the server-provided heartbeat interval. It
does not observe progress frames and remains running while the agent retries complete or
fail. Frame validation enforces lease ID, sequence, terminal shape, and the existing worker
protocol limits. A result payload must be an object before complete. Error frames, worker
exit, malformed streams, and progress timeout produce the matching durable remote fail.
The agent never re-dispatches an uncertain operation to a restarted child.

A child that exits is killed/reaped if necessary and restarted with the same incarnation,
worker ID, epoch, and secret after bounded backoff. Three consecutive startup failures end
the whole incarnation as `child_restart_exhausted`; one successful handshake resets the
counter. This fixed policy avoids a configuration surface before operational evidence
justifies one.

SIGINT or SIGTERM broadcasts shutdown. In-flight operations stop, their children are
terminated, and held leases are reported as `user_cancellation` while lease heartbeats
continue. All children receive stdin EOF and have the configured grace to exit before kill
and reap. The agent then retries deactivation with reason `graceful_shutdown` until success,
a terminal fence/auth response, or a second termination signal. Failure exits nonzero and
leaves the incarnation to become durably failed through heartbeat expiry.

## Failure table

| Failure | Agent action | Durable evidence |
|---|---|---|
| Lost activation/acquire/heartbeat/terminal response | Retry identical body and key | One replay row and one mutation |
| Control-plane outage | Keep independent tasks retrying with capped backoff | Held lease remains visible; node/lease expiry records failure if outage exceeds TTL |
| Prior incarnation or stale node | Stop polling, terminate children, exit nonzero | Superseded/failed incarnation and retired workers |
| Child protocol or identity mismatch | Kill/reap, deactivate, exit nonzero | Failed incarnation with `child_startup_failed` |
| Child crash while leased | Fail lease, restart child without redispatch | Lease failure plus eventual active or failed incarnation |
| Restart budget exhausted | Deactivate and exit nonzero | Failed incarnation with `child_restart_exhausted` |
| Graceful shutdown | Cancel/fail held work, reap children, deactivate | Retired incarnation with `graceful_shutdown` |
| Deactivation response lost | Replay identical deactivation | One ended incarnation and one retirement set |

## Threat model

### Boundaries added

1. **Local config and token reference into the agent.** The local operator controls TOML,
   paths, environment-variable names, worker programs, and argv. Strict decoding, bounds,
   absolute programs, direct argv execution, transport validation, and secret redaction
   control this boundary.
2. **Agent to control plane.** Requests cross the network under the node bearer credential.
   TLS validates server identity; only loopback may use cleartext; API body limits and
   strict DTOs bound input; bearer auth, active-incarnation fencing, worker ownership, and
   idempotency authorize mutations.
3. **Agent to child worker.** A configured local executable receives worker credentials and
   an operation payload. The environment is cleared, binding is forced and verified
   loopback, exact version and identity are challenged, operations are bearer-authenticated,
   and startup/output/time limits bound the child.
4. **Child output into the agent.** Readiness text and NDJSON frames are child-controlled.
   Length/time bounds, strict protocol decoding, lease/sequence validation, object-result
   validation, and fail-without-redispatch behavior control it.
5. **Remote request into SQLite.** Authenticated request values and existing rows are still
   untrusted persisted data. Store-owned parameterized SQL, checked conversions, strict ID
   decoding, transaction ownership, uniqueness/check constraints, and typed replay decoding
   control it.

### Existing boundaries widened

The remote execution routes now accept an incarnation ID and enforce a stronger identity
tuple. Worker protocol operations are not widened to the network; the agent is a new
supervisor using the existing loopback transport. The node CLI reads new typed fields but
does not gain mutation authority.

### Actors and trust

The control plane trusts an authenticated logical node credential to supersede that node's
current process lifetime and declare its workers. It does not trust request shape, worker
ownership claims, child output, network availability, or persisted rows. The local operator
is trusted to choose executables and grant them the declared operations. Other network
hosts, old agent processes, and child processes are untrusted across their stated
boundaries.

### Explicitly out of scope

Operating-system sandboxing, same-user process isolation, credential rotation, mutual TLS,
cross-node byte transfer, and compromised-host containment are not added here. A process
running as the agent's operating-system user may inspect that user's memory or files; host
service isolation is an operator responsibility. The bearer credential plus server TLS is
the accepted #416 transport contract.

## Verification

- Migration tests prove empty, existing-node/worker, corruption, uniqueness, and terminal
  check constraints, including checked incarnation decoding.
- Store/control-plane tests prove activation replay, atomic rollback, supersession ordering,
  stale fencing, worker ownership fencing, heartbeat updates, deactivation replay, logical
  retirement, and stale-node failure ordering.
- API tests prove strict request bodies, authentication, response envelopes, unknown-field
  rejection, required incarnation fields on all remote routes, and end-to-end replay.
- Agent unit tests prove config bounds, URL/CA rules, secret redaction, child environment,
  readiness bounds, loopback rejection, exact handshake/identity, backoff, request-key reuse,
  frame validation, and shutdown classification.
- Agent integration tests use a real loopback API router and out-of-process echo worker to
  prove activation, polling, independent heartbeat during silent progress, completion,
  restart supersession, child crash, and graceful deactivation.
- CLI envelope snapshots prove current and historical incarnation inspection without
  secrets.
- `just ci` is the final local guardrail. A live smoke proof launches `voom-api`, an echo
  worker through `voom-node-agent`, and a synthetic probe ticket against an ephemeral SQLite
  database, then inspects the terminal lease and incarnation history.
