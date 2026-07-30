# Issue 415: Operation heartbeat liveness design

## Goal

Keep a workflow lease, and every operation claim renewed by that lease, live
until the operation has durably succeeded or failed. A worker terminal response
is an intermediate event; it is not the end of leased work.

## Existing failure

The source-backed operation adapters execute this lifecycle:

1. prepare or resume durable operation state;
2. dispatch to a worker;
3. validate the worker result;
4. verify and probe staged bytes;
5. commit or recover durable artifacts;
6. encode the ticket result;
7. release or fail the workflow lease.

`await_with_lease_heartbeats` currently wraps only step 2 in the video,
remux, and audio runtime dispatchers. Policy verification wraps worker
verification and persistence but stops before result encoding and lease
terminalization. Once either wrapper returns, slow host-side work can cross the
lease and audio-claim expiry without another heartbeat.

The store is correctly fail-closed: a heartbeat cannot resurrect an expired
audio claim. The defect is the control plane stopping heartbeats too early, not
the claim expiry rule.

## Decision

`dispatch_control_plane_ticket` becomes the single heartbeat owner for a
source-backed operation adapter. It wraps the complete selected adapter future:

- video transcode;
- remux;
- audio transcode, including synthesis;
- audio extraction;
- policy artifact verification;
- result encoding and the successful or failed lease transition.

The operation-specific runtime dispatchers and policy verification adapter no
longer create heartbeat loops. Generic non-source-backed NDJSON execution keeps
its existing stream watchdog and heartbeat behavior because it does not enter
this adapter path.

The heartbeat select is biased toward the operation future. Once terminal lease
mutation completes, that future is ready and must win over a simultaneously
ready heartbeat tick. This prevents a late heartbeat from observing the
released lease and replacing a successful result with a conflict.

Heartbeat errors before terminal completion remain fatal. The outer owner
durably fails the lease before returning the heartbeat error, preserving the
failure handling previously performed by each adapter around its inner loop.
If expiry or takeover has already made the lease non-held, that terminal
conflict is returned instead. Chaos heartbeat suppression remains on the same
operation key and therefore still exercises the genuine missed-heartbeat path.

## Deterministic regression seam

Unit tests need to hold execution after the worker result but before validation
without adding a production delay or timing flag. A `cfg(test)` synchronization
object is carried by `WorkflowChaosOptions`:

- one operation kind identifies the held adapter;
- `worker_result_observed` tells the test the old inner wrapper has returned;
- `resume_post_dispatch` releases the adapter to continue.

Each runtime dispatcher signals this seam immediately after receiving its
worker result. Policy verification signals it after its existing
verification-and-persistence future returns but before result encoding and
lease release. That is the boundary where its current nested heartbeat owner
stops.

Tests use real Tokio time and the real SQLite pool. They never pause Tokio time.
While the adapter is held, the test advances the injected `ManualClock` in
increments smaller than the lease TTL and waits in real time for each durable
heartbeat before advancing again. The total domain-time advance exceeds the
original TTL:

- before the fix, no post-worker heartbeat is written and the regression test
  times out or an audio claim expires;
- after the fix, each heartbeat renews the lease and audio claims, and the
  operation crosses the claim-sensitive terminal boundary.

This seam is test-only and does not alter production configuration or public
contracts.

## Test matrix

The executor regression matrix drives each source-backed workflow beyond the
worker result:

| Operation | Required assertion |
|---|---|
| video transcode | post-worker lease heartbeat and successful release |
| remux | post-worker lease heartbeat and successful release |
| audio synthesis | post-worker heartbeat, terminal dispatch evidence, staged operation, and a claim expiry beyond the original TTL |
| audio extraction | post-worker heartbeat, live extraction claim, success |
| policy verification | post-worker heartbeat, persisted verification, success |

Existing tests remain the fail-closed proof for expired-claim resurrection,
competing claim generations, and chaos-suppressed heartbeats.

The repository's only checked-in video fixture has no audio streams, so the
synthesis case cannot satisfy its later bundled result-probe shape check. It
asserts the claim-sensitive terminal dispatch and staging boundaries instead;
the existing synthesis tests cover successful probing, commit, and lineage.

## Non-goals

- no lease or claim schema change;
- no protocol or worker change;
- no longer TTLs or weakened expiry comparisons;
- no independent heartbeat task detached from the leased future;
- no change to generic stream watchdog semantics.

## Success criteria

- exactly one heartbeat loop owns every source-backed adapter lifecycle;
- heartbeats continue after the worker terminal result through durable lease
  terminalization;
- completion wins a simultaneous terminal heartbeat tick;
- heartbeat-write failure does not leave a lease held;
- audio claim renewal continues through post-dispatch work;
- genuine missed heartbeats and stale claims remain fail-closed;
- focused tests and `just ci` pass without warnings.
