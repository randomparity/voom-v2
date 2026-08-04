# Store-specific agent guidance

This file supplements the repository-root `AGENTS.md` for `voom-store`.

## Persistence boundary

- `voom-store` owns SQL and durable row decoding. Add a narrow repository capability instead
  of issuing SQL from an upper crate. Return typed domain projections rather than rows or
  loosely related primitive tuples.
- Keep transaction policy with the caller when an operation spans repository calls, durable
  events, or workflow state. Repository APIs used in such sequences accept the caller's
  transaction and must not commit independently.
- Decode persisted enums and identifiers fail-closed. Use checked `i64`/`u64` conversions for
  both query inputs and row outputs, including optional fields; never use wrapping casts for
  durable IDs.
- Validate row corruption before domain-level presence or conflict checks. A malformed stored
  ID or size must remain a contextual database error even when another column is absent.
- A query filtered by durable identity should fetch and decode the identified row before
  comparing expected related IDs. Filtering mismatches out in SQL turns conflicts and corrupt
  data into misleading `NOT_FOUND` results.
- Preserve deterministic ordering explicitly in SQL or in the typed result. If a refactor
  moves a query, retain its scope predicates, tie-breakers, null semantics, and active/retired
  filters; add combined-corruption and boundary-value tests, not only one-invalid-field cases.
