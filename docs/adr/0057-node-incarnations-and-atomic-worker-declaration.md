# ADR 0057: Node incarnations atomically declare supervised workers

## Status

Accepted

## Context

ADR 0050 makes one pull-based node agent the trust boundary on each storage host. A
logical node remains stable across process restarts, while every agent process lifetime
has a fresh incarnation that fences messages from prior processes. The current remote API
authenticates only the logical node token. Worker rows are registered separately, and node
heartbeats can reactivate a stale node. Those contracts cannot distinguish two processes
holding the same logical-node credential, and they provide no atomic point at which a new
process supersedes the old process and takes ownership of its workers.

The agent must also survive a lost activation response. Registering workers through
independent calls would leave an unknown partial set after a timeout and would require a
reconciliation protocol before the agent could safely poll work.

## Decision

The agent generates one random 128-bit `NodeIncarnationId` when its process starts. It
sends that ID, its worker declarations, and one idempotency key in a single authenticated
activation request. In one SQLite transaction the control plane:

1. authenticates the durable logical node;
2. reserves or replays the activation idempotency key;
3. supersedes any current incarnation and retires its workers;
4. records the new active incarnation and places its ID on the logical-node row;
5. registers the declared workers, capabilities, and grants against that incarnation; and
6. stores the complete activation response for replay.

The response is the agent's sole source of worker IDs and epochs. A repeated request with
the same key and body replays it; the same incarnation under a different activation is a
conflict. Worker declarations use a bounded logical name, exact `OperationKind` values,
advertised artifact-access modes, and a positive parallelism limit. The control plane,
not the request, derives worker kind, durable names, capabilities, and grants.

Every later remote mutation carries the incarnation ID. The control plane accepts it only
when it equals the node's current active incarnation and the referenced worker belongs to
that incarnation. The incarnation ID is included in the server-side idempotency namespace,
so keys from different process lifetimes cannot collide. The existing `nodes.epoch` remains
the optimistic row epoch and is never used as the incarnation fence.

Supersession and worker retirement do not cancel leases already held by the prior
incarnation. The fence rejects that incarnation's later lease heartbeats and terminal
calls, so each held lease follows the existing TTL-expiry path before its ticket becomes
available again. This preserves the established retirement and lease-failure ordering and
its durable expiry evidence instead of adding a second cancellation path to activation.

A graceful or failed agent shutdown uses one authenticated, idempotent deactivation
request. It ends the incarnation, records a bounded reason, retires its workers, and clears
the node's active-incarnation pointer. A new activation supersedes a live predecessor.
Heartbeat expiry ends the active incarnation as failed before the logical node is marked
stale. A stale incarnation cannot heartbeat itself back to life; a restarted process must
activate a new incarnation.

The agent forces child workers to bind `127.0.0.1:0`, generates per-worker credentials,
performs the existing exact-version handshake and identity challenge, and rejects any
reported non-loopback endpoint. Worker operations remain authenticated by the existing
worker protocol. The node agent talks to the control plane over HTTPS, except that explicit
cleartext is accepted only for a loopback control-plane URL.

## Consequences

Activation is a larger transaction than worker-by-worker registration, but there is no
partially visible process lifetime for the agent to reconcile after a timeout. Starting a
second process with the same node credential deliberately fences the first process rather
than attempting leader election. Promotion is deliberately fail-stop: it fences a healthy
incumbent before the replacement has received worker IDs or proved its children ready. A
broken replacement can therefore cause immediate downtime; operators recover by fixing
and restarting it, which creates another fresh incarnation. Operators can inspect current
incarnation state on
`voom node show` and incarnation history with `voom node incarnation list`.

Existing synthetic remote callers must activate and send an incarnation ID. There is no
compatibility path that accepts unfenced remote mutations. Historical worker rows remain
readable but have no incarnation and cannot be acquired through the production remote
path.

This is a coordinated, pre-release flag-day cutover. Operators first quiesce legacy remote
callers, create and verify a pre-migration database backup, apply migration 0035 through
the supported `voom init` path, deploy and start the schema-35 control plane, and only then
start incarnation-aware agents. The control-plane process only connects and never applies
migrations. An older control-plane binary rejects schema 35 as too new, so rollback
requires restoring the verified pre-migration backup together with the older binaries;
mixing old remote callers or binaries with the migrated service is unsupported and fails
closed.

Loopback TCP plus worker credentials protects the child operation endpoint from the
network and unauthenticated operations, but it is not an operating-system sandbox. A
malicious process running as the same host user remains outside this decision's threat
model.

## Considered & rejected

- Use `nodes.epoch` as the process fence. Heartbeats advance that row epoch, so it is not a
  stable process identity and ADR 0050 explicitly reserves it for optimistic concurrency.
- Activate first, then register each worker through separate calls. A lost response leaves
  an ambiguous partial manifest and makes safe restart depend on a second reconciliation
  protocol.
- Prepare worker identities, verify children locally, then commit takeover. This reduces
  restart downtime when the replacement is broken, but adds a durable prepared state,
  expiry and cleanup semantics, credentials valid before ownership, and another replay
  transition. The pre-release synthetic-first lifecycle accepts one-phase fail-stop
  takeover instead of that protocol.
- Let the server generate the incarnation ID. A response lost after commit would hide the
  new fence from the process that owns it; client generation makes the retry body complete.
- Keep the synthetic runner as the production supervisor. It dispatches fake providers in
  process, couples heartbeat timing to polling and dispatch, and owns no child lifecycle.
- Bind children on configurable network addresses. The issue requires node-local child
  endpoints; widening them adds authentication and exposure risk without serving this
  issue's synthetic-first scope.
- Keep the current incarnation-less remote API. It cannot distinguish concurrent process
  lifetimes holding the same node token, so stale processes remain able to mutate current
  work and the issue's fencing requirement is unmet.
