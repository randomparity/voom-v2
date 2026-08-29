# 0090 — Bound node-agent grace to the supported stop timeout

## Status

Accepted (2026-08-29)

## Context

Issue #597. ADR 0088 bounds the node-agent shutdown tail to
`shutdown_grace_seconds + 26` seconds: two 10-second control-plane calls, the
configured child grace, a 1-second post-kill reap, and a 5-second backstop
margin. Configuration currently accepts grace values through 60 seconds, so an
accepted configuration can take 86 seconds.

systemd's upstream default stop timeout is 90 seconds, but supported Linux
distributions may choose a shorter default. Fedora 44 uses 45 seconds. An init
system that reaches its stop timeout sends `SIGKILL`, preventing the node agent
from completing the retirement write that the bounded tail is intended to
protect. The node agent cannot discover the effective unit timeout reliably
from its configuration validator, and this repository does not ship a unit.

## Decision

The supported supervisor stop-timeout floor for the node agent is 45 seconds.
Every accepted configuration must fit the shutdown tail within that floor
without relying on a distribution default or a unit-file override.

`shutdown_grace_seconds` therefore accepts `1..=18`. The fixed 26-second tail
plus the maximum grace is 44 seconds, leaving one second between the agent's
backstop and the supervisor's 45-second kill deadline. The existing 5-second
backstop margin remains part of the fixed tail; the extra second orders the two
independent deadlines instead of trying to cover another internal wait.

Validation owns the ceiling and reports the complete policy when it rejects a
value: the accepted range, the 45-second supported floor, and the
`shutdown_grace_seconds + 26` arithmetic. Operators who need a worker to receive
more than 18 seconds must change that worker's shutdown behavior; increasing
`TimeoutStopSec` does not make a larger grace value a supported node-agent
configuration.

The operator runbook states the fixed supported policy. It may still tell
operators to set an explicit `TimeoutStopSec` of at least 45 seconds so a local
distribution default cannot undercut the supported floor, but operators no
longer calculate a timeout from an accepted grace value.

## Consequences

- Configurations with `shutdown_grace_seconds` in `19..=60` become invalid.
  The project is pre-release, so this is a replacement rather than a
  deprecation window.
- Every accepted grace value fits the documented 45-second supervisor floor.
- A supervisor configured below 45 seconds remains unsupported; the runbook
  makes the explicit unit setting the installation check.
- The validation boundary test and the runtime shutdown-budget assertion share
  the 18-second ceiling as an intentional coupling to this ADR.

## Considered & rejected

- **Keep `1..=60` and require operator arithmetic.** verified: ADR 0088 and
  `docs/runbooks/operator-node-agent.md` require comparing the accepted value
  with `shutdown_grace_seconds + 26`; issue #597 records Fedora 44's 45-second
  default, under which accepted values above 19 exceed the supervisor timeout
  and 19 itself leaves no ordering between the independent deadlines.
- **Ship a systemd unit with a larger `TimeoutStopSec`.** verified:
  `git ls-files '*.service' '*.unit'` at commit
  `54fd1fd4bf27b0de763422861a4ec3e9884f690a` returns no files. Adding packaging
  is a new deployment surface outside issue #597's configuration-validation
  scope.
- **Accept larger values when the host unit has a matching override.** judgment:
  configuration validation must stay deterministic and cannot rely on probing
  a particular service manager or deployment unit.
- **Accept 19 seconds because the totals are equal.** verified:
  `shutdown_grace_seconds + 26` equals 45 at grace 19, while systemd forcibly
  terminates a service that is still running when `TimeoutStopSec` elapses.
  Equal independent deadlines do not guarantee the node agent completes first.
