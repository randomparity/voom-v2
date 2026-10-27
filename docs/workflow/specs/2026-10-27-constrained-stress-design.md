# Completion response windows in the synthetic runner (#756)

## Problem and authority

Pristine `e49e302790fb94f150250848acdfa21c382623f6` reproduces the scheduled
failure with `just stress`: after 17.13 seconds the synthetic session exits on
HTTP408/REQUEST_TIMEOUT. Completion has a ten-second response window even when
SQLite contention delays standalone completion. The runner treats unknown outcome
as terminal. ADR0109 already requires frozen completion replay for healthy windows.
Scope: issue #756, WORK:SCOPE token `q756-a8b62e91`; operator approved the exclusions
and completion-only replay design in the invoking session.

## Scope and design

Keep HTTP handling in `voom-fakes::remote_runner`. No ownership transition or
production dependency is needed. `complete` opts into response-window handling;
other callers retain `post` behavior through a private common sending function.
Freeze the JSON body bytes and idempotency key before sending. On HTTP408 only,
validate schema `0`, command `execution.complete` or `api.request`, status `error`,
absent/null data and error code `REQUEST_TIMEOUT`. Then yield and repeat that
identical request. Malformed/inconsistent windows fail rather than replaying.
Success and terminal errors retain their existing handling. No dispatch, acquisition,
new key or completion count occurs between windows. This follows the production
client's existing policy without importing node-agent orchestration into fakes.

Existing session heartbeats retain their lifecycle ownership; healthy windows have
no artificial retry-count cutoff. The stress drain deadline is cooperative: recovery
can wait for a gate held by completion before checking it. Perpetual valid windows
can therefore require operator cancellation locally or the existing 90-minute CI
job limit; this repair does not promise a hard drain bound. Ordinary HTTP
and transport errors retain existing behavior. No new network retry policy.

## Success

- The focused HTTP regression fails on the current timeout and passes with replay.
- Several windows send identical body/key and finish once success arrives.
- Malformed windows and terminal errors stop; other API calls do not gain replay.
- Original `just stress` completes with its ticket-conservation assertions.
- Repository guardrails pass, including `just ci` before push.

## Failure model

1. Actors/deployments: local operators and constrained CI running authenticated
   synthetic HTTP sessions against the configured control plane.
2. Invariants/assets: exact completion identity, no duplicate dispatch or counted
   completion, truthful terminal failures and ticket conservation.
3. Accepted classes: prolonged healthy windows remain pending and may outlast the
   cooperative drain deadline while recovery waits; operator cancellation or the
   existing CI job timeout bounds that residual. Unrelated transport/5xx errors retain
   terminal behavior because this repair adds only completion-window replay.
4. Covered elsewhere: server transaction cancellation and idempotency (ADR0087 and
   ADR0109); production node-agent retry budgets and lease renewal (existing client).

## Exclusions and validation

Unrelated stress cleanup and workflow resource-limit changes remain future work
owned by repository maintainers, explicitly approved by the operator.
Use sibling `remote_runner_test.rs` HTTP regressions, real time for database tests,
`just stress`, then assembled `just test` and final `just ci`. No migration, public
contract, package version or ADR change; ADR0109 settles the timeout semantics.

## Threat model

The configured control-plane response crosses the HTTP trust boundary. Replay
requires both HTTP408 and the exact structured timeout fields; malformed data
fails closed. The bearer, endpoint and frozen body/key stay in the existing
request path. No authentication or destination policy changes. A perpetually
valid timeout remains pending as disclosed above; response-size and generic
transport hardening remain pre-existing sender behavior.
