# Issue #337 implementation plan

Base: `main` at merged issue #99 (`299082a`).

## Step 1 — Add the operation and lineage schema

Files:

- `migrations/0024_atomic_audio_extract_operations.sql`
- `crates/voom-store/src/migrator.rs`
- `crates/voom-store/src/repo/media/audio_extract_operations.rs`
- sibling repository tests and migration inventory tests

Behavior:

- persist one extraction operation keyed by the published operation, source
  version, and complete ordered normalized target set, plus canonical ordered
  outputs, per-generation dispatch/quiescence evidence, and finalized
  source-stream lineage;
- make `operation_key` the semantic uniqueness boundary; do not add a narrower
  `(source_file_version_id, operation_id)` constraint that would collapse
  legitimate distinct target sets;
- make nullable output `commit_record_id` unique so a legacy commit can be
  bound to at most one output; first adoption runs in one immediate transaction
  and exact key/snapshot/stream replay is idempotent while any conflict fails;
- fence mutating executors with an expiring claim and
  state/generation/claim-token compare-and-swap transitions;
- keep the fence live through verification, prepare, each promotion,
  observed-failure transition, recovery, and finalize;
- enforce monotonic state and uniqueness at the schema/repository boundary;
- atomically bind stage rows, prepare commit records, and finalize all outputs.

Red tests:

- duplicate operation/output/path/result/lineage insertion fails;
- the same planned operation/source/target set coalesces, while a different
  ordered target set creates a distinct operation;
- concurrent repository binding admits one output for a commit record; exact
  stored semantics replay while a different operation key/snapshot/stream
  conflicts;
- incomplete finalize rolls back;
- retry reads the original ordered rows.

Verification:

- `cargo test -p voom-store audio_extract`
- `cargo test -p voom-store migration_inventory`
- `cargo clippy -p voom-store --all-targets --all-features -- -D warnings`

Commit: `feat(store): persist atomic audio extraction operations`

## Step 2 — Reconcile pre-ledger singleton commits

Files:

- `crates/voom-control-plane/src/audio/mod.rs`
- `crates/voom-control-plane/src/audio/commit.rs`
- `crates/voom-store/src/repo/media/audio_extract_operations.rs`
- `crates/voom-store/src/repo/media/artifacts.rs`
- `crates/voom-store/src/repo/media/identity.rs`
- sibling and focused integration tests

Behavior:

- after selection and canonical target derivation but before operation creation
  or extraction dispatch, inspect the target and its durable owner;
- lazily adopt only a uniquely proven pre-0042 committed singleton in either
  supported payload shape: a historical payload with the legacy semantic key,
  or a #99 descriptor-bearing one-output payload with its published operation
  ID, output ID, descriptor, source version, and ordered target-set key;
- validate source version, requested pinned snapshot and stable stream,
  extraction lineage JSON, verification, paths/bytes, result identities, and
  source-bundle membership/role;
- reuse and revalidate an existing result snapshot or probe the exact committed
  target before one immediate adoption transaction;
- reconstruct the committed report around existing file/artifact/commit/member
  identities, adding only missing snapshot, operation/output, and normalized
  lineage rows;
- fail before mutation for pending, recovery-required, staged-only, missing
  target, malformed, incomplete, mismatched, or ambiguous evidence.

Red tests:

- crash after the old commit but before result snapshot, success event, and
  ticket completion: retry adopts without calling the extraction dispatcher;
- normal historical committed data returns the same identities;
- concurrent first adoption binds one output; exact replay returns it, while a
  different pinned snapshot or stable stream for the same commit fails with
  both operation keys and the commit ID;
- seeded historical and #99 descriptor-bearing one-output commits preserve
  their respective operation/output identities at every old crash boundary and
  never call the extraction dispatcher;
- every uncommitted, missing-target, malformed, mismatched, or ambiguous case
  leaves database/filesystem counts unchanged and does not dispatch.

Verification:

- `cargo test -p voom-control-plane legacy_extract_adoption`
- `cargo test -p voom-store audio_extract_adoption`
- strict control-plane/store Clippy

Commit: `feat(audio): adopt committed legacy sidecars`

## Step 3 — Resolve plural selections and durable dispatch attempts

Files:

- `crates/voom-control-plane/src/audio/mod.rs`
- `crates/voom-control-plane/src/audio/dispatch.rs`
- `crates/voom-control-plane/src/audio/selection.rs`
- `crates/voom-control-plane/src/audio/stage.rs`
- `crates/voom-control-plane/src/audio/worker_contract.rs`
- `crates/voom-control-plane/src/audio/workflow.rs`
- `crates/voom-control-plane/src/worker_process.rs`
- `crates/voom-worker-protocol/src/http/client.rs`
- `crates/voom-worker-protocol/src/http/server.rs`
- `crates/voom-worker-protocol/src/http/streaming.rs`
- `crates/voom-worker-protocol/src/wire/envelope.rs`
- `crates/voom-ffmpeg-worker/src/handler.rs`
- `crates/voom-cli/src/cli.rs`
- `crates/voom-cli/src/commands/media/artifact.rs`
- `crates/voom-events/src/payload/artifact.rs`
- sibling, worker-conformance, and CLI tests

Behavior:

- return a non-empty ordered selection vector;
- use each planned `name_suffix` without re-sanitizing it;
- create stable operation-owned staging and target paths;
- reconcile planned-state partial/complete worker leaves by deleting only
  validated quiesced regular-file leaves, then advance a durable
  dispatch generation under the live writer claim;
- isolate generations in distinct attempt directories and reject stale
  completions by generation/claim CAS; only the winning generation can become
  eligible for the later all-output staged transaction;
- commit worker epoch, idempotency key, and every intended attempt leaf before
  sending; accept terminal evidence only after the request handler, provider
  children, and output writers/file descriptors have exited;
- pass a typed dispatch context/outcome carrying operation key, generation,
  worker ID/epoch, idempotency key, and terminal lifecycle proof through the
  real runtime and bundled-worker transport;
- add `voom artifact acknowledge-audio-extract-attempt` through the existing
  JSON envelope; atomically record quiescence only for the exact operation key,
  generation, worker ID/epoch, idempotency key, and persisted path set of a
  quarantined planned attempt whose leaves are not bound as staged output;
  allow that attempt to be the current planned generation when no live writer
  claim exists, then require a new claim before exact cleanup/generation
  advance; reject live-claim, staged/bound, wrong-attempt, non-quarantined, and
  stale acknowledgement and append an audit event;
- build and validate one plural request/result and every attempt output.

Red tests:

- two descriptors stay provider-index ordered;
- normalized path collisions fail before directory/file creation;
- missing/extra/reordered/malformed result fails before durable staging;
- worker writes member 0 then fails: clean exact owned leaves and redispatch
  with a new generation;
- crash after worker success before the staged transaction: replay the same
  key and preserve the same generation's complete leaves for the later staged
  transaction;
- concurrent resume admits one claimant, and a delayed old-generation result
  completing after the new generation is staged cannot affect bound staging or
  target paths; stale cleanup/redispatch requires cached terminal,
  cancellation/process-exit, or explicit post-isolation operator
  acknowledgement, never TTL or database retirement alone;
- host-crash recovery replays the same generation/idempotency key to obtain the
  cached terminal result;
- crash immediately before send and immediately after send both recover through
  the same already-committed attempt; a provider child that outlives its
  request handler blocks terminal/quiescence evidence and redispatch;
- database worker retirement or elapsed time without quiescence blocks cleanup
  and redispatch; observed process exit or explicit post-isolation
  acknowledgement unlocks them without changing operation/output identity;
- wrong-attempt, stale, staged/bound, non-quarantined, or live-claim
  acknowledgement fails, emits no acknowledgement, and never dispatches; the
  exact quarantined current planned generation with no live claim is audited,
  then a new claimant cleans only its leaves and advances;
- repository CAS permits exact cleanup of either an obsolete unbound quiesced
  generation or the current generation when the operation is `planned`, every
  persisted leaf is unbound, no other active attempt exists, and the new
  claimant owns the live claim; it rejects every staged/bound generation;
- identical observed facts for distinct outputs remain valid.

Verification:

- `cargo test -p voom-control-plane audio::selection`
- `cargo test -p voom-control-plane audio::stage`
- `cargo test -p voom-control-plane audio::worker_contract`
- `cargo test -p voom-control-plane audio::dispatch`
- `cargo test -p voom-control-plane audio::workflow`
- `cargo test -p voom-ffmpeg-worker extract`
- `cargo test -p voom-cli artifact`

Commit: `feat(audio): resolve ordered extraction output sets`

## Step 4 — Stage and verify the complete set

Files:

- `crates/voom-control-plane/src/audio/mod.rs`
- `crates/voom-control-plane/src/audio/commit.rs`
- `crates/voom-control-plane/src/audio/events.rs`
- sibling tests

Behavior:

- load/create the operation ledger and short-circuit committed retries;
- keep the operation `planned` through complete result/file validation and
  pre-probe every member;
- use one claim/generation-predicated transaction to bind the winning
  generation's immutable leaves, create every staging handle/location, persist
  every result fact and normalized probe payload/worker attribution, and
  transition `planned -> staged`;
- verify all members and reuse successful verification rows on resume;
- prevent prepare when any member fails verification.

Red tests:

- member-N probe/verifier failure creates no
  target/pending commit/result snapshot/bundle/lineage;
- retry of a staged operation reuses artifact identities;
- malformed/partial output creates no artifact rows;
- crash immediately before the staged transaction replays the same dispatch key
  and binds the same generation; repository invariants reject an observably
  incomplete staged operation.

Verification:

- `cargo test -p voom-control-plane audio::`
- `cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`

Commit: `feat(audio): stage and verify extraction output sets`

## Step 5 — Prepare, promote, finalize, and recover atomically

Files:

- `crates/voom-control-plane/src/audio/commit.rs`
- `crates/voom-control-plane/src/artifact/fs.rs`
- sibling filesystem observation tests
- store repository methods from Step 1
- sibling and integration tests

Behavior:

- prepare every pending commit under one gate/transaction;
- on every recovery entry, recheck the source-lineage commit-safety gate in the
  successor-owned transaction before any missing-target promotion or finalize,
  and persist the evaluated lease evidence consistently for all members;
- promote members add-only in order;
- renew/check claim immediately before each install;
- on claim loss, fence the loser from all writes and require the successor to
  atomically mark the operation and every active member commit recoverable
  before continuing;
- record target ownership, regular-file mode, device, inode, link count, size,
  and checksum after install; reopen without following symlinks and require
  those facts immediately before finalize;
- create the Unix parent as host-owned mode `0700`, record its device/inode, and
  require the same parent ownership/type/mode/identity plus host-owned regular
  mode-`0400` targets immediately before finalize;
- use a typed Unix observation for UID, mode, device, inode, link count, size,
  and hash; separate pure comparison tests from real no-follow,
  mode/symlink/inode tests that require no privileged ownership changes;
- recover missing/exact targets and reject mismatched collisions;
- finalize every identity, result media snapshot, bundle member, commit record,
  and lineage row in one transaction;
- return a committed ledger unchanged on retry.

Red tests:

- fail after prepare, after member N, and after all promotions;
- with all target bytes already promoted, add a new blocking lease and a live
  non-blocking advisory lease after prepare: recovery cannot finalize; after
  releasing only the blocker, every member completion records the advisory
  lease ID from one shared recovery-gate evaluation identity, distinct from the
  prepare evidence;
- expire the claim after prepare and between members while a successor resumes;
- expire the claim immediately after the pre-install check and prove add-only
  exact-byte collision handling remains safe;
- prove the claim loser writes no recovery transition and the successor marks
  every active member recovery-required before continuing;
- mutate a promoted target in place before finalize and prove ownership,
  mode/inode/link/size/hash drift fails closed;
- alter parent ownership/mode/inode or substitute symlink/non-directory
  components and prove finalize fails closed;
- assert every member commit is recovery-required after an observed failure;
- no visible result rows before finalize;
- recovery returns the same ordered identities;
- finalized retry leaves database and filesystem counts unchanged.

Verification:

- `cargo test -p voom-control-plane audio::commit`
- `cargo test -p voom-control-plane --test audio_extract_flow`
- strict control-plane/store Clippy

Commit: `feat(audio): publish sidecars as one recoverable commit`

## Step 6 — Expose ordered lineage-complete reports

Files:

- `crates/voom-events/src/payload/artifact.rs`
- `crates/voom-control-plane/src/audio/events.rs`
- `crates/voom-control-plane/src/audio/mod.rs`
- compliance run view/coordinator promotion queries
- CLI snapshots/fixtures and payload inventory if required

Behavior:

- add ordered success/failure/event and execution report items;
- retain historical singular first-output fields;
- attach ordered extraction results to compliance execute/run reports;
- promote every sidecar result location;
- read historical event/ticket result JSON.

Red tests:

- event old/new serde compatibility;
- ticket and CLI envelope includes every ordered descriptor, path, artifact,
  result snapshot/facts, bundle member, and lineage edge;
- promotion enumerates plural locations with scalar fallback;
- resume under a fresh job/ticket resolves the same semantic legacy operation
  and returns unchanged artifact/commit/target identities;
- identical historical semantics at different targets produce distinct keys;
  repeated semantics at one add-only target intentionally coalesce.

Verification:

- `cargo test -p voom-events artifact_audio_extract`
- `cargo test -p voom-control-plane compliance`
- `cargo test -p voom-cli compliance`
- relevant insta snapshot review

Commit: `feat(report): expose ordered extraction lineage`

## Step 7 — Generated-media end-to-end and recovery matrix

Files:

- `crates/voom-control-plane/tests/audio_extract_flow.rs`
- focused test support only where necessary

Behavior:

- execute real one- and multi-stream generated media;
- inspect produced media facts and durable lineage;
- exercise collisions, malformed/partial output, retry, and commit crash
  boundaries.

Red tests:

- each acceptance case fails on the pre-#337 singleton host or incomplete
  commit model.

Verification:

- `cargo test -p voom-control-plane --test audio_extract_flow -- --nocapture`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Commit: `test(audio): verify atomic plural extraction media`

## Step 8 — Review and full guardrails

- Re-read the complete branch diff for scope and compatibility.
- Run adversarial review against `main`; fix every defensible finding and
  re-review.
- Run security-focused review of path ownership, symlink/collision handling,
  worker result trust, and recovery.
- Run simplification review and apply only behavior-preserving reductions.
- Run `just ci`.
- Rebase on the latest `origin/main`, rerun `just ci`, push, and open a PR with
  `Closes #337`.
