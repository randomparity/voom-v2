# Node-Agent Periodic Schedule Jitter Design

Issue: #459
Decision: [ADR 0064](../../adr/0064-centered-jitter-for-node-agent-periodic-schedules.md)

## Scope authority

- Scope identity: issue #459 plus token `B28CB600-63B5-452F-96D7-47EEBE366B4F`.
- Outcome: disperse acquisition-poll, node-heartbeat, and lease-heartbeat schedules across
  otherwise phase-locked node agents.
- Completion criteria: each schedule stays within its own safety budget; mean periodic
  request rate does not increase; lease and incarnation TTL fencing remains valid; shutdown
  interrupts every sampled sleep; deterministic tests prove bounds and interruption.
- Provenance: public issue #459, related issue #448, and the campaign assignment reserving
  ADR 0064 and migration 0042.
- Exclusions: configuration and public API changes, persistence/schema changes, migration
  0042, unrelated retry backoff or worker runtime changes, and deferral records.
- Surface: `crates/voom-node-agent/src/runtime.rs`, its sibling unit tests, directly required
  in-crate timing helpers, this spec, its implementation plan, ADR 0064, and one ADR index row.
- Ambiguities: none; the issue explicitly permits distinct semantics for the three loops.
- Interaction: unattended campaign subagent.

## Current behavior and root cause

Each coordinator sleeps for exactly `poll_interval` after an idle or terminal acquisition.
The node heartbeat task sleeps for exactly the derived `heartbeat_interval`, currently
`max(TTL / 3, 1 second)`. Each held lease sleeps for exactly the control-plane-granted
`heartbeat_after_seconds`. A fleet receiving the same values and starting or recovering
together therefore sends each class of periodic request in synchronized waves.

Client retry jitter from issue #448 cannot break these phases after requests succeed: every
successful loop immediately returns to its identical fixed periodic interval. The direct
poll sleeps also do not currently select against shutdown, so simply allowing longer jittered
poll delays would worsen shutdown responsiveness.

## Approaches

### Selected: centered per-cycle jitter with loop-specific deadline checks

Every cycle samples from a symmetric `[interval / 2, interval + interval / 2]` range. The
midpoint remains the existing interval, preserving expected request rate while successive
samples break phase lock. Polling uses the range directly. Node heartbeats use it because the
effective incarnation TTL is at least twice their base interval. Lease heartbeats use it only
when the upper bound is strictly below the granted TTL; otherwise they retain the exact
granted interval and existing fail-closed behavior.

One private nanosecond-resolution sampler accepts `&mut impl Rng`. It samples the full `u128`
nanosecond range represented by `Duration` and reconstructs seconds plus subsecond nanoseconds;
it does not narrow accepted lease grants through `u64` nanosecond saturation. Three small
loop-specific functions state the distinct policies and call the sampler. Production tasks
seed their own `StdRng` from the operating system once, then reuse it across cycles. Seeded
production-loop seams make the sampled deadlines observable without public configuration.

### Rejected: full jitter below the existing interval

Sampling from zero through the interval follows the retry-backoff precedent, but periodic
traffic differs from failure recovery: it cuts the mean delay in half and raises fleet request
load. Near-zero heartbeat samples also perform needless writes.

### Rejected: one startup offset

A one-time offset preserves the ongoing interval, but shared outage recovery can align agents
again after their request retries complete together. Per-cycle sampling continually disperses
successful loops.

### Rejected: runtime timing abstraction or configuration

A scheduler trait or new jitter settings would add public and test surface for three private
Tokio loops. Injecting only the random generator into pure private functions is enough for
deterministic proof.

## Detailed design

`centered_jitter(interval, rng)` converts the lower and upper duration bounds to saturated
nanoseconds and uniformly samples the inclusive range. Poll, node-heartbeat, and
lease-heartbeat functions make the authority and budget visible at their call sites instead
of hiding all three policies behind one generic schedule type.

The acquisition coordinator creates one RNG and samples after every idle or terminal result.
A private poll wait selects the sampled sleep against `shutdown.changed()` and the existing
child-exit future. If either control event wins, the coordinator routes it through the same
settlement or restart handling used when the event arrives during acquisition. The sampled
upper bound therefore does not extend shutdown or child-failure detection latency.

The node-heartbeat task creates one RNG and samples immediately before every select. Its base
interval remains `heartbeat_interval(TTL)`, and its effective TTL remains
`max(advertised TTL, 2 * interval)`. The maximum sample is `1.5 * interval`, leaving at least
`0.5 * interval` before the local deadline. The existing timeout uses the remaining deadline
budget and preserves self-fencing.

The lease-heartbeat task derives its base interval and granted TTL exactly as today. It uses
centered jitter only when the upper bound is strictly below the TTL. If not, it sleeps for the
base interval exactly, preserving the current prompt fence for malformed or incoherent grants.
Its stop watch remains in the same `select!`, so settlement and shutdown interrupt the sample.

## Failure behavior and boundaries

- A poll RNG sample cannot prevent shutdown: the shutdown watch wins the poll wait.
- A node heartbeat delayed past its remaining TTL still follows the existing local expiry and
  fatal path; jitter does not relax the deadline.
- An unreachable lease-heartbeat request still fences at the granted TTL.
- A non-positive granted heartbeat interval or TTL keeps the existing one-second
  normalization and fail-closed behavior.
- Random-source initialization uses the already-required `rand` dependency. No new dependency,
  input parser, secret, authorization decision, or network boundary is introduced, so the
  security-specific review arm is not triggered.

## Testing

Seeded unit tests will prove that:

1. poll samples span both sides of the configured interval and stay within 50% through 150%;
2. node-heartbeat samples stay in the centered range and strictly inside the effective TTL;
3. coherent lease-heartbeat samples stay centered and before the granted TTL;
4. incoherent lease grants retain the exact interval;
5. intervals beyond `u64::MAX` nanoseconds preserve centered bounds and mean rather than
   saturating to a shorter delay;
6. independent seeded streams do not produce one fixed schedule;
7. the production acquisition-poll wait consumes successive seeded samples, and shutdown and
   child exit each interrupt a maximum sampled sleep without advancing paused time; and
8. seeded production node- and lease-heartbeat loops observe at least two successive sampled
   deadlines while retaining stop-channel interruption and TTL fencing.

Existing paused-time tests continue proving unreachable requests fence at the exact TTL. The
focused crate suite, workspace lint, and `just ci` provide regression evidence. Bite proof
temporarily restores the former fixed delay independently in each production poll, node, and
lease loop while retaining the new tests; the corresponding test must fail in each arm before
the sampled implementation is restored.

## Success criteria

- The three normal periodic schedules no longer use one fixed interval on every cycle.
- Every sample remains inside its documented loop-specific timing budget.
- The distribution midpoint remains the former interval, so expected request rate is unchanged.
- Shutdown remains able to interrupt all periodic sleeps.
- TTL values, fencing deadlines, request data, and public configuration remain unchanged.
- Focused tests and `just ci` pass with no unexpected skips or warnings.
