# Control-plane-specific agent guidance

This file supplements the repository-root `AGENTS.md` for `voom-control-plane`.

## Orchestration boundary

- The control plane coordinates repository capabilities; it does not own production SQL.
  Do not add `sqlx` query construction or execution outside the explicitly allowed test
  fixtures. Extend `voom-store` with the smallest capability that expresses the use case.
- Preserve caller-owned transactions and fact ordering when composing repository operations.
  If existing behavior mutates durable state before appending an event, or appends evidence
  before unlocking dependencies, encode that order in focused failure-path tests.
- Keep domain newtypes intact through workflow and event helper structs. Only unwrap an ID at
  a genuinely polymorphic wire boundary such as an envelope subject; do not create an
  all-`u64` staging structure.
- Never hold a Tokio lock guard while awaiting an operation that can acquire the same lock,
  especially a write guard. Narrow the guard's lexical scope or copy the required state before
  the await, and test the contention path with explicit synchronization rather than sleeps.
- Tests involving a real `SqlitePool` use real Tokio time and an injected domain clock. Tests
  for capacity, restart, or concurrency use `Notify`, barriers, or durable-state assertions;
  broad elapsed-wall-clock assertions are not proof of the behavior under test.
