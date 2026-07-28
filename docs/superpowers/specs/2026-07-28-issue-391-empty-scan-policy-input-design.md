# Empty scan-derived policy inputs

## Context

The operator runbook defines whole-library and root-scoped scan selection as a
durable snapshot of the eligible video files at creation time. When selection
finds no eligible videos, that snapshot is still meaningful: planning,
execution, and reporting must complete as a zero-work no-op.

The scan builders already construct that empty aggregate, but the generic
`PolicyInputSetDraft` validator rejects every draft without a media snapshot or
bundle target. That generic invariant predates the operator contract and must
remain intact for imported, manual, fixture, and test callers.

## Decision

Add an explicit `ValidatedPolicyInputSetDraft::new_empty_scan` constructor for
the one exceptional aggregate shape. It accepts only an imported draft whose
target/member collections are all empty, while retaining the shared slug,
schema, fixture-label, and aggregate-budget validation. The existing `new`
constructor and `validate_input_set` continue to reject all targetless drafts.

Whole-scan and root-scoped builders select the validator from facts they
computed themselves:

- non-empty selection uses the existing generic validator;
- empty selection uses `new_empty_scan`;
- both persist through the existing store-owned transaction.

The exceptional constructor is not selected by draft labels or other
caller-controlled naming conventions. Generic creation cannot reach it.

Policy execution resolves the stored input before tool and verifier preflight.
For a resolved empty input it skips those worker-only checks and proceeds
through the coordinator's existing zero-phase path. Compilation, stored policy
identity, input lookup, profile resolution, planning, compliance reporting,
issue application, job creation, and durable summary finalization still run.

## Transaction and concurrency behavior

The existing policy-input repository transaction remains the sole persistence
boundary. Parent, fixture-label, and member inserts commit together. An
injected child-insert failure rolls the parent back. Concurrent creation of the
same empty slug is serialized by SQLite and the unique slug constraint: one
aggregate commits and the other returns the repository's existing constraint
error, with no partial duplicate.

Empty execution creates the normal owning job and a succeeded workflow summary
with zero planned, committed, failed, and skipped counts. It creates no phases,
file-phase rows, tickets, leases, worker requests, or execution events.

## Compatibility

This change does not alter the policy DSL, compiled-policy JSON, CLI envelope
shape, durable payload schema, public error codes, or generic validation
contract. Existing non-empty scan selection and execution use their current
paths.

## Verification

Tests cover:

- generic targetless drafts remain invalid;
- empty database, all-non-video selection, and an empty root scope;
- exact CLI JSON counts and durable zero-member aggregates;
- planning, preview reporting, execution, and post-run reporting with zero work;
- no worker dispatch or worker requirement for empty execution;
- concurrent same-slug creation;
- injected persistence failure with parent, child, job, ticket, lease, and event
  row inspection.
