# Node-agent shutdown consistency design

## Scope and authority

Issue #449 requires three corrections inside the node-agent runtime: preserve genuine
second-signal forcing when a fatal exit races the first signal, handle the lease-settlement
outcome in production, and validate lease freshness against the TTL granted in the
dispatch. The operator acceptance for #449 additionally requires deterministic concurrency
coverage, runbook updates where semantics change, and preservation of shutdown mutation,
event, and failure ordering.

The implementation is limited to `runtime.rs`, its sibling unit tests, and the node-agent
operator runbook. ADR 0060 records the signal-phase, settlement-outcome, and granted-TTL
decisions. No schema, migration, configuration, API, or cross-crate change is needed.

## Current failure modes

The runtime chooses one first exit among a termination signal, a fatal report, and a
coordinator join. The graceful branch consumes the first signal. Fatal and
restart-exhausted branches do not, yet `wait_for_coordinators` presently enables force
unconditionally. If fatal wins while the first signal is buffered, the reaping loop reads
that buffer entry and broadcasts `Forced`, aborting settlement after only one signal.

`settle_leases_for_shutdown` returns whether forced shutdown interrupted settlement. Its
coordinator caller ignores the value, so the coordinator exit cannot describe whether
leases completed settlement or were aborted.

`validate_lease` times and classifies heartbeat validation with configured `lease_ttl`,
while the ongoing heartbeat fence uses `dispatch.lease_ttl_seconds`. When the control plane
grants a different TTL, validation applies a deadline that does not govern the held lease.

## Selected design

### Explicit signal phase

Introduce a private two-state signal phase:

- `AwaitingFirst`: no termination signal has been consumed for this shutdown. Receiving a
  signal advances to `ForceEnabled` and does not broadcast force.
- `ForceEnabled`: the first signal was already consumed. Receiving a signal broadcasts
  `ShutdownKind::Forced` and records a forced outcome.

`RuntimeExit::Graceful` starts coordinator reaping in `ForceEnabled`; fatal and
restart-exhausted exits start in `AwaitingFirst`. A closed signal source disables further
signal handling without spinning. Coordinator reaping still completes every coordinator
unless genuine force causes its lease tasks to abort through the existing watch channel.

This phase is state, not a timing heuristic. A signal buffered before reaping and a signal
delivered later during reaping follow the same transition.

### Typed settlement outcome

Replace the boolean settlement result with a private `LeaseSettlement` outcome containing
`Completed` and `Forced`. `CoordinatorExit::Shutdown` carries that outcome. The shutdown
branch follows the existing order:

1. publish lease cancellation;
2. settle leases or abort them after forced shutdown;
3. shut down and reap the child;
4. return the typed coordinator outcome.

`wait_for_coordinators` observes shutdown outcomes and aggregates `Forced`. The top-level
runtime still stops node heartbeat only after all coordinators finish, then preserves the
existing exit classification and deactivation ordering. Fatal errors remain fatal; a
forced graceful or restart-exhausted shutdown remains unsuccessful.

### Granted-TTL validation

For an acquired dispatch, normalize `dispatch.lease_ttl_seconds` to at least one second,
using the same checked conversion as the heartbeat loop. Use half that granted TTL for
each validation request timeout and the full granted TTL for the elapsed freshness check.
The configured TTL continues to populate the acquire request only.

## Alternatives

Draining buffered signals before reaping is insufficient because it distinguishes timing,
not semantic phase: a first signal arriving just after the drain would still force. Gating
force on graceful exit avoids the race but restores an unbounded fatal-settlement hang.

Dropping the unused settlement boolean would remove dead data, but it would also erase a
meaningful outcome already known at the coordinator boundary. Carrying a small enum makes
the forced path explicit without changing public interfaces.

## Ordering invariants

The change must not reorder any durable or process-lifecycle step:

- cancellation is published before lease settlement;
- lease complete/fail attempts finish before child shutdown unless genuine force aborts
  them;
- every child is shut down and reaped before its coordinator returns;
- all coordinators finish before node heartbeat stops;
- graceful deactivation occurs only after reap and heartbeat stop;
- fatal exit propagation remains after heartbeat stop and does not add deactivation;
- existing lease mutation and event ordering inside `settle_lease` is unchanged.

## Error handling and observability

A genuine second signal continues to return the existing actionable forced-shutdown error
where that error is observable. A fatal exit retains its original fatal error even if a
second signal accelerates settlement, so force does not mask the root cause. No new logs or
public error codes are introduced. The typed internal outcome is observable through tests
and through the runtime's existing forced result aggregation.

The operator runbook will state that the first signal never forces settlement, including
when an internal fatal or restart-exhausted exit began shutdown concurrently. A second
signal is always required to abandon settlement or deactivation.

## Testing

Tests will prove the change bites before implementation:

1. Buffer one first signal while a fatal-style shutdown is reaping a blocked coordinator.
   Prove the shutdown watch remains fenced and settlement is not forced; then deliver a
   second signal and prove force is broadcast and reaping completes.
2. Prove graceful reaping, whose first signal was already consumed, still forces on the
   next signal.
3. Prove a coordinator surfaces `LeaseSettlement::Forced` after forced lease abortion and
   that coordinator reaping aggregates that outcome.
4. Give validation a dispatch TTL different from configured TTL and a response whose delay
   is fresh only under the grant. Prove validation accepts it. Existing stale-attempt,
   heartbeat-fence, terminal-response, shutdown-order, and second-signal tests remain green.

Focused verification is `cargo test -p voom-node-agent`; the repository guardrail is
`just ci`.

## Security relevance

The change does not add or widen a route, credential, permission, parser, network exposure,
or dependency. It changes local signal and timeout decisions for already-authenticated
control-plane traffic. A diff-scoped threat scan is therefore not triggered by the
workflow's security criteria; the adversarial concurrency review remains required.

## Rollback

Rollback is a code revert. There is no persisted format, migration, deployment ordering,
or compatibility transition.
