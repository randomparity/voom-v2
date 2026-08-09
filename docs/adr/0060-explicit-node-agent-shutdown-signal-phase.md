# ADR 0060: Track node-agent shutdown signal phase explicitly

## Status

Accepted

## Context

The node agent begins ordered shutdown after a termination signal, a fatal coordinator
exit, or restart exhaustion. A second termination signal may abandon blocked lease
settlement or deactivation. The graceful path consumes the first signal in the runtime's
top-level selection, but fatal and restart-exhausted paths do not.

The coordinator-reaping loop currently treats every signal it receives as force. If a
fatal exit wins the selection while the first signal is already buffered, that first signal
is reinterpreted as a second signal. Lease tasks are aborted before terminal settlement,
even though the operator sent only one signal. The lease-settlement helper also reports
whether force interrupted it, but its production caller discards that outcome.

## Decision

Shutdown carries an explicit signal phase into coordinator reaping. A graceful exit enters
the phase where the next signal forces because it already consumed the first signal. Fatal
and restart-exhausted exits enter the phase where one signal remains to be consumed without
forcing. Only a later signal advances from force-enabled to forced and broadcasts the
forced shutdown kind.

Lease settlement returns a typed completed-or-forced outcome. A coordinator includes that
outcome in its shutdown exit, and the coordinator-reaping loop aggregates forced outcomes
instead of discarding them. Lease settlement still precedes child shutdown and reap;
heartbeat stop and fatal propagation or deactivation retain their existing order.

Lease validation derives both its per-attempt timeout and freshness check from the lease
TTL in the acquired dispatch. It applies the same minimum-one-second normalization as the
lease heartbeat loop. Local configured TTL remains only the requested TTL for acquisition;
the server grant governs the lease after acquisition.

## Consequences

A first signal never becomes force merely because a fatal exit won a concurrent selection.
Operators must send a genuine second signal to abandon settlement on every exit path.
Fatal shutdown may therefore remain blocked on settlement after one signal, as documented,
until settlement completes or a second signal arrives.

Coordinator exit values become more descriptive, but no persisted state, API, schema, or
event changes. No migration is required. Invalid non-positive granted TTL values continue
to fail safe at a one-second local window, matching heartbeat behavior.

## Considered & rejected

- Drain currently buffered signals before enabling force. This handles only a signal that
  arrived before the drain; a first signal arriving later during fatal settlement would
  still be mistaken for force unless the phase remained explicit.
- Enable force only for graceful shutdown. Fatal settlement can block on an unreachable
  control plane too, so this recreates the hang fixed by the original second-signal escape.
- Remove the lease-settlement return value. That eliminates dead data but hides whether a
  coordinator abandoned terminal settlement; propagating the typed outcome preserves the
  information already computed and lets the runtime aggregate it.
- Keep validation on configured TTL. The control plane, not local configuration, owns the
  granted expiry deadline; using two TTLs for the same held lease creates inconsistent
  freshness and heartbeat decisions.
