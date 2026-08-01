# Control-plane SQL boundary

- Status: Approved
- Date: 2026-08-01
- Desloppify finding:
  `review::.::holistic::cross_module_architecture::control_plane_bypasses_store_boundary`

## Problem

`voom-store` is the workspace storage layer, but production code in
`voom-control-plane` still executes SQL and decodes rows for store-owned tables.
The current bypasses span execution tickets and leases, workflow expansion and
summaries, artifact inspection and verification, audio dispatch, and commit
coordination. Schema knowledge therefore has two owners, and changing a durable
table or vocabulary requires finding consumers outside the repository crate.

## Goal

Make `voom-store` the sole owner of production SQL, row decoding, persisted-state
validation, and single-repository mutation semantics. Keep use-case orchestration,
transaction sequencing, and cross-repository decisions in `voom-control-plane`.
Once the migration reaches zero direct queries, enforce the boundary in CI with
no grandfathered exceptions.

## Non-goals

- Moving workflow or transaction orchestration into `voom-store`.
- Creating a generic query repository or a second database facade.
- Changing schemas, durable tokens, public payloads, or error codes merely to
  complete the ownership migration.
- Prohibiting direct SQL in sibling test modules and integration-test fixtures.
- Splitting the large audio orchestration module during this migration.
- Modifying an existing ADR. The change enforces the repository's already
  declared crate layering rather than recording a new architectural decision.

## Boundary

Production Rust sources under `crates/voom-control-plane/src/` must not use an
SQLx API that constructs or executes SQL after the migration. The prohibition
includes the `query*` function and macro families, `raw_sql`, and
`QueryBuilder`. Files whose names end in `_test.rs` are test fixtures and remain
outside the prohibition.

Repository methods live with the table and durable vocabulary they own. When an
operation participates in a control-plane transaction, the repository exposes a
transaction-accepting method rather than opening or committing its own
transaction. The control plane remains responsible for ordering calls across
repositories and deciding whether the encompassing transaction commits.

Repository return types express domain outcomes instead of leaking SQL rows,
column tuples, or unchecked persisted strings. Existing public error
classification is preserved unless a focused migration step demonstrates that
the current contract is ambiguous and adds behavior tests for the replacement.

## Migration sequence

The migration is performed as independently verified seams, with one
conventional commit per completed step:

1. Type audio synthesis dispatch-attempt status at the repository boundary and
   consume it exhaustively in the control plane.
2. Make audio extraction and synthesis claim-release outcomes explicit without
   forcing different lifecycle policies into one misleading contract.
3. Move ticket, lease, worker, and execution-state SQL into their owning store
   repositories.
4. Move workflow coordinator, expansion, and summary SQL into focused store
   operations while preserving control-plane transaction boundaries.
5. Move artifact inspection, verification, commit, media, and remaining
   production SQL into their owning repositories.
6. Verify the production control-plane query count is zero, then add the CI
   boundary guard and its self-test.

There is no temporary allowlist or checked-in baseline. The guard is enabled
only after the migration is complete, so its invariant is absolute from the
first passing commit.

## Guard design

The guard uses structure-aware matching to scan production Rust sources for the
forbidden SQLx call families. It reports every violating file and source
location, exits non-zero when any are present, and prints a single clean success
line otherwise. Its self-test creates isolated fixtures proving that:

- each forbidden call family fails;
- formatting and multiline calls cannot evade detection;
- similarly named non-SQLx functions do not fail;
- sibling test files may use SQL for fixture setup; and
- a clean production tree passes.

The guard is exposed through a dedicated `just` recipe and included in
`just ci`. It follows the existing shell-check convention: fail fast, actionable
diagnostics, no silently skipped paths, and a self-test wired into CI.

## Error handling

New repository methods retain typed SQLx errors as `VoomError` sources with
operation and identifier context. Unknown durable vocabulary fails closed at the
repository boundary. Zero-row mutations return an explicit domain outcome or a
contextual conflict according to the existing workflow's ownership semantics;
they are never silently normalized merely to make sibling APIs look alike.

## Testing

Each migration seam starts with behavior tests at its public repository or
control-plane boundary. Tests cover success, missing rows, stale identity or
generation, replacement races, malformed durable values, and transaction
rollback where applicable. They assert outcomes and durable effects rather than
SQL implementation details.

Focused repository and control-plane tests, warnings-denied Clippy, the new
boundary guard, ADR-diff verification, and the repository's staged hook suite
must pass before each commit. A Desloppify rescan follows bounded clusters to
confirm that findings disappear rather than migrate between files.

## Acceptance criteria

1. Production `voom-control-plane` contains no direct SQLx query or query-builder
   construction.
2. Every migrated operation is exposed through a focused owning `voom-store`
   repository API, including transaction-aware variants where required.
3. Control-plane orchestration and transaction ordering remain behaviorally
   unchanged.
4. Persisted finite vocabularies crossing the boundary are typed and fail closed.
5. The structure-aware guard and self-test run through `just ci` with no
   allowlist or baseline.
6. Focused tests and the full repository guardrails pass without warnings.
7. No existing ADR is modified.
