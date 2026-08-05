# ADR 0054: The control-plane API process owns TLS and graceful draining

## Status

Accepted

## Context

Issue #416 requires the existing Axum control-plane routes to become a production-safe
remote-node service. ADR 0050 already requires authenticated, encrypted agent traffic,
while the current `voom-api` crate owns only a router. The process boundary must reject an
unsafe transport before binding, bound untrusted requests, preserve bearer authorization,
and stop without abandoning accepted work.

The operator selected one explicit development exception: cleartext HTTP is permitted only
when the configured bind address is loopback. Every non-loopback deployment is HTTPS-only.

## Decision

`voom-api` gains its own long-running binary and keeps listener, TLS, request-boundary, and
shutdown ownership in that crate. The binary opens the existing database through
`ControlPlane::open` and `HealthPlane::open`; it never initializes or migrates storage.

The process uses Rustls through `axum-server`. TLS certificate and private-key PEM paths are
required unless the operator explicitly selects loopback cleartext. Certificate trust stays
standard server authentication: clients configure their trusted CA and existing node bearer
tokens continue to authorize execution routes. The server does not add client-certificate
authentication.

Configuration is fail-fast. Cleartext on a non-loopback bind, a partial TLS file pair, an
unreadable or malformed PEM, a zero bound, or an unavailable database prevents the listener
from starting with an actionable diagnostic. The safe default bind is loopback.

The router applies fixed production bounds: a 1 MiB request body, 30-second TLS handshake,
request-head, and request-processing deadlines. Axum keeps the existing JSON envelopes and
route idempotency behavior. The existing `/health` route is the readiness report: it returns
success only for a current database schema.

SIGINT and SIGTERM stop acceptance and allow in-flight requests up to a 30-second grace
period. Startup, listening, and shutdown diagnostics go to stderr through `tracing`; stdout
remains unused. Diagnostics name operations and configuration fields but never include
certificate/key contents or bearer-token values.

## Consequences

Remote nodes can authenticate the server with configured CA trust and then use the existing
bearer-token boundary. Expired or untrusted certificates fail at the client TLS handshake;
operators must provision and renew certificates outside VOOM. Certificate hot reload,
rotation orchestration, and mutual TLS remain future work rather than parallel mechanisms.

The new process adds Rustls-serving and Tower middleware dependencies, but avoids a bespoke
Hyper accept loop and its connection-draining state machine. A process restart is required
after certificate replacement. Cleartext remains available for explicit loopback-only tests
and local operation, never as a remote deployment fallback.

## Considered & rejected

- Keep `voom-api` as a router library with no shipped server. This leaves the epic without
  an executable remote-node ingress and cannot satisfy issue #416's transport, lifecycle,
  or deployment acceptance criteria.
- Terminate TLS only in a reverse proxy. This would leave the shipped API process able to
  expose remote execution directly over cleartext and could not enforce the issue's
  non-loopback rejection contract.
- Build a custom `tokio-rustls` and Hyper accept loop. This offers finer control but
  duplicates connection tracking and graceful-drain behavior already provided by
  `axum-server`, increasing security-sensitive code without an acceptance benefit.
- Require mutual TLS. ADR 0050 requires encrypted authenticated traffic, but issue #416
  explicitly retains node bearer authorization and asks for server-certificate validation.
  Adding a second client-identity mechanism would expand the contract.
