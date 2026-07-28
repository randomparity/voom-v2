# Audio extraction N+1 claim fences

**Status:** Accepted

## Context

Audio extraction publishes an ordered, non-empty set of sidecar files before
one transaction registers every result. The operation claim prevents a stale
executor from continuing host filesystem mutation or finalizing relational
state after expiry, takeover, or dispatch-generation change.

The store-owned `assert_live_claim` predicate is read-only. It checks the exact
operation key, dispatch generation, lease, token, non-terminal state, and
durable expiry. Issue #337 deliberately replaced claim reacquisition at this
boundary with that assertion so a stale in-memory expiry cannot shorten a
heartbeat-extended claim.

Fresh commit and recovery currently assert immediately before and after every
member promotion. For `N` members this performs `2N` identical store reads. The
post-promotion assertion for member `i` is already the precondition for
promoting member `i + 1`.

## Decision

For every non-empty ordered promotion set, use these `N+1` boundaries:

1. Assert the exact live claim before promoting the first member.
2. Promote one member.
3. Assert the exact live claim immediately after that member.
4. Repeat steps 2–3 for every remaining member.
5. Finalize only after the post-last-member assertion succeeds.

Fresh commit and recovery use the same boundary numbering:

- boundary `0`: before the first promotion;
- boundary `i + 1`: after promotion of member `i`.

No claim is acquired, renewed, released, or rewritten by a fence. The existing
store predicate remains the only production eligibility check.

## Failure and ownership semantics

Claim loss at boundary `0` leaves every target absent. Claim loss at boundary
`i + 1` leaves only the exact prefix through member `i` promoted. No later
member and no finalize transaction may run.

Promotion is add-only, so already-promoted exact bytes remain crash-recovery
evidence:

- During a fresh commit, the stale claimant cannot mark recovery under a claim
  it no longer owns. The successor first records every pending member as
  recovery-required, then validates or promotes the set.
- During recovery, recovery-required rows and events already exist. A second
  lost claimant leaves that evidence unchanged for another successor.

A stale claimant must not create result file versions, file locations, media
snapshots, bundle members, extraction lineage, or a committed operation.

## Deterministic test seam

Add a crate-private claim-fence hook following the repository's existing
failure-injection pattern. The no-op hook is used by production entry points.
Tests can act immediately before a numbered assertion to:

- count the exact assertion schedule;
- expire the durable claim;
- replace its lease/token with a successor claim; or
- advance the durable dispatch generation.

The hook is specific to claim assertions. It does not mock the store predicate
or promotion behavior. Every injected mutation is therefore detected by the
real `assert_live_claim` SQL query.

For a two-member set, fresh commit and recovery each exercise boundaries
`0`, `1`, and `2`. At every boundary the tests force expiry, takeover, and
generation advance. They inspect:

- the promoted target prefix and absence of later targets;
- operation and artifact commit states;
- absence of result versions, locations, snapshots, bundle rows, and lineage;
- recovery-required events and their non-duplication on repeated recovery;
- exact hook observations `[0, 1, 2]` on an uninterrupted run.

This is focused instrumentation: it observes the claim-fence boundary and
mutates only durable claim facts.

## Compatibility

This change has no migration, public API, CLI, DSL, compiled-policy, event
payload, or durable JSON shape impact. It changes only the number and placement
of read-only store assertions around the existing promotion loop.

## Rejected alternatives

### Keep `2N` assertions

This is correct but performs one redundant store round trip at every
inter-member boundary.

### Assert only before each promotion

This omits the post-last fence, allowing finalization after claim loss during
the last promotion.

### Assert only after each promotion

This allows the first filesystem mutation to begin without a live-claim check.

### Renew or reacquire at promotion boundaries

This can overwrite newer durable claim state and would reverse #337's
read-only ownership decision.
