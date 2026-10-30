# Refuse control-plane and node-agent protocol skew (#817)

## Problem

ADR 0120 gate G7 requires that a control plane, node agent or worker on a different protocol
version is refused before lease execution. Only the agent-to-worker handshake (ADR 0016)
enforces that. #794 ran mixed protocol 5/6 control planes and agents through the local
acceptance stack: both directions activated and executed leases, and with `probe_hash_reuse` on
a protocol-5 control plane failed the session at batch decode after the lease ran (#817).

Decision: [ADR 0123](../../adr/0123-control-plane-node-agent-protocol-match.md). It needs its own
record: it adds a public header contract on the 20 node routes and a refusal ahead of
authentication, which neither ADR 0016 (worker wire only) nor ADR 0120 (states the flag day,
enforces nothing) decides.

## Design

**Header.** `x-voom-protocol-version`, a decimal `u32`, the name ADR 0016 already uses toward
workers. Both sides compare it with `voom_core::PROTOCOL_VERSION`.

**Control plane (`crates/voom-api`).** `protocol.rs` (new) holds `PROTOCOL_VERSION_HEADER` and
one `axum::middleware::from_fn` gate. `base_router` merges the commit, execution and scan
routers, applies the gate with `.route_layer` (matched routes only), then adds `/health` outside it. The gate:

1. reads the request header: absent → `400 BAD_ARGS` "missing x-voom-protocol-version
   (control plane speaks protocol N)"; not UTF-8 or not a `u32` → `400 BAD_ARGS` "malformed
   x-voom-protocol-version; expected a decimal version" (the value is not echoed); unequal → `400 BAD_ARGS` "unsupported protocol
   version: offered=O, expected N". All three carry the hint "run the control plane and every
   node agent from one release; mixed protocol versions are refused (ADR 0123)" and command
   `api.request`. The handler does not run.
2. otherwise runs the handler.
3. sets the header to `PROTOCOL_VERSION` on the response from step 1 or 2.

**Node agent (`crates/voom-node-agent/src/client.rs`).** `send_attempt` adds the header to
every request. On a 2xx status, before reading the body, it calls `check_protocol_version`:
missing, non-UTF-8, non-`u32` or unequal → `VoomError::Config`, returned as
`AttemptResult::Complete(Err(..))`, so it is never retried. A 400 from the gate already maps to
`VoomError::Config` through `map_terminal_error`. Non-2xx responses are not checked (ADR 0123
decision 3). `scan_client.rs` sends through `ControlPlaneClient::send`, so it needs no change.

**Other callers of the node routes (sweep).** Searched `scripts/`, `docs/runbooks/`,
`docs/*.md`, `crates/voom-fakes`, `crates/voom-conformance`, `crates/voom-test-support`,
`crates/voom-cli` and `.github/` for the node routes and `x-voom-idempotency-key`, and
`scripts/` for relays (`http.server`, `urlopen`, `proxy`, `curl`), case-insensitively
(`rg -i`). Hits:

| Caller | Change |
|---|---|
| `crates/voom-fakes/src/remote_runner.rs` `post_request` | sends the header |
| `scripts/issue-733-group-proof-fixture.py` relay (`do_POST`) | copies the response header back to the agent |
| same fixture, self-built requests in `check_reuse_acquire` and `check_late_publication_rollback` (also driven by `scripts/issue-733-reuse-clone-regression.py`) | send the header, read from `crates/voom-core/src/lib.rs` in the checkout the binaries are built from |
| `crates/voom-api` route tests, `crates/voom-node-agent/tests/{client_tls,lifecycle}.rs` mocks | send / return the header |
| `docs/runbooks/operator-control-plane-api.md` curl example and reverse-proxy sentence | add the header |

`scripts/acceptance-bootstrap.py` (#339 stack) and the other `issue-*` regressions launch the
real binaries and relay nothing. `scripts/media-evidence-baseline.py` (#794) and
`scripts/net-resilience.sh` call only worker routes, which ADR 0016 already versions.

**Docs.** One troubleshooting entry in `docs/runbooks/operator-node-agent.md`; one sentence in
the protocol 6 paragraph of `docs/release-process.md`. ADR index row for 0123.

## Entry points

Every node-token route goes through the gate: activate, deactivate, node heartbeat, worker
readiness, lease acquire/heartbeat/complete/fail, the six commit routes, and the six scan
routes (four POST, two GET). The agent check runs on every `ControlPlaneClient` call, which is
the agent's only path to the control plane. Re-registration after a restart is a new
activation, so it is gated like the first.

## Failure model

1. **Actors and deployments.** An operator deploying control plane and node agents from release
   builds over TLS or loopback HTTP; the node agent as the only production caller of node
   routes; the synthetic remote runner and acceptance fixtures in tests.
2. **Invariants and assets.** A gated control plane refuses every node-route request from
   another version before it has any effect; an agent stops on the first 2xx answer from another
   version, so no lease is acquired after an activation that crossed skew; the public `BAD_ARGS`
   code and envelope shape; existing same-version behavior unchanged.
3. **Accepted failure classes.**
   - A protocol-5 control plane records the activation the protocol-6 agent then refuses; the
     agent exits and the incarnation expires as after any post-activation stop. No lease runs.
   - An older, ungated control plane swapped in under a running agent applies whatever request
     reaches it before the agent reads the answer and exits. That can be a lease failure that
     spends a ticket attempt, a lease held until its TTL, or an applied commit step. The swap is
     an operator error outside the flag-day procedure; G7 concerns skew at activation.
   - The remote runner sends the header but does not check responses: it is a test driver run
     against a control plane from the same build.
   - The version is visible to unauthenticated callers (ADR 0123 consequences).
4. **Covered elsewhere.** Agent-to-worker skew: ADR 0016 handshake. Re-running G7 against
   release binaries: #794.

## Threat model

- **Boundary.** Widened: the node routes now parse one more request header before
  authentication; the agent parses one more response header. Nothing is added.
- **Actors.** Network callers reaching the API listener (unauthenticated until the bearer check);
  the control plane as seen by the agent (trusted over TLS, but possibly another release).
- **Controls.** Request: header read with `to_str` and `parse::<u32>`, compared, refusal before
  any body read or database access; no refusal echoes the received value. Response: same parse; refusal is terminal and logged
  through the existing error path.
- **Pre-auth disclosure.** Without a token, a caller can already read the version: the gate
  stamps it on the 401 that any authenticated node route returns to a tokenless request. The
  "missing" and "unsupported" refusals state that same number and nothing else. They read no
  database row, name no node, and are identical on every node route. So refusing before auth
  exposes nothing beyond the response header. The malformed refusal does not echo the value it
  received.
- **Out of scope.** Authentication of the version claim: a caller that lies about its version
  still needs a valid node token for anything to happen.

## Validation

- `crates/voom-api/src/protocol_test.rs` (new): missing, malformed, `N+1` and `N-1` refused with
  400 `BAD_ARGS` and the response header; refusal precedes authentication and body parsing;
  `N` reaches the handler; `/health` ungated; a table of every node route refuses a request
  without the header.
- `crates/voom-node-agent/src/client_test.rs`: requests carry the header; 2xx with `N+1`,
  missing or malformed header → `Config` after exactly one attempt; the gate's own 400 refusal
  body → `Config` after exactly one attempt (terminal, not retried, even with retries
  unbounded); error responses without the header keep their existing classification.
- `crates/voom-node-agent/tests/lifecycle.rs`: the real agent runtime against the real router
  behind a layer that rewrites the response header to `N-1` (an older control plane) exits with
  the agent-side refusal, takes no lease and leaves the ticket `Ready`.
- `crates/voom-api/tests/remote_execution_route.rs`: an unversioned activation and acquire (an
  older agent) are refused, create no incarnation and leave the ticket `Ready`.
- `just ci` green. `just check-owner-publication <fresh workdir>` (default `chain` mode, which
  routes the agent through the relay; synthetic lavfi media) runs once and its result is
  reported. Extract mode and the reuse-clone regression, which reach the self-built requests,
  are not run: the operator asked for one chain run. Their header comes from the same helper,
  which the chain run imports.
