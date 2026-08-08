# ADR 0061: Shared test helper for SQLite check-constraint bypasses

## Status

Accepted

## Context

Tests that seed deliberately invalid SQLite rows must disable
`ignore_check_constraints` on one pooled connection and execute every dependent
statement through that same connection. The pattern is repeated across
`voom-store` and `voom-control-plane` tests, making the connection-affinity
invariant easy to miss.

## Decision

Provide `voom_store::test_support::with_check_constraints_disabled`. The helper
acquires one connection, enables the pragma, passes that connection to a
caller-supplied async operation, and drops it before returning. A repository
test guard rejects raw `ignore_check_constraints` pragmas outside the helper.

## Consequences

The bypass scope and connection affinity are named and centralized. Test setup
closures must route dependent SQL through the supplied connection, while normal
repository reads remain pool-backed after the helper returns. The source guard
keeps future tests from reintroducing open-coded bypasses.

## Considered & rejected

- **Keep open-coded sequences:** rejected because reviewers must repeatedly
  reconstruct a per-connection SQLite invariant.
- **Set the pragma through `SqlitePool`:** rejected because the setting is
  connection-local and dependent statements can use another pooled connection.
- **Use a transaction-only helper:** rejected because the existing tests need
  invalid rows to remain visible after the setup connection is released.
