# Owner media publication (#733)

## Authority and problem

Frozen scope: #733 token q733-60db64db, WORK:SCOPE6383576036. Operator approved
original repair/exclusions, “Approve additive probe evidence”, and “Approve grouped
completion”. ADR0109 records the additive decisions. Root confirmed existing exact
protocol gating 3->4 is a necessary compatibility consequence.

At 4c7d7d47, the reduced authenticated real fixture returns four successful mutation
tickets but skipped phases, original verification, no commits and no output. Receipt
releases raw success before publication. Declarative edges, policy joins and public
resume also consume that success; settlement alone cannot enforce publication.

## Scope and ownership

Restore data-only publication through existing artifact/audio repositories and fenced
owner commits. Preserve ADR0042 plural atomicity, ADR0026 synthesis lineage, ADR0050
held-lease/claim lifetime and node bytes, ADR0074 member capabilities and ADR0075 media
envelopes. No byte-ownership transition or new runtime state store. Scalar manual
commits and existing durably complete synthetic results retain behavior/validation.
Missing historical probe attribution is never guessed or automatically upgraded.

Excluded: SQL migration or unrelated auth/protocol/routing/fence/ownership redesign;
#528 terminal promotion/reclaim/inspection bytes; #497/#615–618 containment/config;
#561 general accounting; #566 broad encoder chaos; #339 retained canary/capacity/
full-library; operator production/retention/cleanup/source mutation.

## Success

1. Required mutations atomically publish catalog/snapshot/ticket evidence before
   either existing dependency reader observes Succeeded.
2. Later main-file phases consume prior committed chain tips; final verification
   targets produced media. Companion physical facts and synthesis lineage/reports
   remain truthful.
3. Extraction retains exact semantic operation/output/role/source identity, private
   attempts/claims and all-or-none sidecar catalog publication.
4. Small authenticated fixtures prove physical chain/output and public resume without
   repeated encoders, commits, snapshots, lineage or overwrite.
5. Missing evidence or required publication fails loudly. Fully validated sidecar-only
   phases may retain existing unchanged-main-chain Skipped summaries, with explicit
   published sidecar outputs; this is not missing-publication success. Existing
   ordered-sidecar finalize tests preserve that distinction without a SQL change.

## Evidence and pre-dispatch admission

Add strict shared OwnerProbeEvidence: actual WorkerId and ProbeFileResult with pre/
post facts and snapshot. Retain optional snapshot for compatibility and require exact
agreement when both occur. The node captures actual child credentials, compares both
probe observations to independently observed output, and fails mutation if evidence
is unavailable. Validate snapshot shape and unique numeric stream indices/IDs;
normalize stable descriptor IDs only after mapping real observed physical indices.

Before audio execution, remote acquire's existing transaction resolves exact operation
against pinned snapshot/selection and complete normalized targets. Reuse synthesis
key synthesize:<source>:<planned-operation> and extraction's length-prefixed SHA256
source/snapshot/operation/ordered-target key (accepted historical code a8505842^).
Recorded committed work satisfies the new ticket without encoding. Reconcile staged/
prepared work from its existing generation; never clear bound outputs or start a
competing generation without existing quiescence proof. Acquire current lease claim
and record worker/epoch/idempotency/generation/private attempt paths BEFORE send.
The durable dispatched envelope and attempt agree on exact paths. No filename/codec
inference or job/ticket-only semantic key. Shared MAX_COMMIT_GROUP_OUTPUTS=128 is
checked at admission before dispatch and at group API; never truncate.

## Held receipt and atomic terminal publication

Remote completion authenticates/revalidates incarnation and checks completed replay
read-only before held-lease validation. Validate stored dispatch and exact attempt,
owner access/root epochs, source IDs/location/facts, complete ordered outputs, strict
worker report, actual probe ownership/incarnation/capability, facts and snapshot.
Reject caller publication markers. Strip only agent access/observation additions
before strict worker decode. Validate ALL before creating durable staged state.

One receipt transaction creates/reuses immutable handles with existing source lineage,
exact rooted staging locations, data-only successful verifications/events and prepared
commits/ledger bindings. Remux/video uniqueness uses existing verification workflow-lease;
audio uses exact operation/output/attempt identity, preserving both verification
workflow fields as NULL for existing semantic-operation reuse. Never rewrite an old
bound verification to claim another ticket's provenance. Persist safe original completion context: node,
incarnation, worker, lease, original idempotency key/hash and exact result; NO bearer.
The internal context also retains the original source location epoch and rooted
identity; every publication fence reloads the same live version/location. Missing
legacy context evidence fails truthfully. Duplicate context must match before reuse. Prepare uses the handle's exact source-
location pin so sibling staging pins cannot make the source ambiguous. Any failure
rolls back all receipt-created state, without orphan artifacts/reservations.

DO NOT consume access plan, release source lease, mark ticket Succeeded, complete its
idempotency key or expand work at this stage. Wait outside SQLite locks under
live source/claim/terminal checks. Bound each API completion response window to ten
seconds using existing HTTP408/REQUEST_TIMEOUT, preserving its truthful message
through the global thirty-second boundary. The completion client strictly decodes
that envelope and repeats the frozen request without consuming or resetting the
ordinary cumulative network/429/5xx error budget. Invalid timeout envelopes remain
errors. Completion settlement observes existing cancellation/shutdown/lease fences;
healthy apply/copy/hash work has no ten-second producer deadline.
Existing node run_lease heartbeat remains running through settle_lease; existing
held-lease heartbeat renews operation claims. No post-release renewal authority.
True expiry never revives a stale claim; retries reuse exact durable attempts.

Fenced scalar/group finalization validates source lease/claim liveness and original
completion context in addition to commit guards, then ATOMICALLY creates result
catalog and validated real snapshots; finishes audio operation/lineage/report state;
retires staging and completes records/intents; consumes access proof; releases source
lease and succeeds ticket with full result; reserves/completes ORIGINAL completion
replay. Manual commits without media context retain their existing path.

One data-only source-completion helper preserves existing result/event shapes and
checked-ID failure ordering. A narrow read-only replay lookup uses existing hash,
status and response validation; reserve+complete only occurs in terminal transaction.
No committed in-progress completion reservation spans network wait. Replayable
preflight errors retain terminal error recording. Waiting original request returns
exact persisted replay after finalization.

## Explicit extraction grouping

Prepare every member/binding in one transaction. Optional open-intent group metadata
carries exact operation ID, claim lease/token, generation, ordinal and total count.
Authenticate projection; redact claim/fence material in Debug/logs. Only complete
same-owner groups are actionable. Apply ALL through existing per-member authorize,
applying journal, no-replace install and outcome before ONE frozen group request
carries every typed intent ID, supplied fence and applied facts. Preserve frozen
member requests/proofs for same-incarnation retries. Fresh incarnation without its
fences follows existing recovery standdown.

Normal auth/replay surrounds one read-then-write final transaction. Decode entire
exact operation/output/intent/receipt set before classification. Reject bad bound,
missing/extra/duplicate/reordered/cross-operation members, changed claim lease/token/
generation/expiry, any owner/incarnation/root/location drift, wrong fence or facts.
Validate ALL first. Then use existing sidecar row capability with original produced-
from source, persist every preprobe snapshot, bundle role and stream lineage, complete
all records/intents/operation and source ticket/accessplan/replay together. Concurrent
requests serialize; same-key replay returns same ordered IDs. Scalar completion for
grouped members rejects before mutation; standalone scalar behavior is unchanged.

Recovery classifies WHOLE operation from existing receipts and rechecks every scope/
safety gate. It converges complete group under existing authority or leaves all
catalog rows unpublished/recovery-required; preserve live-claim and quiescence/
generation rules. Recovery is not the normal substitute for supplied group fences.

## Resume, compatibility and failure model

Atomic catalog/snapshot/source completion removes commit-before-enrichment window.
Failure before source release must roll back every new catalog/lineage row. Lost
response after commit followed by PUBLIC resume must produce no repeated work.
Existing dependency and read-only resume readers remain sufficient; no post-success
CAS or mutation inside prepare_resume. Legacy missing-evidence mutation success is
rejected truthfully rather than reinterpreted.

Protocol4 retains the existing exact equality gates. The current agent rejects a
schema3 media dispatch before source byte access or child dispatch; the immutable
baseline schema3 decoder rejects schema4 envelopes. Worker HTTP handshakes likewise
require exact agreement. Matched API/agent/child binaries remain an operator rollout
requirement; this does not promise node/API registration-level version rejection or
full mixed-binary deployment acceptance. Package version, SQL schema and negotiation
remain unchanged.
Register strict new durable JSON subpayload boundaries in inventory/scope. Upgrade
matched binaries before new execution; do not reinterpret retained protocol3 jobs.

Deployment: matched authenticated node/API with real-storage SQLite, separate tiny
synthetic media. Required failures: missing/malformed probe; wrong worker/owner/
incarnation; stale root/location/claim/generation; bad group; duplicate/lost requests;
original TTL crossed with healthy heartbeat and true expiry without heartbeat;
encoder failure; occupied/drifted target; crash before receipt, after partial apply,
before terminal transaction and after committed response; concurrent receipt/complete.
Each reuses exact durable work or fails without dependency advancement/partial catalog.
Applied targets without catalog may remain (ADR0042); fresh-incarnation fence loss
can require classified/operator recovery (ADR0074). No source cleanup.

## Validation

Focused admission/receipt/replay/expiry/atomic rollback/join/resume/group/node/API
regressions; existing artifact/audio suites; real chain/plural extraction/public
resume physical proof; final just ci, normal hooks, whole-branch/security reviews,
PR CI and commit-bound handoff. Root alone merges; no #339/production run here.
