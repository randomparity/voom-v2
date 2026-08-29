# Supported node-agent stop-timeout design

## Goal

Close issue #597 by ensuring every accepted `shutdown_grace_seconds` value fits
the node agent's bounded shutdown tail within the supported supervisor
stop-timeout floor.

## Authority and constraints

The frozen scope is issue #597 and `WORK:SCOPE` token `q597-6e3ad6f7`.
[ADR 0090](../../adr/0090-bound-node-agent-grace-to-supported-stop-timeout.md)
chooses a 45-second supported supervisor floor and a maximum 18-second worker
grace. The resulting 44-second agent bound leaves one second before the
supervisor kill deadline. The change does not add a service unit, inspect the host service manager,
change runtime shutdown budgets, add dependencies, or migrate persisted data.

The implementation is limited to node-agent configuration validation, directly
coupled boundary assertions, ADR/index documentation, and the node-agent
operator runbook. `main` is the base branch and `just ci` is the complete
guardrail.

## Behavior

`AgentConfig::validate` accepts `shutdown_grace_seconds` from 1 through 18,
inclusive. Zero and values 19 or higher return `CONFIG_INVALID`.

The upper-bound diagnostic must identify the rejected value and tell the
operator all three facts needed to act:

1. the accepted range is `1..=18` seconds;
2. the supported supervisor stop-timeout floor is 45 seconds; and
3. the bounded tail is `shutdown_grace_seconds + 26` seconds.

It tells an operator who needs more than 18 seconds to change the worker's
shutdown behavior. A larger `TimeoutStopSec` is not presented as an escape from
the supported configuration contract. This avoids a diagnostic that recommends
a unit-file change while the validator still rejects the value.

The runbook replaces per-value timeout arithmetic with one installation rule:
configure the supervisor to allow at least 45 seconds, then choose an accepted
grace value. It retains the explanation of the bounded tail and documents that
supervisor values below 45 seconds are unsupported.

## Components and data flow

`config.rs` defines one named maximum for the accepted grace and uses a focused
validation branch so the policy-specific error can be actionable. No generic
bound helper changes. `config_test.rs` proves 18 succeeds, 19 fails, and the
failure exposes the range, arithmetic, supported floor, rejected value, and
remediation. The existing runtime budget-ladder test uses the same numeric
ceiling in its worst-case total assertion, preserving the documented coupling
without adding a production dependency between configuration and runtime.

The ADR index receives exactly the ADR 0090 row because `check-adr-index` is CI
gated. The runbook and ADR describe the same policy and arithmetic.

## Error handling

The validation failure remains `VoomError::Config` / `CONFIG_INVALID`. It
occurs before token loading, matching the existing configuration failure order.
No platform probe or I/O is added to validation.

## Testing and acceptance

- A configuration with grace 18 loads successfully.
- A configuration with grace 19 fails with `CONFIG_INVALID` and an actionable
  policy diagnostic.
- Existing zero-bound coverage continues to fail.
- The runtime worst-case shutdown-budget assertion uses 18 seconds and proves a
  total of 44 seconds, strictly below the 45-second supervisor floor.
- `just ci` passes, including ADR-index coupling, formatting, Clippy, tests,
  docs, dependency denial, and audit.

## Rollback

Reverting the change restores the old 60-second accepted maximum and its
operator arithmetic. No data migration or external state cleanup is required.
