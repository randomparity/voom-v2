# 0007 — The write lock is taken before the node token is verified

## Status

Open
review-by: 2026-11-27

## Concern

On the remote-execution control path the `BEGIN IMMEDIATE` write lock is taken
*before* the caller's node token is checked, so an unauthenticated party can make the
control plane take `SQLite`'s write lock.

The ordering, verified at `f7bfb742`:

- `crates/voom-api/src/execution.rs:576-588` (`bearer`) shape-checks only — an
  `Authorization` header, valid UTF-8, a `Bearer ` prefix, and a non-empty remainder.
  It compares nothing against any stored hash.
- `crates/voom-control-plane/src/cases/execution/remote_execution/acquire.rs:60`
  opens the transaction with `begin_read_then_write`.
- `:61-69` then calls `require_remote_incarnation_fence_in_tx` with `input.token`.
- `heartbeat.rs` repeats the ordering at `:28` (open) and `:33` (token).

The ordering is not accidental: the token check reads from the database, so it needs a
transaction handle. It is also pre-existing — issue #592's change did not introduce it
and does not touch it.

What #592 *did* introduce is a reason to care. Its accepted residual — a detached
opener may hold one of eight pooled connections for up to `LOCK_WAIT_BUDGET`, living
at most `POOL_ACQUIRE_BUDGET + LOCK_WAIT_BUDGET` = 75s (`voom-store/src/pool.rs:20,30`,
pool size at `:81`) — was accepted in the design's threat model against an actor
described as "an authenticated node agent … not anonymous". That description was
wrong, and the design record has been corrected. The residual is drivable without
credentials, and `bounded_router` (`voom-api/src/server.rs:344-352`) layers a body
limit, a timeout, and a response mapper — no concurrency limit.

State the direction honestly: before #592 the same unauthenticated request could wedge
every writer against that database permanently, until the process restarted. The fix
strictly improves this. What remains is that a pre-auth request can still occupy a
pooled connection for a bounded period.

## Why deferred

Reordering is not a one-line move, and it is not this issue's outcome. Issue #592's
frozen charter
(https://github.com/randomparity/voom-v2/issues/592#issuecomment-5447684231, token
`q592-387107cd`) scopes the work to eliminating the write-lock deadlock; nothing in its
five completion criteria reaches authentication ordering. Bundling it would be
absorbed adjacent work on the implementing run's own authority — the same reason that
run cut its `init.rs` change.

The fix also needs design, not just edits. Verifying the token requires a database
read, so a pre-check needs its own read-only transaction (`begin_read_only`) before the
write lock is taken, which means two transactions where there is now one, a second
round trip on every request, and a decision about what happens when the pre-check and
the in-transaction fence disagree. That last is the part with teeth: the in-transaction
check exists so the fence decision is serialized against concurrent incarnation
changes, and a pre-check must not be mistaken for that guarantee.

## Impact where it was found

Raised by `$detect-evil` during the branch review of issue #592, which refuted the
design's stated actor model. It is recorded here rather than fixed there because the
security record was the defect in scope; the ordering is the defect out of scope.

A rate limit or a concurrency cap on the pre-auth portion of these routes may be the
cheaper mitigation than reordering, and belongs in the same assessment.

## What would resolve it

Either the token is verified before any write lock is taken, or the pre-auth path is
bounded so an unauthenticated party cannot occupy pooled connections. Done when an
unauthenticated request with a well-formed body and an arbitrary bearer string cannot
cause a `BEGIN IMMEDIATE` on the remote-execution routes, proven by a test that drives
those routes with a bogus token and asserts no write lock is taken — or, if the
bounding route is chosen instead, when the pre-auth concurrency cap is asserted by a
test and the residual is stated against it.

## Provenance

target: crates/voom-control-plane/src/cases/execution/remote_execution/acquire.rs
target: crates/voom-control-plane/src/cases/execution/remote_execution/heartbeat.rs
target: crates/voom-api/src/execution.rs
Raised by `$detect-evil` during `$trial-loop` on the issue #592 implementation branch,
2026-08-27, under `$quest` for issue #592 (scope token `q592-387107cd`). The ordering
was verified against the sources named above before filing.
tracker: #592
