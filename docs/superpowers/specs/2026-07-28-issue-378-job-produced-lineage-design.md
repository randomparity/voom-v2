# Issue #378 — Job-Produced Lineage Correlation

Date: 2026-07-28
Status: Approved
Base: `main` at `4c260ccb89bc15d013d1c4f58937264e0fc866af`

## Context

The phase-barrier coordinator currently decides whether a planned file committed
by rereading the selected asset's active tip after dispatch. Any tip different
from the planned version is recorded as the current job's output. That inference
is invalid when another writer promotes the same lineage after this job's
dispatch freshness fence has passed.

The durable execution path already records stronger evidence:

- phase-node ticket lookup binds tickets to the current job, phase, and node;
- successful operation ticket results name the job, ticket, lease, selected
  source version, staged artifact, verification, commit record, produced
  version, produced location, and reprobe snapshot;
- the commit record binds the selected source and produced version/location to
  the artifact and verification;
- policy-verification rows bind the artifact to the workflow ticket and lease;
  generic staged-artifact verification rows predate that link and leave both
  columns null;
- the location and snapshot bind to the produced version.

The finalizer discards this evidence and consults global lineage state instead.
No schema, DSL, compiled-policy, or ticket-result wire change is required.

## Goals

1. Attribute a committed file-phase row only to exact evidence owned by the
   same job, phase node, ticket, and lease.
2. Preserve attribution when a later unrelated promotion supersedes the
   job-produced version before finalization.
3. Never record an unrelated promotion as the workflow's produced version,
   location, snapshot, or artifact.
4. Apply the same predicate to successful and partially failed fresh and
   resumed runs.
5. Keep verification, no-op, skipped, and multi-output behavior explicit.

## Non-goals

- Changing published policy syntax or compiled-policy JSON.
- Changing operation ticket-result shapes.
- Preventing concurrent lineage promotion; #352 owns pre-dispatch freshness,
  while commit serialization remains owned by the commit pipeline.
- Reattributing sidecar outputs as the selected primary file's chain advance.
- Adding a new workflow summary table or migration.

## Production Evidence

For one planned file and phase node, candidate tickets are scoped by job,
phase, node, and rendered source file version. A candidate is job-produced
lineage only when all of these facts agree:

1. The ticket belongs to the current job, is succeeded, and is one of the
   current phase-node ticket ids.
2. Its result names that exact job and ticket.
3. Its result source version equals the file version selected for the phase.
4. Its result commit record is `committed`.
5. The commit record's source version, artifact handle, verification, produced
   version, and produced location equal the ticket result.
6. The result lease belongs to the result ticket. The verification succeeded,
   and any workflow ticket/lease link it carries equals the result ticket and
   lease. Generic staged-artifact verification requires both links to remain
   null rather than accepting a partial link.
7. The produced location belongs to the produced version.
8. The reprobe snapshot belongs to the produced version.
9. The produced version belongs to the selected file asset.

The result is the exact produced version, location, staged artifact handle, and
reprobe snapshot. The finalizer does not substitute the active tip or another
live location. The exact location may since have been retired; its durable
identity remains the correct attribution.

Every succeeded ticket result in the job-owned phase invocation is inspected.
A result that declares some but not all same-lineage commit fields, contains
malformed identifiers, points at missing rows, or disagrees with durable
evidence fails visibly. A phase may contain several mutating operations for one
file; the greatest exact result file-version ID is the last serialized
same-lineage commit and becomes the phase output. Two exact tickets claiming
that same latest result version are a conflict rather than an ordering choice.

## Outcome Semantics

### Successful phase

For a planned disposition:

- exact verification evidence records `Verified`;
- otherwise, exact same-lineage commit evidence records `Committed`;
- otherwise, if the selected version is still the active tip, record `Skipped`
  as the existing no-op behavior;
- otherwise, fail with stale identity evidence. The unrelated tip remains
  active but is not written to the file-phase row or regenerated report.

When exact commit evidence exists, it remains the phase output even if a newer
unrelated tip is active. The next phase's existing pre-dispatch guard then
rejects the now-stale job-produced version before sending more work.

### Failed phase

The executor drains in-flight dispatches before failed-phase finalization.
Record only planned files with exact verification or same-lineage commit
evidence. A selected file with no exact evidence receives no failed-phase row,
even if an unrelated writer advanced its tip. The original dispatch failure
remains the returned error.

### Resume

The new job's phase-node tickets are the only candidates. Historical rows
remain seeds and are not treated as evidence that the resumed job produced a
new version. Exact current-job evidence uses the same predicate as a fresh run.

### Multi-output and sidecars

Audio extraction can produce several committed sidecar assets. Its ordered
ticket result remains the source for terminal artifact promotion. Those
versions do not belong to the selected primary asset, so they do not advance
the primary `PhaseFile` or become its `Committed` summary refs. With the source
tip unchanged, the primary file-phase outcome remains `Skipped`. A complete
different-asset sidecar result is ignored for primary-lineage advancement; it
is not treated as malformed merely because the extraction report keeps
per-output reprobe snapshots inside `outputs`.

Audio synthesis companions follow the same rule. The transcode report's
same-asset primary result advances the selected file; companion results remain
sidecar lineage.

## Implementation

Add a small internal projection for common committed-operation result fields.
It validates the required common fields in the existing result object without
claiming to deserialize the operation-specific remainder or changing its
serialized form.
Resolve candidate rows with one bounded query over the already scoped ticket-id
set. Use left joins so missing declared evidence remains observable, then
validate result identifiers against commit, verification, version, location,
and snapshot columns before constructing `ProducedRefs`.

Replace both active-tip-based commit branches in `finalize_file` and
`finalize_failed_phase` with the shared evidence resolver. Keep
`verified_refs_for_tickets` as the verification-specific predicate and evaluate
it first.

`ProducedRefs::resolve` remains for resume reconciliation of historical
committed rows. Current phase finalization receives exact refs directly and
does not perform a "first live location" lookup.

## Tests

Add deterministic post-dispatch finalization fixtures that create a succeeded
phase-node ticket and its real correlated verification/commit/version/location/
snapshot rows, then advance the same asset through an unrelated commit before
finalization.

Cover:

1. Fresh success records the ticket-produced version while the unrelated
   version remains the active tip.
2. Resume success uses only the resumed job's ticket evidence.
3. Partial failure records a committed sibling from exact evidence and omits
   the unrelated-only branch.
4. A successful ticket with mismatched commit, ticket, lease, location, or
   snapshot evidence fails without a false committed row.
5. A no-evidence planned result with an unchanged tip remains skipped.
6. A no-evidence planned result with an externally advanced tip fails stale
   without a file-phase row.
7. Multi-output sidecars are not treated as the primary asset's chain advance,
   while exact ticket-result locations remain available for promotion.

Assertions inspect ticket results, commit records, file-phase rows, regenerated
reports, emitted events, and the final active lineage—not only the returned
error.

## Alternatives Rejected

### Compare version ordering

`produced_id < active_tip_id` can show that a later version exists, but not who
produced either version. Ordering is not ownership.

### Accept any successful ticket result

A result alone can be malformed or point at unrelated durable rows. Correlation
must cross-check commit and verification ownership.

### Store the active tip plus the ticket ids

Ticket attribution beside an independently selected tip preserves the bug: the
two facts need not describe the same write.

### Add a new ownership column to file versions

The existing ticket, verification, and commit relationships already encode the
ownership chain. Duplicating it would add migration and consistency burden
without improving the proof.

## Implementation Plan

1. Add failing coordinator tests for exact committed evidence, a later external
   tip, malformed/mismatched evidence, and failed-phase omission.
2. Add the common ticket-result projection and one same-lineage evidence query
   in `workflow/coordinator/finalize.rs`.
3. Replace successful and failed active-tip attribution with the shared
   predicate and stale/no-op handling.
4. Add resume and multi-output regression coverage, then assert durable
   summaries, reports, events, commit rows, and active lineage.
5. Run focused tests and strict clippy, review the diff for simpler reuse, run
   the adversarial review loop, rebase, and run `just ci`.

## Verification

- focused coordinator finalization and resume tests;
- workflow summary/report and promotion tests;
- strict clippy for touched crates;
- `just ci`;
- adversarial review of concurrency, rollback compatibility, and durable
  failure-state assertions before shipping.

## Design Review

The adversarial review approved the design after two corrections:

- missing joined evidence must fail rather than disappear through an inner
  join;
- different-asset extraction sidecars are valid multi-output results, not
  malformed primary-lineage evidence.

The design preserves the existing schema and durable JSON shapes, shares one
store-backed ownership predicate across success and failure finalization, and
leaves the global active tip relevant only to the no-op stale check.
