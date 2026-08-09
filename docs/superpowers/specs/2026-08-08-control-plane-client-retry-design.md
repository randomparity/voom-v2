# Control-Plane Client Retry Design

Issue: #448
Decision: [ADR 0059](../../adr/0059-bounded-jittered-control-plane-client-retries.md)

## Scope authority

- Scope identity: issue #448 plus `work-448-c7e2b1`.
- Outcome: bound client retries at the source, require an explicit unlimited choice, and
  disperse fleet-identical retry timing.
- Sources: issue #448 body and owner comment, the current campaign assignment, and the
  repository instructions.
- Exclusions: fixed-interval poll and heartbeat jitter, issue #452, migrations, new
  configuration, and files outside the assigned surface.
- Interaction: unattended campaign subagent.

## Current behavior

`ControlPlaneClient` stores `RetrySettings` with an optional maximum attempt count.
`from_config` selects `None`, so `send` retries transport failures and HTTP 408, 429, or 5xx
responses forever. The frozen request object already preserves the serialized body,
idempotency key, and request hash across attempts. Non-retryable responses and malformed or
oversized envelopes already return immediately.

The retry delay doubles from 250 milliseconds to a 30-second ceiling without jitter. All
agents use the same sequence, so a fleet-wide failure aligns their recovery attempts.

## Approaches

### Selected: bounded constructor default plus explicit unlimited constructor

The production constructor selects five attempts. A separately named constructor selects
unlimited attempts, making that exceptional choice visible at the construction call site.
The retry loop adds full jitter while continuing to double the ceiling independently of the
sampled sleep.

This keeps one retry implementation, adds no config field, and changes no operation wrapper
or wire contract.

### Rejected: one overall deadline per request

A deadline describes elapsed time more directly, but each attempt can consume the existing
30-second request timeout. Wrapping the whole loop introduces cancellation ordering around
an in-flight HTTP request and competes with the shorter domain deadlines in `runtime.rs`.

### Rejected: configurable retry policy

Operator configuration would make the bound adjustable, but no requirement needs it. It
also permits deployments to restore the unsafe default and expands validation and docs.

## Detailed design

`RetrySettings` retains its optional attempt count as a private implementation detail.
`from_config` passes `Some(5)`; `from_config_with_unbounded_retries` passes `None`. The
existing private settings constructor remains the deterministic unit-test seam.

On a retryable failure, `send` checks the attempt limit before sleeping. Exhaustion returns
`VoomError::ExternalSystemUnavailable` containing the number of attempts and the last error
message. It does not include a request body, token, or response body. Terminal errors return
unchanged and do not gain retry-exhaustion wrapping.

When another attempt is allowed, a small helper samples a uniform duration in
`0..=delay_ceiling`. `send` sleeps for that sample, then doubles `delay_ceiling`, capped at
30 seconds. Sampling does not influence the next ceiling.

The public operation methods continue to call `send`. No production path opts into unlimited
retrying, and `runtime.rs` keeps every existing domain-specific timeout and force escape.

## Failure behavior

- Transport, HTTP 408, HTTP 429, and HTTP 5xx failures consume one attempt.
- The final retryable failure returns an actionable exhaustion error immediately.
- HTTP 4xx responses other than 408 and 429 remain terminal on the first attempt.
- Invalid envelopes, response-read failures, and oversized bodies keep their current terminal
  behavior.
- A zero jitter sample is valid; the attempt cap still prevents a tight infinite loop.
- Random sampling cannot increase the sleep beyond the current ceiling.

## Security and trust boundaries

The design adds no network boundary and widens no caller permissions. The existing local
operator supplies the control-plane URL, CA path, and node token; the remote control plane
controls status codes and response bytes. Existing URL-origin checks, TLS validation,
response-size bounds, strict envelope decoding, and bearer authentication remain unchanged.

The new exhaustion path controls disclosure by reporting only attempt count and the existing
sanitized error. It never formats the token, frozen request body, or remote response body.
Unlimited retrying is an explicit local API decision, not a value the remote service can
select. Denial-of-service behavior caused by fixed poll and heartbeat intervals remains out
of scope and is owned by a separate follow-up.

## Testing

Unit tests will prove:

1. the production constructor carries a finite default;
2. retryable statuses and transport failures stop exactly at the configured attempt count
   with an actionable error;
3. the explicit unlimited constructor continues past the production limit;
4. full-jitter samples stay within the ceiling and vary under a seeded generator;
5. body and idempotency-key reuse survives all retry classes; and
6. terminal responses still perform one attempt.

The TLS integration test will use the explicit unlimited constructor for its existing claim
that certificate failures remain retryable. `cargo test -p voom-node-agent`, `just lint`, and
`just ci` provide crate, workspace, and full guardrail evidence.

## Success criteria

- Production retrying is finite without caller cooperation.
- Unlimited retrying appears only at an explicitly named construction call site.
- Retry exhaustion identifies the attempt count and last failure class without leaking
  credentials or request data.
- Fleet retries use full jitter while preserving exponential ceiling growth.
- Existing request replay and terminal-response contracts remain green.
- `just ci` passes with no skipped checks.
