# Consistent owner-publication completion observation (#843)

## Authority and problem

Scope: #843, claim q843-b015fb05, expanded operator approval recorded in
WORK:SCOPE https://github.com/randomparity/voom-v2/issues/843#issuecomment-6575989857.
The operator replied exactly "approve" to the production waiter repair,
deterministic regression, retained expiry-fixture correction, four real proofs,
full CI and one PR. Exclusions are explicitly EMPTY. This is cycle 2,
M / full-spec / fixed denominator 250; the original S/no-spec investigation,
failed review and consumed controlled diagnostic remain historical evidence.

A controlled synthetic run showed duplicate completion returning 409 CONFLICT
and 200 success after publication had atomically released the source lease and
persisted an OK completion replay. PendingMedia::wait reads replay and guards in
one read transaction, commits it, then reads the lease again outside that snapshot.
A healthy publication between those reads is misclassified as lost authority.
The first uninstrumented failure did not retain response pairs; the controlled
run proves this mechanism without claiming exact attribution to that first run.

## Scope and design

ADR0109 already requires atomic publication and terminal replay; ADR0123 requires
exact protocol headers. No new architectural decision or numbered record is needed.
The publication module remains the owner; direct callers and finalization stay intact.

Recommended: delete the redundant out-of-transaction lease read and its error branch.
The existing guard_context_in_tx checks the exact held worker lease and expiry
inside the same snapshot as the preceding replay lookup. Keep that guard, member
checks, read transaction ownership and the existing 50 ms wait outside transactions.
The next iteration reads the committed replay before checking held authority.

Alternatives: a second replay lookup after observing a released lease requires a
second snapshot and another error-classification path; serializing the read with
writers adds lock contention. Neither is needed when one existing read snapshot
already answers replay versus pending authority consistently. No ownership move,
caller migration, compatibility facade or obsolete public API is involved.

Retain the demonstrated fixture correction: wait for the isolated API to expire
the deliberately stale source lease and fail its paired ticket before comparing
publication snapshots. Allow only those exact liveness changes; preserve the full
pre/post table equality and exact duplicate-response tuple assertions.

## Success

1. A publisher committing between waiter iterations causes the waiter to return
   the exact persisted completion outcome, rather than a false lost-lease conflict.
2. With no completion replay, expired or non-held source authority still fails.
3. The four #843 modes pass on current-source binaries in fresh retained synthetic
   directories with actual empty failure arrays and recorded exits/durations.
4. The report distinguishes original failures, controlled evidence, repaired proof
   observations and final guardrails. It claims no unrun CI or platform coverage.

## Failure model

- Actors/deployment: authenticated owner completion, atomic publication writer and
  concurrent waiter on the existing SQLite/WAL deployment; synthetic media proofs.
- Invariants/assets: exact replay identity, held lease/claim and source/root fences,
  member atomicity, rollback, strict headers, source preservation and private tokens.
- Accepted classes: one existing poll interval before noticing committed publication;
  separate ordinary request timeout/retry behavior remains unchanged. No false
  lost-lease conflict from the confirmed publication interleaving is accepted.
- Other ownership: new schemas/protocols/dependencies, byte ownership and unrelated
  engine behavior are outside this approved repair surface. Existing production
  cancellation/expiry rules remain controlling; no bypass is introduced.

## Threat model

The changed trust boundary is persisted replay/lease state into completion judgment.
An authenticated caller can repeat a completion; it gains no new authorization.
Replay lookup and guard_context_in_tx retain typed IDs, request identity, node and
worker incarnation, access-plan owner/root epochs and source-location fences.
No request field, credential handling, route or durable shape changes. The fixture
reads synthetic database rows and HTTP errors; parameterized IDs, bounded waits and
safe code/table-name diagnostics retain existing privacy and resource bounds.
Existing structural guards and independent security review check the touched boundary.

## Validation

Use a real SQLite fixture and installed SQLx before_acquire hook on a dedicated
single-connection waiter pool. Arm it only after setup. Gate exactly the second
acquisition; a separate publisher pool commits lease release plus completed replay
before releasing it. No unrelated work shares the waiter pool. The old code's second
acquisition reads a released lease and fails; the repair's second acquisition reads
the terminal replay and returns it. Notify synchronization and bounded timeouts
establish ordering, without sleeps as evidence, paused Tokio time or production hooks.
A no-replay negative checks expired and released leases. Existing real fixture
proofs cover the complete publication/member path omitted from this focused fixture.
Run focused red/green and a controlled fault reintroducing the deleted read, relevant
source guards, fmt/lint, mandatory hooks, and final just ci after target lease grant.
The human-readable evidence report has no executable consumer; verify its figures
against retained command/JSON artifacts rather than a wording test.

## Global Constraints

Rust 1.95.0 remains pinned; no dependency or toolchain floor changes. Targets are
none declared; verification records the Linux x86_64 host without inferring targets.
Use existing tokio/sqlx/SQLite, real Tokio time and injected domain time. No new
schema, protocol, dependency, media ownership or issue. All four proof modes use
fresh retained private synthetic directories; public artifacts contain no private
paths, hostnames, credentials or raw capability-bearing bodies. Cargo and hook
execution require the campaign's exclusive shared-target lease.
