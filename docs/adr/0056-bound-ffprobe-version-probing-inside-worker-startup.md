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

The production ffprobe version probe receives a four-second deadline. Its supervisor uses a
named ffprobe-specific outer budget derived from that deadline plus five seconds of process
startup, scheduling, error-propagation, and bind coordination allowance. Both durations and
their nine-second sum live in the shared worker-startup vocabulary, and a unit test asserts
the derivation and strict ordering. Other bundled workers retain their existing five-second
startup budget.

The version probe keeps ownership of timeout termination and reaping; the timeout error
continues to name the executable and elapsed budget. A nested worker-process regression
delays the worker before it starts the version probe, then hangs a PID-recording ffprobe. It
must observe the inner dependency diagnostic and the reaped ffprobe PID before the derived
outer deadline.

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

Ordinary scheduler stalls have four times the former version-probe tolerance, while the
supervisor preserves the former five-second allowance around that inner work. A hung version
child therefore has a tested interval in which to emit its actionable dependency error and
finish kill/reap before the supervisor can replace it with a generic bound-address timeout.
A machine unable to start, probe, and bind within the derived nine seconds still fails
readiness by design; no finite process timeout claims to survive unbounded host starvation.

The aggregation test no longer proves the real ffprobe executable path. Existing
ffprobe-worker configuration and bundled-worker readiness tests retain that coverage, and
the aggregation test becomes deterministic evidence for the policy diagnostic it owns.

## Considered & rejected

- Keep the one-second version deadline and only hermeticize the aggregation test. This
  removes one flaky witness but leaves the observed production false-negative unchanged.
- Raise only the version deadline inside the existing five-second outer timeout. Worker
  scheduling consumes an unknown portion of that margin, so the supervisor can win first,
  replace the actionable dependency timeout, and interrupt inner cleanup.
- Read persisted deny eligibility before starting ffprobe. This makes the test cheap by
  changing production failure order and weakening ADR 0034's fresh-readiness prerequisite.
- Install a fake ffprobe through the process environment in the test. Environment mutation
  is process-global and races other tests; it does not isolate the unit at its actual
  readiness boundary.
- Add a reusable observer trait or operator timeout configuration. There is one production
  probe and one test substitution, so a private generic callback and private duration
  parameter solve the injection problem without a configurable surface. Shared named timing
  constants are still required because two process owners enforce the nested deadline.
