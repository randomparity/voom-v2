# HTTPS control-plane API server design

Issue: #416

Status: Approved requirement, implementation pending

Governing decisions: [ADR 0050](../../adr/0050-node-owned-storage-and-byte-blind-control-plane.md)
and [ADR 0054](../../adr/0054-control-plane-api-process-owns-tls-and-draining.md)

## Scope authority

- Scope identity: `https://github.com/randomparity/voom-v2/issues/416#scope-07d48cef-6c5b-4eea-a0d9-0d5f816ee5e2`.
- Outcome: a dedicated control-plane API process for authenticated remote-node traffic,
  with production non-loopback operation protected by HTTPS.
- Provenance: public issue #416, accepted ADR 0050, and the operator's 2026-08-04
  campaign-checkpoint decision selecting explicit loopback-only cleartext.
- Permitted surface: `crates/voom-api/**`, necessary workspace dependency declarations and
  `Cargo.lock`, API integration tests, ADR 0054 and its index row, this spec and plan, and
  relevant operator documentation.
- Exclusions: no scheduling loop, broad REST API, database migration, alternate SQLite
  owner, route-use-case relocation, node-agent implementation, mutual TLS, certificate
  issuance, or certificate hot reload.
- Ambiguities: none.
- Interaction: interactive.

## Goal and success criteria

The `voom-api` package will produce a foreground `voom-api` binary. It opens an existing,
current VOOM database without creating or migrating it, constructs the existing
control-plane router, and listens until SIGINT or SIGTERM.

The change is successful when automated tests prove:

1. a client that trusts the configured CA completes TLS server authentication and reaches
   an existing node-authenticated route;
2. an untrusted CA, expired server certificate, invalid bearer token, and cleartext
   non-loopback configuration each fail at the boundary responsible for them;
3. request bodies, TLS handshakes, request heads, request processing, connection lifetimes,
   and shutdown draining are bounded;
4. an accepted in-flight request finishes within the grace period while new connections are
   no longer accepted, and forced cutoff leaves same-key retry as the recovery path;
5. route idempotency and error envelopes are unchanged;
6. bearer tokens and PEM key contents never appear in response bodies or process
   diagnostics; and
7. the process adds no scheduler loop and applies no migration.

## Approaches considered

### Rustls through `axum-server` — selected

Use `axum-server` for plain/TLS listeners, bounded TLS handshakes, listener readiness, and
graceful connection draining. Restrict Hyper to HTTP/1.1, disable persistent connections,
and wrap the post-handshake stream with a 90-second total connection deadline. Apply Tower
HTTP middleware to the existing Axum router and set Hyper's request-head deadline on the
server builder. This reuses maintained connection lifecycle code while leaving application
use cases unchanged.

### Direct Tokio-Rustls and Hyper accept loop

This would expose every connection and task boundary, but VOOM would have to implement and
test accept cancellation, TLS handshake deadlines, connection tracking, HTTP graceful
shutdown, and drain deadlines. It adds code at the most security-sensitive boundary without
changing the requested behavior.

### External TLS terminator only

This keeps the crate smaller, but the API process itself could still bind remote cleartext
and would not satisfy the required fail-closed transport contract. Operators may still use
an upstream proxy later, but it is not the server's only TLS boundary.

## Package and module shape

- `crates/voom-api/src/config.rs` owns Clap parsing and converts raw arguments into a
  validated `ServerConfig`. It never reads certificate or key contents.
- `crates/voom-api/src/server.rs` owns request bounds, listener construction, Rustls PEM
  loading, the post-handshake deadline stream, listening notification, and graceful
  draining. It accepts a router so tests can exercise timeout and shutdown behavior without
  test-only production routes.
- `crates/voom-api/src/lib.rs` keeps the shared envelope type and adds the explicit boundary
  adapter that replaces stock middleware 408/413 responses with API JSON envelopes.
- `crates/voom-api/src/main.rs` initializes stderr logging, resolves the existing database
  configuration, opens `HealthPlane` and `ControlPlane`, installs OS signal handling, and
  invokes the server. It contains no scheduling loop.
- Existing `execution.rs` continues to own route commands, bearer parsing, idempotency
  hashing, and use-case calls.

Unit tests remain sibling `*_test.rs` files. Integration tests under
`crates/voom-api/tests/` exercise a real TCP listener, TLS handshakes, process startup, and
the existing route surface.

## Public configuration contract

The binary accepts these fields:

| Field | Contract |
|---|---|
| `--database-url` / `VOOM_DATABASE_URL` | Existing database URL resolution; the process never initializes it. |
| `--bind` | Socket address, default `127.0.0.1:7443`. |
| `--tls-cert` | PEM certificate chain path; required with `--tls-key`. |
| `--tls-key` | PEM private-key path; required with `--tls-cert`. |
| `--allow-cleartext-loopback` | Explicitly selects HTTP and is valid only for a loopback bind; conflicts with TLS fields. |

Exactly one transport is selected: the complete TLS pair or explicit loopback cleartext.
Missing, partial, or conflicting transport input fails before bind. IPv4 and IPv6 loopback
addresses are accepted; unspecified, private, link-local, and public addresses are not
loopback and therefore reject cleartext.

Fixed limits deliberately avoid speculative public knobs:

- maximum request body: 1 MiB;
- TLS handshake deadline: 30 seconds;
- request-head deadline: 30 seconds;
- complete request-processing deadline: 30 seconds;
- total post-handshake connection deadline: 90 seconds; and
- graceful shutdown deadline: 30 seconds.

The existing route semantics remain the contract within those process bounds. A request
that exceeds the body limit receives HTTP 413. A request that exceeds the processing
deadline receives HTTP 408. Slow TLS or request-head peers are disconnected because no Axum
handler has run and therefore no JSON command envelope exists to preserve. The server
accepts HTTP/1.1 only and closes each connection after its one response. The outer
post-handshake deadline covers response production, socket write, and flush, so a client
that stops reading cannot retain the connection indefinitely. No unauthenticated peer can
remain between requests or open HTTP/2 streams outside these deadlines.

Stock Tower responses are not public API. A boundary-response adapter replaces the body
limit and request-processing timeout responses with `application/json` envelopes. Both use
the generic `api.request` command because these process-wide bounds execute outside the
route handler that owns a route-specific command. Their exact contracts are:

| HTTP | `error.code` | `error.message` | `error.hint` |
|---|---|---|---|
| 408 | `REQUEST_TIMEOUT` | `request processing exceeded the 30-second deadline` | `Retry a mutation with the same idempotency key if its outcome is unknown` |
| 413 | `PAYLOAD_TOO_LARGE` | `request body exceeds the 1048576-byte limit` | `Send a request body of 1048576 bytes or fewer` |

Each response has `schema_version: "0"`, `command: "api.request"`, `status: "error"`,
`data: null`, `warnings: []`, and the error object above. `REQUEST_TIMEOUT` and
`PAYLOAD_TOO_LARGE` are new centralized `voom_core::ErrorCode` variants, not API-local
strings or aliases of existing failure meanings.

## Startup and runtime flow

1. Clap parses types and required/conflicting arguments without exposing input values in
   diagnostics.
2. `ServerConfig` validates the transport against the parsed bind address.
3. Existing VOOM configuration resolves the database URL.
4. `HealthPlane::open` and `ControlPlane::open` connect to the existing database. Any
   missing, uninitialized, partial, dirty, or too-new state aborts startup without binding.
5. TLS mode reads and parses the certificate chain and private key. Failure names the
   operation and affected option, not PEM contents.
6. One generic `ConnectionDeadlineAcceptor<A>` delegates to its inner acceptor and then
   wraps the returned stream. In TLS mode its inner acceptor is Rustls, so the deadline
   starts after the handshake. In explicit loopback-cleartext mode its inner acceptor is the
   plain TCP acceptor, so the deadline starts immediately after TCP acceptance. The wrapped
   stream returns a timed-out I/O error once its total lifetime expires, including during
   response writes.
7. The process binds, logs its bound address and transport mode to stderr, and serves the
   bounded existing router over non-persistent HTTP/1.1.
8. SIGINT or SIGTERM stops acceptance, drains accepted work for at most 30 seconds, and
   logs retirement to stderr.

The existing `/health` endpoint is the readiness endpoint. It returns the current schema
diagnostic envelope and HTTP 200 only while the database is current. Listener availability
is the process liveness signal; no second alias route is added.

## TLS and client trust

Rustls presents the configured certificate chain and proves possession of the configured
private key. Node clients configure their CA trust and continue to present bearer tokens to
execution routes. Tests use a generated private CA and server certificate so success does
not depend on host trust stores.

An unknown CA and an expired certificate are client-side server-authentication failures and
never reach an HTTP handler. A production node agent is owned by issue #417, so this change
proves the client contract with a real Rustls-backed HTTP client and documents the equivalent
operator probe; it does not add a second temporary client implementation.

## Error and diagnostic contract

Startup failures go to stderr and return a nonzero process status. Diagnostics include the
failed operation, relevant option name, and corrective action. They do not print argument
values that may be secrets, PEM bytes, authorization headers, or bearer tokens. Stdout is
unused by the long-running process.

HTTP requests continue to use the existing API envelopes, including the explicit
`api.request` envelopes for process-generated 408 and 413 responses. TLS failures have no
HTTP response. Invalid bearer credentials keep the single generic HTTP 401 response and
`WWW-Authenticate: Bearer`; the presented token is absent from the body and logs.

## Shutdown behavior

Signal handling owns one `axum-server::Handle`. Once signaled, the handle stops accepting
connections and asks each active HTTP connection to shut down gracefully. Requests already
inside the router may finish within the 30-second grace period. At the deadline remaining
connections are dropped and process exit completes.

A dropped mutating response is ambiguous: the route transaction may have committed before
the connection was cut off, or cancellation may have occurred before commit. The node
client must retry that request with the same idempotency key. Existing route ownership then
returns the original result or performs the not-yet-committed transition once. No durable
state transition is invented at the server layer; route use cases retain transaction and
idempotency ownership.

## Threat model

### Boundary inventory

Added boundaries:

1. local operator arguments and environment into server configuration;
2. certificate and private-key files into Rustls configuration;
3. remote TCP/TLS peers into the process listener; and
4. OS termination signals into connection draining.

Widened boundaries:

5. untrusted HTTP requests can now reach the existing Axum routes over a production
   listener; and
6. route use cases reach the existing SQLite pool from a long-running process.

### Actor model

- Anonymous network peers control connection timing, TLS bytes, request heads, and request
  bodies but possess no valid node token.
- Authenticated nodes possess one bearer token and control their own route inputs; they do
  not gain authority over another node or worker.
- A local operator controls process arguments and the certificate/key file locations.
- The host account and deployment mechanism are trusted to protect private-key files and the
  existing SQLite database. VOOM does not defend against a host principal that can replace
  either file.

### Controls per boundary

| Boundary | Validation and authorization | Bounds and failure disclosure |
|---|---|---|
| Operator configuration | Clap type parsing; complete TLS pair; loopback test for cleartext | Fail before bind; name option and remedy, never supplied value or key bytes |
| PEM files | Rustls parses certificate chain and matching supported private key | TLS setup is startup-only; parse errors are sanitized |
| TCP/TLS peer | Rustls server authentication; HTTP unavailable before handshake; HTTP/1.1 only | 30-second handshake and request-head deadlines; 90-second total post-handshake lifetime includes response write/flush; one response per connection; TLS failures expose no application data |
| HTTP route | Existing bearer-token and node/worker authorization; existing strict DTO parsing | 1 MiB body and 30-second processing deadline; generic auth failure and existing envelopes |
| Shutdown signal | Only local OS signal delivery invokes the handle | Stop accept immediately; 30-second drain; no secret-bearing diagnostic |
| SQLite | Existing `connect`, schema probe, repositories, transactions, and checked decoding | No migration or new persistence; existing database errors remain authoritative |

### Standing security categories

- Secrets: private-key bytes remain inside Rustls loading; bearer values stay in
  `SecretString` and are never formatted or serialized.
- Cryptography: no custom cryptography; `axum-server`'s Rustls provider performs TLS.
- Deserialization: existing concrete request DTOs keep `deny_unknown_fields`; the new
  listener bounds bodies before extraction.
- Supply chain: add only the maintained Axum/Rustls server and Tower middleware needed for
  the boundary, plus test-only certificate generation; `cargo deny` and `cargo audit`
  remain hard gates.
- Defaults: loopback bind is safe; remote cleartext is impossible; certificate verification
  is never disabled in client tests or documentation.

### Explicitly out of scope

- Client certificates and mutual TLS: existing bearer tokens remain the node identity
  mechanism selected by issue #416.
- Certificate issuance, renewal, revocation distribution, and hot reload: deployment owns
  these; replacing a certificate requires restart.
- Denial of service above the fixed per-connection bounds, including aggregate concurrent
  connection or rate limiting: no requirement establishes a tenant or edge-rate policy.
- Host compromise: a principal able to replace the private key or database is already
  inside the trusted deployment boundary.
- New node routes, a node agent, or scheduler loops: those are separate epic work.

## Verification plan

### Focused unit tests

- configuration accepts TLS on any bind and explicit cleartext on IPv4/IPv6 loopback;
- configuration rejects non-loopback cleartext, missing/partial/conflicting TLS input, and
  zero limit values in test-only limit construction;
- router middleware returns the exact 413 and 408 JSON contracts above. Tests assert HTTP
  status, `application/json` content type, schema version, command, envelope status, `null`
  data, empty warnings, code, message, and hint rather than status alone;
- server protocol tests reject HTTP/2 negotiation and prove that HTTP/1.1 responses close
  their connection;
- a deadline-stream test proves that a blocked write wakes and fails at its deadline. A
  parameterized real slow-reading-client test covers both compositions: Rustls inside the
  shared deadline acceptor and the plain acceptor inside the same wrapper. Neither may
  retain a large-response connection past a shortened test deadline;
- TLS loader diagnostics omit sentinel PEM contents; and
- signal-driven handle shutdown completes an accepted slow request within the test grace
  while refusing a new connection, then cuts off a request that exceeds a shorter test grace
  period.

### Integration tests

- generated CA + valid server certificate + trusted client reaches `/health` and an
  existing bearer-authenticated execution route;
- a client trusting a different CA fails the TLS handshake;
- a client presented an expired server certificate fails the TLS handshake;
- an invalid bearer token over accepted TLS returns the existing generic 401 envelope and
  neither response nor captured diagnostics contain the token;
- a deterministic committed-but-response-lost test runs the production authenticated route
  over a capacity-bounded test stream, withholds response reads, observes the committed
  database transition and idempotency row, and triggers a shortened connection deadline.
  Retrying the identical request and key on a fresh connection must replay the original
  stored result and leave exactly one durable transition/event;
- the binary rejects non-loopback cleartext before listen; and
- the binary opens an initialized database without changing migration count.

Existing `remote_execution_route` tests remain the regression proof for idempotency and
JSON error shapes. Final verification runs focused `voom-api` tests, formatting, clippy,
the complete `just ci` suite, and GitHub's coverage/CI checks.

## Rollback

Before deployment, rollback is `git revert`: there is no migration or persisted format.
After deployment, stop the `voom-api` process and revert the binary/configuration change;
existing CLI and router-library consumers remain unchanged.
