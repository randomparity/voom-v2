# Candidate operation matching (#714)

## Problem and scope

A canonical `scan_library` worker cannot see a ready
`synthetic.workflow.operation.scan_library` ticket. Discovery filters raw tokens before
ADR 0072's dispatch normalization. Eligibility also compares raw tokens. This design
extends the existing classifier and store matching paths; it introduces no registration,
wire, schema, worker declaration or authentication surface. See [ADR 0107](../../adr/0107-normalize-known-operations-during-scheduling.md).

## Decision

Use `TicketOperation::normalize()` as the vocabulary authority. Add a core method
`matching_tokens(&self) -> Vec<TicketOperation>` to `NormalizedTicketOperation`:
known operations return canonical then namespaced encoding; custom operations return
the original token; unknown reserved operations return an empty list. Keep existing
`matching_token()` for canonical dispatch. No prefix-stripping SQL or duplicate bootstrap
capabilities is introduced.

The store expands supported matching tokens for ready-ticket SQL without changing its
ready/deadline/attempt/open-job predicates or priority/deadline/ID ordering. Deduplicate
query tokens; preserve the original stored ticket kind. An empty supported set returns
no candidates, so a reserved unknown token cannot starve unrelated work.

Worker candidate construction decodes capability operations and whole grant/deny arrays
before classification; do not use SQL EXCEPT before validating persisted tokens.
Normalize known operations to canonical set keys, retain custom exact keys, discard
unknown reserved keys, then subtract normalized denies. Union capability/grant candidates
as today; capability AND grant remain required at eligibility.

Eligibility matches capability rows under either known encoding and compares grant/deny
entries by the same equivalence. A deny in either encoding wins across grant rows.
Unknown reserved operations are ineligible even if explicitly declared; malformed stored
operation syntax or grant-array structure remains a database error, including malformed
entries paired with a deny. Decode before absence/denial classification.

Capacity and capability projections use the same recognized aliases: grant max_parallel
keys combine by the existing maximum rule across equivalent keys and rows, then wildcard,
then default one. Held lease counts include both known ticket encodings once. Hardware
capacity keeps the existing per-device minimum-session limit and distinct-lease accounting;
its capability queries include both known transcode encodings. Capability details/history
include matching declarations in existing ID order. Custom tokens retain exact matching.

Authenticated remote acquisition, root ownership/locality, liveness, guarded leasing,
transaction/replay boundaries, dispatch canonicalization and persisted values remain with
their existing owners. Store SQL owns matching; core owns vocabulary. No caller migration
or new scheduling path is needed; remove the raw-token-only matching within these methods.

## Success and validation

1. A canonical worker activated through the authenticated API acquires a namespaced
   byte-work ticket on its owned active root; dispatch is canonical and replay returns
   the same lease. Existing wrong-owner, stale incarnation and rollback arms still pass.
2. Core tests enumerate the existing OperationKind vocabulary and both encodings; custom
   and unknown-reserved results are exact and empty respectively.
3. Store regressions cover cross-encoding capabilities/grants/denies, multiple rows,
   malformed persisted entries before denial, custom exactness, unknown exclusion,
   capacity key equivalence and hardware accounting. Ready-ticket tests mix priorities,
   deadlines, exhausted attempts, closed jobs and both encodings in one ordered result.
4. Focused regressions must fail against the old matching behavior, then pass after the
   repair. Run affected store/API/control-plane tests, structural guards, formatting and
   lint, mandatory hooks and final `just ci`. Retain the canary proof for #436 after merge.

## Global constraints

Rust 1.95.0; no new dependencies or schema migrations. Targets:
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`.
Use sibling unit-test files, typed domain values and caller-owned named transactions.
No source media modification/deletion, exhaustive production grammar, DSL changes,
full production execution or retained-artifact deletion is authorized.

## Failure model

- Actors and deployments: local operators and authenticated node agents using the existing
  SQLite control plane, including concurrent acquisition and durable replay.
- Invariants/assets: deny precedence, persisted corruption diagnostics, capacity, root/node
  fencing, deterministic selection, atomic lease/plan/replay and canonical dispatch.
- Accepted failure classes: unknown reserved operations remain ineligible without aborting
  a candidate loop; their dormant-ticket observability remains ADR 0069's bounded residual.
- Covered elsewhere: API authentication/current incarnation and lease/root fencing retain
  ADR 0072 and existing remote-execution tests; #436 owns real process scan proof;
  #339 owns canary/production acceptance and operator selection/retention decisions.

## Threat model

- Boundary inventory: no new boundary; widened interpretation of known operation tokens
  in stored capability/grant/deny data and ticket selection, followed by existing acquisition.
- Actors: authenticated workers can advertise supported operations; persisted SQLite rows
  are untrusted. Trust the current owner/authorization checks, not worker token spelling.
- Controls: typed syntax validation before classification, closed OperationKind vocabulary,
  equivalent denies, bound SQL parameters, unchanged capacity/root/incarnation rechecks
  inside the acquisition transaction. Errors retain contextual database diagnostics.
- Out of scope: compromised operator database authority and anonymous authorization bypass
  are owned by existing access controls; no new credentials or network endpoints are added.
