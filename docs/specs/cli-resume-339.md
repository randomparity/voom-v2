# Explicit CLI resume for production acceptance (#339)

## Problem

The operator runbook promises resume by repeating `compliance execute`, but the
CLI always takes the fresh-run compliance path. ADR 0009 requires an explicit
prior job and reconciliation of its durable per-file phase state.

## Scope and authority

The operator authorized this separately scoped prerequisite after the #339 park.
The frozen scope is issue #339 WORK:SCOPE token `q339-281f62c9`, prerequisite cycle.
Add optional `--resume-job-id <id>` to `compliance execute`. Carry the ID as a
`JobId` in compliance execution options and select prepared resume inputs before
issue application. Reuse the coordinator's existing resume preparation/runner.
Keep ordinary execution, safety preflight, runtime liveness checks, issue
application, and success/partial-error JSON construction on their existing paths.
No schema, dependency, DSL, media mutation, or reconciliation-algorithm changes.
No ownership transition or new architectural choice: the compliance facade owns
preflight/presentation; the existing coordinator owns durable reconciliation.

## Behavior

Without the flag, execution remains fresh. With it, validate that the prior job
exists, prepare reconciliation before applying issues/opening a new job, and
check the requested file window matches the prior durable window. Missing or
invalid prior state returns the existing specific error envelope and no new job.
A resumed job uses existing terminal/verified phase evidence rather than
repeating completed work. The normal compliance envelope reports its new job
and carried phase/ticket/verification evidence, including partial data on failure.
Operation-output arrays retain their existing current-job semantics.
Safety policy enforcement and backup requirements apply before dispatch on
both paths. Document original policy/input/settings and a stopped prior
executor as operator prerequisites; no automatic liveness fencing is added.
The prerequisite PR references #339 and must not close production acceptance.

## Failure model

This serves one attended operator using a dedicated database and unchanged
policy/input identities, with the prior executor stopped (ADR 0009 single writer).
Unknown prior IDs, incomplete/corrupt reconciliation state, mismatched windows,
safety rejection, and downstream partial failure must fail loudly. Existing
coordinator lineage, corruption, and promotion diagnostics remain authoritative.
Concurrent prior executors, policy migration, and fixing promotion crash windows
are outside this prerequisite; do not claim those failures are solved.

## Validation

Parser regression proves the new flag is accepted and retained; it fails red
before implementation. Compliance tests prove resume preserves verification/ticket
identities and does not dispatch completed phases, including repeated partial
resume; invalid prior state/window leaves jobs/issues unchanged. Safety refusal
opens its existing blocked issue without opening a job. CLI integration exercises flag-to-facade-to-coordinator wiring and
single-envelope error/success or partial output. Existing fresh-run coverage
continues to run. Docs are checked against actual CLI help and test evidence,
not prose assertions. Run relevant focused tests, format/lint/source checks,
then assembled `just ci` before delivery; retain exact results and build identity.
