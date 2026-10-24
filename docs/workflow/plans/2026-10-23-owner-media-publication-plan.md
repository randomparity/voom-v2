# Implement owner media publication (#733)

Scope token q733-29de38e1 (approved resumption; same charter); BASE_BRANCH main; branch feat/owner-publication-733.
Spec: ../specs/2026-10-23-owner-media-publication-design.md; ADR0109.
Expected implementation size: 8000–9500 changed lines (L).
Build reconciliation measured 6982 changed lines before final negative-proof additions.
Exact source/probe association, truthful terminal failure and explicit validation/catalog
helper boundaries exceeded 4500–6500; required surface remains unchanged.
The final estimate also includes exact persisted-provenance correlation, isolated
retained-database regressions and restoration of healthy completion lifetime through
strict bounded response windows. These are required correctness dependencies of
the unchanged approved outcome. The prior 6500–8500 estimate missed that work.
The estimate includes evidence/group contracts, pre-dispatch audio admission,
held receipt/atomic source completion, API/node/commit integration and regressions.
Review exposed absent audio lifecycle callers; their data-only responsibilities plus
group proof exceed the earlier 1800–3000 estimate. This corrects task coverage,
not scope or the fixed denominator. Fixed denominator
remains 1000 from the externally frozen L assessment; no estimate-based resizing.

## Execution and dependencies

One coupled PR. Evidence/types precede receipt/publication and group integration;
those integrate before real chain/extraction acceptance. Use Cast for coupled
orchestration or Party only for isolated file ownership under the interfaces below.
No task may change schema, weaken strict report decoding or absorb excluded byte work.
Every public capability-carrying type uses redacted Debug; all IDs remain newtypes.
All SQLite reads are untrusted and all multi-repository work shares a named tx helper.

## Task 1 — Shared evidence and node probe preservation

Files: worker-protocol operations/dispatch.rs (or sibling publication.rs + exports),
node-agent media.rs and sibling tests; payload inventory/scope list.

Interfaces: define strict shared `OwnerProbeEvidence { worker_id: WorkerId,
result: ProbeFileResult }`. Add optional `probe: Option<OwnerProbeEvidence>` to the
existing serialized observed-output content while retaining optional snapshot.
Define shared strict group identity/member proof/response types consumed by Tasks3/4;
identity carries operation ID, claim lease/token, generation and expected count;
per-open member carries ordinal. Member proof carries ArtifactCommitIntentId, supplied
fence and applied size/hash facts. Set shared MAX_COMMIT_GROUP_OUTPUTS=128; reject an oversized extraction before worker dispatch with existing configuration error, and reject an oversized proof before mutation.

Verification:
- Mode: focused-test; media_test.rs `mutation_requires_attributed_matching_probe`:
  red is Complete despite missing/foreign/mismatched pre/post probe; green command
  `cargo test -p voom-node-agent mutation_requires_attributed_matching_probe`.
- Mode: focused-test; worker-protocol sibling tests `owner_publication_wire_is_strict`:
  red is absent types/unknown-field acceptance; green command
  `cargo test -p voom-worker-protocol owner_publication_wire_is_strict`.

Implementation algorithm: capture endpoint credentials.worker_id at actual dispatch;
strict-decode ProbeFileResult, require pre/post size/hash equal independently observed
output, require valid snapshot stream object/index identity, preserve typed receipt
and old snapshot. Missing endpoint/result fails mutation with existing truthful
malformed/verification classification. Serialize only after all outputs validate.
Register newly typed durable JSON read boundaries in existing inventory and scope.

## Task 2 — Admission, held receipt and atomic source completion

Files: remote_execution/acquire.rs/complete.rs and publication siblings; artifact
media-publication modules; store remote_idempotency/audio/tickets narrow transaction
seams; artifact/commit prepare/finalize integration; coordinator guard/tests.

Interfaces: strict persisted `RemoteMediaCompletionContext` holds typed node,
incarnation, worker, lease plus original idempotency key/hash/result, NO bearer.
`admit_remote_media_in_tx(cp,tx,lease,ticket,now)` resolves exact semantic ledger/
claim/attempt/dispatch, or satisfies recorded committed work without encoding.
`stage_remote_media_in_tx(cp,tx,input,ticket,plan,now)` validates and creates/reuses
staging/prepared references while source lease stays HELD.
`finish_remote_media_in_tx(cp,tx,context,published_result,now)` validates original
source fences, consumes plan, releases lease/succeeds ticket and reserves/completes
original remote replay in the caller's finalization transaction. Add store
`lookup_replay_in_tx(tx,RemoteIdempotencyInput)->Result<Option<RemoteMutationReplay>,VoomError>`
with SAME hash/status/response-corruption checks as current reservation reader;
missing returns None, existing in_progress remains Conflict. No new persisted state.

Verification:
- Mode: focused-test; `audio_admission_precedes_dispatch` and
  `committed_audio_admission_reuses_without_dispatch`: RED missing claim/attempt or
  duplicate encoding; `cargo test -p voom-control-plane audio_admission_`.
- Mode: focused-test; `media_receipt_` tests: missing/foreign/mismatching probe cannot
  release or leave artifacts; exact duplicate context reuses one prepared set;
  `cargo test -p voom-control-plane media_receipt_`.
- Mode: focused-test; remote_idempotency_test.rs `lookup_replay_`: same hash/inprogress/
  corruption outcomes with no row insertion; `cargo test -p voom-store lookup_replay_`.
- Mode: focused-test; `media_finalization_is_atomic_with_ticket`: injected failure
  after catalog writes before source release rolls back catalog/snapshot/lineage/
  commit/ticket/replay; `cargo test -p voom-control-plane media_finalization_`.
- Mode: focused-test; `healthy_publication_renews_original_claim` and
  `expired_publication_cannot_revive_claim`, real Tokio + ManualClock cross original
  TTL with/without existing heartbeat; commands
  `cargo test -p voom-control-plane healthy_publication_` and
  `cargo test -p voom-control-plane expired_publication_`.
- Mode: focused-test; `join_waits_for_every_parent_publication`: declarative edge and
  policy A/B->C with B held before finalization and A complete, C undispatchable;
  `cargo test -p voom-control-plane join_waits_`.

Admission: parse pinned source/selection; extraction key is existing length-prefixed
SHA256 over sourceversion/snapshot/operation/complete normalized ordered targets;
synthesis key synthesize:<sourceversion>:<planned-operation>. Grounded historical
helpers at a8505842^ preserve semantics. Match every descriptor/role/snapshot/target.
Return committed recorded work without worker; reconcile staged/prepared generation;
refuse active ambiguous/unquiesced attempts. Claim exact lease/generation and record
worker epoch/idempotency/private attempt paths BEFORE send. Persist actual rendered
paths with attempt so dispatch and receipt agree; never clear prior bound attempt.
Satisfy known work through ordinary lease release in acquire tx, then return Idle,
not a fake worker grant or result. Pre-dispatch oversized groups fail configuration.

Receipt: authenticate and read-only replay probe before held validation; validate
stored dispatch/attempt/source/facts/access epochs/ordered output count+locators,
raw strict worker result, actual attributed probe capability/ownership/incarnation/
facts/snapshot. Reject reserved markers, stripping only agent-added access/observations
for worker decode. Validate ALL before mutation. Create/reuse handles/locations/
data-only verification/events and prepare/bind commits in ONE tx; store exact safe
completion context. Scalar verification lease uniqueness or exact audio operation/
attempt is re-entry anchor. Context mismatch rejects. Preserve replayable preflight
error recording; roll back staged domain/database failures without orphan state.

Keep source lease HELD/plan Selected while awaiting publication outside SQLite
locks. Bound individual completion responses (existing REQUEST_TIMEOUT), not the
healthy producer lifetime. Strict validated timeout windows repeat the frozen
request under heartbeat/cancellation, preserving the cumulative ordinary failure
budget. Validate source/operation claims and terminal intent states between polls.
Existing run_lease heartbeat through settle_lease renews claims; no post-release
lifecycle. Finalization records real normalized snapshots and audio metadata BEFORE
source release; finish_remote_media_in_tx writes full result/dependency events and
original completion replay in SAME tx. Original waiter returns stored replay.
Prepare uses handle's exact source-location pin, not unique-live lookup polluted by
sibling staging pins. Manual commits without media context keep existing behavior.
No post-success result CAS/enrichment is needed.

Audio: derive exact pinned selections via existing selection.rs; preserve ledger
claim/generation/attempt predicates, original bundle and descriptor role. Expose
narrow _in_tx variants of existing pool algorithms, without duplicated policy.
Synthesis real appended indices/codec/channels/tags/dispositions come from validated
worker/probe mapping; stable descriptor IDs are assigned only to proven physical
indices. Complete synthesis lineage and strict report atomically with result.
Extraction Published report uses real output/bundle/lineage IDs, no fabricated legacy
placeholders. Failed parent CAS rolls back all preceding child writes.

## Task 3 — Exact extraction group prepare, finalize and recovery

Files: artifact/commit group module + tests, intent.rs/finalize.rs/recovery.rs/prepare.rs;
store audio_extract_operations.rs and artifact intent repositories as needed for
checked typed group projections. No SQL in control-plane.

Interfaces: group prepare takes exact staged operation record and live claim,
prepares/binds ALL pending commits in one caller tx. `remote_complete_commit_group`
takes node/token/incarnation/idempotency/request hash plus shared group proof and
returns ordered existing result IDs. Store group projection resolves by exact
operation/commit ID and decodes all IDs/enum/state/JSON before mismatch/absence
classification. Existing group output commit_record_id UNIQUE is the binding.
`complete_mutation` scalar path rejects any prepared extraction-group member before
changing state. `converge_intent_in_tx` retains standalone behavior and delegates
only exact known group membership to group finalization, never infers from paths/codecs.

Verification:
- Mode: focused-test; group_test.rs `group_completion_requires_exact_all_member_proofs`:
  red partial scalar catalog publication; command
  `cargo test -p voom-control-plane group_completion_`.
- Mode: focused-test; group_test.rs `group_failure_rolls_back_every_catalog_row` and
  `group_recovery_preserves_atomicity`: injected failure after first sidecar writes
  leaves no sidecar version/snapshot/member/lineage or completed intent; repeat after
  all applied receipts converges once. Commands
  `cargo test -p voom-control-plane group_failure_` and
  `cargo test -p voom-control-plane group_recovery_`.

Algorithm: normal auth/replay reservation; reject invalid bounds/duplicate/order;
load exact operation and complete ordered outputs, verify supplied operation/claim/
lease/generation and live expiry, every prepared commit/intention/source/output pair,
all same current owner/incarnation, root/location epochs, supplied fences, original
or accepted recovery receipt and request facts. Validate entire set before mutations.
Then use existing `record_verified_sidecar_commit_rows_in_tx` per output preserving
source -> produced_from; record preprobe snapshot, bundle member and output lineage;
retire staging rows; complete all intents/records and operation with live claim CAS;
finish original source lease/ticket/accessplan/completion replay and persist
ordered group response/idempotency in SAME tx. Missing/extra/reordered/cross-op/
wrong fence/epoch/claim/receipt rejects the entire set. Concurrent calls serialize.
Recovery classifies every member and gate before group convergence or leaves group
unpublished/recovery-required. Preserve existing quiescence before generation changes;
no routine recovery as normal completion and no scalar subset recovery.

## Task 4 — API/node group driving and authenticated real proof

Files: API commit.rs and route tests; node client.rs/commit.rs and siblings;
scripts/issue-733-owner-publication-regression.py plus justfile test-owner-publication-real recipe, with small generated media;
coordinator/finalize.rs/tests only to reject required missing publication.

Interfaces: optional group metadata in open-intent projection; dedicated group complete
route using existing header auth/hash/envelope conventions. Node client trait gets
matching typed call; standalone scalar endpoint remains unchanged. Node groups by
exact identity/ordinal/count; apply all members with existing journaling/no-replace
before submitting ONE frozen group request carrying ALL fences and observed facts.
No incomplete group is truncated or turned into scalar work. Retain per-member proof
for same-incarnation retries; fresh incarnation without authorization cache follows
existing recovery standdown. Capability material stays redacted in every Debug.

Verification:
- Mode: focused-test; node commit_test.rs `extraction_group_applies_all_before_complete`:
  red scalar completion interleaves before sibling apply; command
  `cargo test -p voom-node-agent extraction_group_`.
- Mode: focused-test; API tests `group_complete_auth_and_exact_proof`:
  wrong bearer/incarnation/body/set rejects with existing envelope and no mutation;
  command `cargo test -p voom-api group_complete_`.
- Mode: focused-test; real authenticated regression initially reproduces the retained
  exact RED assertion with baseline code, then checks remux->HEVC->EAC3->AAC2 chain,
  durable version/location/snapshot and correct final verify target, final output
  physical facts, separate plural extraction group, and explicit resume with unchanged
  output bytes/mtime and no new mutation tickets/commits/lineage. Green command `just check-owner-publication <fresh-private-workdir> extract`; record prerequisites/results; keep real-media opt-in clearly named.
- Mode: focused-test; coordinator finalize test `required_mutation_without_publication_fails`:
  red planned mutation becomes Skipped; command
  `cargo test -p voom-control-plane required_mutation_without_publication_`.

## Public crash/resume and protocol compatibility

Files: coordinator resume/preparation tests (production readers remain read-only);
core protocol constant and immediate consumers/fixtures/snapshots.
- Mode: focused-test; public `compliance execute --resume-job-id` after lost terminal
  response proves complete original catalog/ticket/snapshot and no new encoder,
  artifact/commit/snapshot/lineage. Preterminal injected failure proves no committed
  partial state. Commands `just check-owner-publication <fresh-private-workdir> extract` and
  `just check-owner-publication <fresh-private-workdir> chain`.
- Mode: focused-test; exact-version tests reject old3/new4 BOTH directions before
  execution: `cargo test -p voom-worker-protocol handshake_` and
  `cargo test -p voom-node-agent schema_mismatch_fails_before_child_dispatch`. Change only PROTOCOL_VERSION3->4 and
  immediate asserted consumers; no package bump, SQL migration or negotiation.

## Design review dispositions

Two valid independent passes: six high in-surface findings, four distinct concerns,
all accepted-fixed in this design. Join/claim findings: defer Succeeded/release until
atomic terminal publication. Pass1 public-resume finding: eliminate commit-before-
enrichment window and test public resume. Pass2 predispatch finding: existing-ledger
admission before send. No third confirming prose pass; scope audit follows.
Suppressed concerns: exact installed bytes may remain unpublished (ADR0042), and fresh
incarnation lacking original fences may require classified/operator recovery (ADR0074).
No deferrals, rejected findings or follow-up candidates.

## Integration, review and handoff

Run focused tests after each unit, then just fmt/source guards/lint. Freeze checkout
for every hook/test run; no concurrent builds against the shared target. Apply
adversarial branch review and security review at required deep capability, simplify
only actual recent diff, run one final just ci on the final candidate before push,
commit with normal prek hooks and command-local operator identity. Deliver green CI,
record verified forge review and commit-bound MERGE-READY; root alone merges.

No deferrals or follow-up candidates; the two disclosed suppressions above remain. Cumulative
fixture setup attempts2 (one setup error, one exactRED); liveness probes0/replacements0;
review/full-suite/CI counts0. Update private progress at design/build/review/ship seams.

## Final verification mapping

Named scenario filters above describe planned intent; they are not claims that a
filter with zero matching tests ran. The authenticated `check-owner-publication`
chain/extract/abort fixtures implement admission, correlated receipt, all-member
authorization-before-apply, grouped negatives/replay/operator recovery, physical
lineage/resume, original-TTL heartbeat, terminal failure/shutdown, semantic reuse
with remaining ready work and acquire replay. Focused actual tests additionally
cover `lookup_replay_`, `required_mutation_without_publication_fails`,
`synthesis_probe_comparison_uses_exact_admitted_companion_projection`,
`standalone_audio_dispatch_does_not_enter_workflow_operation_admission`,
`rejected_fresh_authorization_stands_down_only_when_no_longer_actionable` and
`rejected_authorization_does_not_treat_listing_failure_as_absence`.
Old/new compatibility uses the actual nested agent dispatch entry and HTTP worker
handshake. The immutable baseline decoder has the same exact equality gate with
expected version 3, while the new renderer/decoder expects 4. This does not claim
a node/API registration version check or full mixed-binary deployment acceptance.


Healthy-publication lifetime correction: actual healthy twelve-second owner delay
reproduced premature producer failure at the reused manual ten-second deadline.
ADR0050/0109 require healthy lease/claim lifetime through commit. The bounded
continuation therefore separates the existing HTTP response timeout from terminal
producer settlement, with strict timeout envelopes, unchanged generic error budget,
cancellation and authoritative expiry. This restores the reviewed outcome without a
new wire state, error code, SQL schema or byte owner. Focused tests exercise more
than five response windows, malformed/mismatched envelopes, accumulated failures,
heartbeat/cancellation/shutdown/fencing, and the real delayed owner plus resume.
