# ADR 0064: Center node-agent periodic jitter on the existing interval

## Status

Accepted

## Context

Node agents sleep for fixed acquisition-poll, node-heartbeat, and lease-heartbeat
intervals. Agents started together or released from a shared control-plane outage therefore
remain phase-locked even though control-plane request retries now use full jitter. The three
schedules have different authority and deadline constraints: polling is locally configured,
node heartbeats must precede the incarnation TTL, and lease heartbeats must precede the
control-plane-granted lease TTL.

Jitter must disperse a fleet without increasing its mean periodic request rate. It must also
leave heartbeat deadline margin and preserve prompt shutdown while a sampled delay is pending.

## Decision

Each periodic sleep samples uniformly from 50% through 150% of its existing interval. The
range is centered on the existing interval, so its expected delay and mean request rate remain
unchanged. A private duration sampler accepts an injected random-number generator so seeded
unit tests can prove the bounds without wall-clock randomness. Production tasks each seed an
independent RNG stream from operating-system entropy once and reuse it across cycles; shared
seeds would preserve the fleet phase this decision exists to break.

Acquisition polling uses the configured poll interval as its center. Its pending sleep selects
against shutdown, so a sampled upper bound does not worsen shutdown responsiveness.

Node heartbeats use the existing `TTL/3` interval as their center. The runtime's existing
minimum effective TTL is at least twice that interval, while the latest sample is at most
one-and-a-half intervals, leaving at least half an interval for the request before local
self-fencing.

Lease heartbeats use the granted `heartbeat_after` interval as their center only when the
latest sample remains strictly before the granted TTL. An incoherent grant without that
margin retains the existing exact interval and fail-closed fencing behavior; jitter must not
make malformed authority look healthy by sending earlier than specified. Stop and shutdown
watch channels continue to interrupt heartbeat sleeps.

## Consequences

Fleet-identical periodic work no longer retains one exact phase. Individual samples can be
shorter or longer than before, but the symmetric distribution preserves the existing expected
request rate. Randomness changes only timing; request bodies, TTL values, identities, retry
semantics, and public configuration remain unchanged.

The latest valid heartbeat samples retain deadline margin. Scheduler delay and request time
can still consume that margin, so the existing TTL timeout and fencing paths remain
load-bearing. Degenerate or incoherent lease grants deliberately receive no jitter and fence
as before.

No persisted state or wire contract changes, and migration 0042 is unused. Reverting this
decision restores fixed periodic timing.

## Considered & rejected

- **Retain exact periodic intervals:** has no implementation risk, but preserves synchronized
  fleet waves after shared starts and recovery and therefore does not meet issue #459's outcome.
- **Full jitter from zero through the existing interval:** disperses traffic but halves the
  expected delay and approximately doubles periodic request load.
- **Delay-only jitter from the interval through twice the interval:** never sends earlier, but
  lowers mean request rate and spends too much of heartbeat TTL budgets.
- **One random startup phase followed by fixed intervals:** preserves long-run request rate,
  but agents can re-lock after shared outages and simultaneous successful responses.
- **Operator-configurable jitter:** adds configuration and deployment combinations without an
  operator requirement; the timing budgets are runtime invariants rather than policy knobs.
