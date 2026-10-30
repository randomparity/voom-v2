# 0123 — Control plane and node agent enforce an exact protocol version match

## Status

Accepted (2026-10-30).

## Context

ADR 0016 makes the worker wire an exact `PROTOCOL_VERSION` match, checked at the
`/v1/handshake` negotiation and again on every `/v1/operations` request through the
`x-voom-protocol-version` header. The control-plane routes a node agent calls (execution,
commit and scan) carry no version at all. #794 measured the gap with protocol 5 and protocol 6
release binaries (#817): a protocol-6 control plane with protocol-5 agents, and a protocol-5
control plane with protocol-6 agents, both activated and executed leases. With
`probe_hash_reuse` on, the protocol-5 control plane then failed the scan session at batch
decode, after the lease had run. ADR 0120 contract 9 calls a protocol bump a flag day for the
control plane, every node agent and every bundled worker, and its rollout gate G7 requires
skew to be refused before lease execution. Nothing enforced that between the control plane and
the agent.

## Decision

1. **One contract, both directions.** The control plane and a node agent are compatible iff
   both speak `voom_core::PROTOCOL_VERSION`, the same constant ADR 0016 governs. There is no
   range.
2. **Every node-token route is gated.** The agent sends `x-voom-protocol-version: <version>` on
   every request. `voom-api` checks it in one middleware over the execution, commit and scan
   routers, before credentials, body parsing or the handler. A missing, malformed or unequal
   value gets `400 BAD_ARGS` naming the offered and expected versions, so the request has no
   effect. `/health` is not gated.
3. **The agent checks the answer.** The middleware stamps the control plane's version on every
   response it produces. The agent refuses any 2xx response whose header is missing, malformed
   or unequal, with a terminal error. Error responses are not checked: they never lead to
   execution, and outer timeout and body-limit layers or a proxy can produce them without the
   header.
4. **Activation is the handshake.** Activation is the agent's first call, and an activation
   error ends the agent before it reads any lease. Either skew direction therefore stops the
   agent at startup. A control plane replaced under a running agent is refused on the agent's
   next request; that error is fatal to the agent too.

## Consequences

- G7's control-plane/agent arm holds in both directions without a schema change, a migration
  or a protocol bump. A future bump flows through both checks because both compare against the
  one constant.
- Builds from before this decision send no header and check none. They refuse every build that
  has this decision, even at the same protocol number. That is the intended flag day, not a
  regression.
- A control plane without this decision still applies a request before the agent refuses its
  answer. At startup that is only the activation; the agent exits and the incarnation expires
  like any agent that stops after activation, so no lease is taken. Swapped under a running
  agent, it can also record that agent's last lease failure or commit step.
- A request refused by the middleware reveals the control plane's protocol version to an
  unauthenticated caller, as the worker gate already does. The version is not a secret.
- Any relay between agent and control plane must pass the response header through.

## Considered & rejected

- **Add `protocol_version` to the activation body and record it on the incarnation.**
  judgment: needs a migration and a check on every lease path to cover a control plane replaced
  under a running agent, and the response direction would still need its own field. One header
  checked on both sides covers every route without durable state.
- **Activation-only check.** verified: `crates/voom-node-agent/src/runtime.rs:269` activates
  once per agent run; later lease, heartbeat and scan calls reuse the incarnation, so a control
  plane swapped after activation would never be checked.
- **A separate `/v1/handshake` route, as for workers.** judgment: activation is already the
  first call and already fatal on error, so a second startup round trip adds a route and no
  earlier refusal.
- **Bump `PROTOCOL_VERSION` to 7.** judgment: refusal needs only a mismatch on the new header,
  and pre-decision builds already fail it, so a bump would add churn (#794's measured protocol
  6 envelopes, `media_dispatch.schema` drains) without refusing anything more.
- **Do nothing and rely on the documented flag day.** verified: #817's evidence table shows
  both skew directions executing leases with binaries built from `4a1b37fa` and `f8be728c`.
