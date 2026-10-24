# 0109 — Owner media publication and grouped completion

## Status

Accepted (2026-10-23)

Amends ADR0074's scalar completion contract only for an ADR0042 extraction
operation. ADR0042's all-output atomicity and ADR0050's byte ownership remain.

## Context

Issue #733's reduced authenticated fixture reproduces successful owner-node
mutation tickets with no committed lineage, skipped phases, original-source
verification and empty output. ADR0075's observed outputs never reach the durable
staging, verification, commit and snapshot lifecycle. Its best-effort probe also
discards the probing child's identity, which existing audio ledgers require.

Extraction adds a second constraint: ADR0042 publishes all sidecars together,
while ADR0074 completes one intent with its own one-time fence. Waiting at the
first scalar completion prevents the serial owner from applying its siblings;
using that member's fence for siblings would bypass their capability proof.
The operator explicitly approved additive probe evidence and grouped completion.

## Decision

Require mutation output evidence to retain the actual probe child's typed worker
ID and typed pre/post facts plus snapshot. Validate both probe observations against
the independently observed output and validate the attributed worker's owning
node/incarnation and probe capability. Missing historical attribution fails
publication truthfully; never substitute the mutation worker or current registry.
The existing snapshot field remains readable; the new evidence is additive.

Audio semantic lookup, claims and isolated generation/dispatch-attempt admission
precede worker execution through existing ledgers. Recorded committed operations
satisfy new tickets without another encoder dispatch.

Mutation receipt records data-only staging, verification and prepared commits while
keeping the workflow lease held. Its existing heartbeat renews lease and operation
claims throughout healthy validation/commit (ADR0050). A lease-keyed verification or
exact audio operation/attempt anchors repeated receipt handling. Retain safe original
completion identity/hash/result context in verification evidence, never the bearer.
Retain its original source location epoch and rooted identity internally and reload
that same live version/location at publication fences. Missing legacy evidence
fails truthfully; it is not inferred from the current registry or location.
Check completed replay read-only before staging; reserve and complete the original
completion key only in terminal finalization, so no committed in-progress reservation
spans a network wait.

Fenced finalization atomically records catalog identity, validated real snapshots,
audio metadata, consumed access plan, released source lease, succeeded ticket with
complete result and original completion replay. Missing lease/claim or stale scope
rolls back the whole transaction. Thus dependencies never observe raw Succeeded and
read-only public resume cannot see committed media without snapshot/ticket references.
The waiting receipt call holds no SQLite transaction. Missing required publication
fails truthfully rather than becoming a skip.
The completion route bounds each response to ten seconds using the existing
HTTP408/REQUEST_TIMEOUT envelope, below unchanged thirty-second server/client
bounds. An elapsed response window does not fail a healthy producer. The node
strictly validates that envelope and repeats the same frozen completion while
heartbeats renew the held source lease and operation claim. Ordinary network,
429 and 5xx failures retain their existing cumulative retry budget. Cancellation,
lease/claim expiry and known terminal publication stop the await through existing
settlement; no SQL transaction spans the wait. Manual commit convergence keeps
its existing bounded caller timeout and pending/recoverable result. A rejected fresh authorization may stand down only after the complete
authenticated actionable-intent projection no longer advertises that exact intent;
read failures, still-actionable conflicts and frozen-request errors stay strict.

For extraction, prepare every member intent and bind every ordered output in one
transaction using the existing operation ledger. Advertise the exact group and
claim/generation with its intents. The owner applies every member with the existing
per-member authorization, applying journal and no-replace installation, then sends
one bounded ordered group proof containing every typed intent ID, its supplied
fence and applied facts. Authenticate once and, in one transaction, validate the
exact group, live operation claim/generation, every current owner/incarnation,
root/location pin, fence and applied receipt before publishing any sidecar.
Publish all result assets/versions/locations, pre-probed snapshots, bundle members,
stream lineage, committed records and completed intents, and complete the operation
atomically. Persist the whole response through existing remote idempotency.
Scalar completion rejects grouped members; independent scalar commits keep their
existing behavior. Recovery preserves the group boundary and existing classification
and safety checks; it is never the ordinary substitute for supplied group fences.

Use the existing exact-match protocol gate 3->4 for these lockstep contracts
(ADRs0016/0075). Upgrade matched binaries before execution; old missing-evidence jobs
are not migrated/reinterpreted. No package-version bump or new negotiation.

No SQL migration, authentication redesign, in-memory request rendezvous or
control-plane byte work is introduced. Fence and claim material is redacted from
Debug/log output. This decision does not move terminal promotion or inspection.

## Consequences

Normal extraction requires the new owner/control-plane contract together. Old
missing evidence or unsupported group completion fails closed. Crash after apply
retains receipts and group membership; crash after atomic publication replays the
same IDs. The existing audio operation ledgers remain the semantic resume authority,
including complete ordered target identity and claim generations.

## Considered & rejected

- Count ticket success as publication. verified: the reduced authenticated #733
  fixture at source `4c7d7d47` returns OK with four skipped mutations and no commits.
- Publish sidecars individually. verified: ADR0042 requires the whole ordered set
  or none, including snapshots, bundle membership and stream lineage.
- Supply one fence or use stored sibling fences. verified: ADR0074 requires each
  caller's exact capability in its finalization transaction.
- Use recovery as the normal group completion route. verified: ADR0074 reserves
  recovery for classified timeout/operator recovery, with additional gate checks.
- Hold an in-memory rendezvous of independent requests. judgment: unnecessary
  liveness, resource and cancellation machinery compared with an explicit proof.
- Attribute the probe to the mutation worker. verified: node-agent `probe_output`
  uses a distinct child endpoint and the prior completion retained only its snapshot.
