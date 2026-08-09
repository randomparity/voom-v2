# ADR 0059: Bound and jitter control-plane client retries by default

## Status

Accepted

## Context

The node agent replays frozen, idempotent control-plane requests after transport failures
and HTTP 408, 429, or 5xx responses. Its production constructor currently gives every
operation an unlimited retry count. Callers therefore add their own deadlines or shutdown
escapes, while `acquire`, `complete`, `fail`, and `deactivate` can still wait forever. A new
call site inherits the unlimited behavior unless its author notices that hidden default.

Every agent also uses the same exponential delay sequence. A fleet affected by one outage
therefore retries in phase and sends synchronized load to a recovering control plane.

## Decision

`ControlPlaneClient::from_config` permits five attempts per request. After the fifth
retryable failure, the client returns `ExternalSystemUnavailable` with the attempt count and
the last failure. Terminal responses, response decoding, frozen request bodies, and
idempotency keys keep their existing behavior.

Unlimited retries are available only through a constructor named
`from_config_with_unbounded_retries`. No production call site uses it. The explicit
constructor keeps the exceptional behavior testable and visible without adding an operator
configuration setting or a per-operation flag.

Before each retry, the client sleeps for a uniformly sampled duration from zero through the
current delay ceiling. The ceiling, not the sampled duration, doubles after each failure and
stops at 30 seconds. This is full jitter: agents that fail together do not retain one shared
retry phase.

Existing activation, heartbeat, settlement, and shutdown bounds remain. They express
shorter domain deadlines and cancellation behavior, while the client-level attempt limit is
the last-resort default for every current and future operation.

## Consequences

A control-plane outage becomes a visible error after at most five request attempts instead
of parking a caller forever. With the existing 30-second request timeout, the theoretical
worst case is about 154 seconds: five request timeouts plus four sleeps at ceilings from
250 milliseconds through two seconds. That remains longer than domain-specific lease and
shutdown budgets, so those guards remain load-bearing.

Current `acquire`, terminal-settlement, and heartbeat callers classify an exhausted request
as fatal. The runtime then fences and reaps its children, exits without a deactivation
write, and relies on incarnation or lease expiry plus the service supervisor's normal
process-restart policy. This fail-stop recovery is intentional: continuing after the shared
client has established prolonged control-plane unavailability would let a node operate
against authority it can no longer reach.

Full jitter may choose a near-zero delay, so a single agent can retry sooner than the former
fixed schedule. Across a fleet, the uniformly distributed attempts trade that per-agent
variance for lower synchronized recovery load. Reverting this decision restores the former
unbounded, phase-locked behavior without changing persisted data or wire contracts.

The explicit unlimited constructor can still be misused. Its name makes that choice visible
in review, and no configuration can enable it accidentally.

## Considered & rejected

- Bound each call by one overall wall-clock deadline. This directly expresses elapsed time,
  but it adds another cancellation race around in-flight requests and overlaps the tighter
  lease, heartbeat, and shutdown deadlines already owned by callers.
- Expose attempt count and jitter as operator configuration. No operator requirement calls
  for tuning them, and a new setting would turn a safe default into a deployment matrix.
- Keep unlimited retries and require every call site to supply a deadline. That is the
  current failure mode: omitted guards silently inherit an infinite wait.
- Remove unlimited retries entirely. This is the smallest production surface, but the
  campaign acceptance criteria require an explicit, tested opt-in path.
- Use deterministic exponential backoff. It bounds one caller but leaves a fleet
  phase-locked after a shared outage.
