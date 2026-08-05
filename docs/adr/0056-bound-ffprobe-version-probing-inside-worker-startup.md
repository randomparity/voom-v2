# ADR 0056: Bound ffprobe version probing inside worker startup

## Status

Accepted

## Context

ADR 0034 requires the bundled ffprobe worker to finish `ffprobe -version` before it
advertises readiness and requires every timeout path to terminate and reap its child. The
version probe currently allows one second while its supervisor allows five seconds for the
whole worker to bind. Parallel CI has shown that a responsive host ffprobe can exceed the
inner second under scheduler load, producing a false dependency failure before policy
eligibility is evaluated.

The policy-preflight aggregation test also reaches the production host probe. Its intent is
to prove persisted deny handling and ordered aggregation, not host media-tool availability.
Changing process-global environment for that test would create a different concurrency
hazard, while checking eligibility before readiness would change ADR 0034's failure order.

## Decision

The production ffprobe version probe receives a four-second deadline. The existing
five-second bundled-worker startup deadline remains the outer bound, leaving one second for
the worker to report a dependency failure or bind and advertise readiness. The version
probe keeps ownership of timeout termination and reaping; the timeout error continues to
name the executable and elapsed budget.

The ffprobe crate parameterizes the version timeout only at its private configuration
boundary. Public constructors always select the four-second production value. Tests call
the private boundary with a short duration to prove timeout, kill, reap, and diagnostics
without paying the production delay.

Policy tool preflight keeps metadata order and still verifies bundled ffprobe readiness
before reading or repairing built-in eligibility. A private async readiness callback is
threaded through that existing step. Production supplies the real bundled-worker probe;
the aggregation test supplies a successful in-memory result and asserts the callback was
used. No process-global environment is changed.

## Consequences

Ordinary scheduler stalls have four times the former tolerance, while a hung version child
still fails before the supervisor's outer startup deadline. The two constants remain in
their owning crates rather than becoming a new public timing contract; a focused test and
this record make their ordering explicit. A machine unable to schedule the version probe
and finish worker binding within five seconds still fails readiness by design.

The aggregation test no longer proves the real ffprobe executable path. Existing
ffprobe-worker configuration and bundled-worker readiness tests retain that coverage, and
the aggregation test becomes deterministic evidence for the policy diagnostic it owns.

## Considered & rejected

- Keep the one-second version deadline and only hermeticize the aggregation test. This
  removes one flaky witness but leaves the observed production false-negative unchanged.
- Raise the version deadline to the full five seconds or beyond. The supervisor would win
  first and replace the actionable dependency timeout with a generic bound-address timeout,
  while the inner child might not complete its own cleanup path.
- Read persisted deny eligibility before starting ffprobe. This makes the test cheap by
  changing production failure order and weakening ADR 0034's fresh-readiness prerequisite.
- Install a fake ffprobe through the process environment in the test. Environment mutation
  is process-global and races other tests; it does not isolate the unit at its actual
  readiness boundary.
- Add a reusable observer trait or public timeout configuration. There is one production
  probe and one test substitution, so a private generic callback and private duration
  parameter solve the problem without a new abstraction or operator surface.
