# Issue #334 implementation plan

Design:
[`2026-07-27-issue-334-policy-verification-design.md`](../specs/2026-07-27-issue-334-policy-verification-design.md)

## Success criteria

- Explicit published verify nodes execute through the bundled verifier for dependency-produced and
  pre-existing active artifacts.
- Exact target identity, evidence, ticket terminal state, events, downstream gating, resume
  idempotency, CLI output, and stored compliance reports are behavior-tested.
- Published DSL and compiled-policy wire shapes do not change.
- Focused tests, mutation/sensitivity proofs, and `just ci` pass.

## Commit 1 — Persist ticket-owned verification and read-only outcomes

1. Write migration tests that fail because policy evidence lacks a ticket owner and workflow
   outcomes lack `verified`.
2. Add migration 0026:
   - nullable artifact-verification ticket and lease owners, with a unique partial lease index;
   - rebuild file-phase summary/history tables with `verified`, preserving existing rows and
     indexes, and add the exact verification reference to file-phase summaries.
3. Extend artifact and workflow repository types, reads, writes, validation, and sibling tests.
4. Update phase rollup, run gates, progress counts, and resume history semantics for `verified`.
5. Add a forced evidence-before-finalization crash fixture; make resume validate and seed that
   exact successful evidence without a new ticket or verification row.
6. Run focused store/coordinator tests, then deliberately remove one `verified` mapping and confirm
   a behavior test fails before restoring it.

## Commit 2 — Resolve and verify exact active artifacts

1. Add failing tests for:
   - reusing a dependency-produced committed handle;
   - creating then reusing an existing active-file handle/location;
   - rejecting stale, retired, ambiguous, or mismatched file identity;
   - exact local-path revalidation during verification;
   - replaying lease-owned evidence without duplicate rows while allowing a new-lease retry.
2. Add the transactional active-artifact resolver and creation events.
3. Refactor verification around a shared exact-target core while keeping the public staging input
   and its tests unchanged.
4. Add ticket-owned evidence lookup/insert and report conversion.
5. Run focused artifact/store tests and perform a sensitivity proof by bypassing exact-location
   revalidation, observing the targeted test fail, then restoring it.

## Commit 3 — Dispatch verify nodes and gate downstream phases

1. Add failing binding/executor/coordinator tests for exact payload identity, bundled selection,
   success, failure, abort, continue, and downstream non-dispatch.
2. Bootstrap the built-in verifier only when a planned verify node needs it.
3. Add the explicit bundled dispatch variant; preserve registered-runtime dispatch for every other
   operation.
4. Implement the policy verification adapter with lease heartbeat, evidence replay, terminal lease
   transition, and structured ticket result.
5. Finalize successful verification as `verified` with unchanged file refs and its artifact handle.
6. Run one end-to-end policy test through the real bundled verifier process; use injected
   dispatchers only for deterministic failure edges.
7. Run focused workflow tests and prove gating sensitivity by temporarily treating failed evidence
   as success, observing the downstream-dispatch test fail, then restoring behavior.

## Commit 4 — Expose verification in compliance and CLI results

1. Add failing control-plane and CLI tests for success, partial failure, and
   `compliance report --job-id`.
2. Add the additive `artifact_verifications` view loaded by ticket/job correlation and ordered by
   verification ID.
3. Thread it through success and partial execute data and stored-run report data.
4. Update intentional CLI snapshots; assert stdout remains one standard JSON envelope.
5. Assert failure cases against durable job, ticket, lease, verification, and event rows, not only
   the exit code or worker request.
6. Run focused compliance/CLI tests and prove reporting sensitivity by omitting the evidence load,
   observing the tests fail, then restoring it.

## Commit 5 — Documentation and verification

1. Update the ADR index, control-plane design execution status, and any operator-facing policy
   execution documentation that still says verification is deferred.
2. Re-read the diff for wire compatibility, migration rollback behavior, warnings, and unrelated
   changes.
3. Run the adversarial code review loop and fix defensible findings.
4. Run the simplification review and retain only behavior-preserving reductions.
5. Run all focused tests, `just ci`, and the repository coverage command.
6. Rebase on the then-current `origin/main` only after #364 and #351 are merged, rerun focused
   tests and `just ci`, then push and open the PR. Do not merge; return the green PR to the campaign
   orchestrator.
