# Plan: Fenced node-local verification and commit intents (#422)

Goal: staged artifact commits execute on the storage-owner node behind a
durable fenced intent; the control plane never opens staging or target bytes.
Architecture: new `artifact_commit_intents` table 1:1 with
`artifact_commit_records`; node pulls work over authenticated routes; receipts
journal before mutation; completion consumes a one-time fence inside the
finalize transaction. Spec:
`docs/workflow/specs/2026-08-21-fenced-node-local-commit-intents-design.md`;
ADR: `docs/adr/0074-fenced-node-local-commit-intents.md`.

Stack: Rust workspace (tokio + sqlx + axum), versions inherited from the root
`Cargo.toml` (`version.workspace = true`; no new external dependencies).

## Global Constraints

- Sibling test layout: `#[cfg(test)] #[path = "foo_test.rs"] mod tests;` — no
  inline `#[cfg(test)] mod tests { }` in `src/` (`just check-test-layout`).
- Every durable JSON column deserialized into a typed struct carries
  `#[serde(deny_unknown_fields)]`; register each new typed column in
  `docs/payload-contract-inventory.md` + `scripts/payload-contract-scope.txt`
  (`just check-payload-deny-unknown`, CI-gated via `just ci`).
- Treat values read from SQLite as untrusted: checked conversions via
  `voom_store::repo::common` (`i64_from_u64`, `u64_from_i64`,
  `parse_iso8601`, …). Corrupt storage is `VoomError::Database`.
- Error `code` strings are public contract; add variants/strings, never
  repurpose.
- Workspace lints: pedantic on; `panic!/unwrap/expect` denied; zero warnings.
- Never pair `tokio::time::pause()` with a real `SqlitePool`; drive domain
  time via injected `Clock` (`ManualClock`).
- Guardrail suite: `just ci` (fmt-check, lint, check-test-layout,
  check-paused-time-db+selftest, check-control-plane-sql-boundary,
  check-payload-deny-unknown+selftest, test --all-features, doc, deny, audit,
  check-adr-index). Run focused `cargo test -p <crate>` during development.
- Conventional commits, imperative, ≤72 chars.

## Task 1 — Migration 0038 and store registration

Files: `migrations/0038_artifact_commit_intents.sql`,
`crates/voom-store/src/migrator.rs`,
`crates/voom-store/src/schema_test.rs` (count pin),
`crates/voom-store/src/repo/media/mod.rs`.

Table (STRICT, following 0001 house style; the migration also carries a
0037-style preflight temp-table guard that fails apply when any
`artifact_commit_records` row is `pending` or `recovery_required` — those
legacy rows must be resolved under the prior binary):

```sql
CREATE TABLE artifact_commit_intents (
    id                            INTEGER PRIMARY KEY,
    commit_record_id              INTEGER NOT NULL UNIQUE
        REFERENCES artifact_commit_records(id) ON DELETE RESTRICT,
    artifact_handle_id            INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    source_file_version_id        INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    verification_id               INTEGER NOT NULL REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    staging_location_id           INTEGER NOT NULL REFERENCES file_locations(id) ON DELETE RESTRICT,
    staging_location_epoch        INTEGER NOT NULL CHECK (staging_location_epoch >= 0),
    target_storage_root_id        INTEGER NOT NULL REFERENCES library_roots(id) ON DELETE RESTRICT,
    target_root_epoch             INTEGER NOT NULL CHECK (target_root_epoch >= 0),
    target_provider_relative_locator TEXT NOT NULL CHECK (length(target_provider_relative_locator) BETWEEN 1 AND 4096),
    owner_node_id                 INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    expected_facts                TEXT NOT NULL CHECK (json_valid(expected_facts)),
    state                         TEXT NOT NULL CHECK (state IN ('pending','authorized','completed','aborted','recovery_required')),
    intent_epoch                  INTEGER NOT NULL DEFAULT 0 CHECK (intent_epoch >= 0),
    receipt                       TEXT CHECK (receipt IS NULL OR json_valid(receipt)),
    requested_at                  TEXT NOT NULL,
    authorized_at                 TEXT,
    terminal_at                   TEXT,
    CHECK (
           (state = 'pending' AND commit_fence IS NULL AND authorized_at IS NULL AND receipt IS NULL AND terminal_at IS NULL)
        OR (state = 'authorized' AND commit_fence IS NOT NULL AND authorized_at IS NOT NULL AND terminal_at IS NOT NULL = FALSE)
        OR (state IN ('completed','recovery_required') AND commit_fence IS NOT NULL AND authorized_at IS NOT NULL AND terminal_at IS NOT NULL)
        OR (state = 'aborted' AND terminal_at IS NOT NULL)
    )
);
```

(Translate the `= FALSE` pseudo-SQL into `terminal_at IS NULL` in the real
file; keep CHECK coherence exactly: `authorized` ⇒ fence + authorized_at
present, terminal_at absent.) Add index
`artifact_commit_intents_by_state ON artifact_commit_intents (state, id)`.

Migrator: add `const MIGRATION_0038_SQL` include_str! plus a
`Migration::new(3, Cow::Borrowed("artifact_commit_intents"), …)` entry in
`crates/voom-store/src/migrator.rs`; bump the count pin in
`schema_test.rs` (`expected_migrations_matches_embedded_count`).

Verification: `cargo test -p voom-store schema` — all pass including the new
physical version; `just check-paused-time-db` unaffected.

Acceptance: fresh DB migrates through physical version 3; an insert violating
any state-coherence rule fails.

## Task 2 — Store repo `artifact_commit_intents`

Files: `crates/voom-store/src/repo/media/artifact_commit_intents.rs` (+ sibling `_test.rs`),
`crates/voom-store/src/repo/media/mod.rs` (export),
`crates/voom-store/src/repo/media/use_leases.rs` (consultation hook).

Types (all durable JSON shapes `deny_unknown_fields`):

```rust
pub struct CommitExpectedFacts { pub size_bytes: u64, pub content_hash: String }
pub struct CommitObservedFacts { pub size_bytes: u64, pub content_hash: String }

#[derive(Serialize, Deserialize)] #[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitReceipt {
    Applying   { reported_at: String },
    Applied    { observed: CommitObservedFacts, reported_at: String },
    Mismatched { reason: String, observed: Option<CommitObservedFacts>, reported_at: String },
    OutcomeUnknown { reason: String, reported_at: String },
}

pub enum ArtifactCommitIntentState { Pending, Authorized, Completed, Aborted, RecoveryRequired }
// as_str()/parse() mirroring ArtifactAccessPlanStatus

pub struct NewArtifactCommitIntent { /* pinned columns of Task 1 minus state/epoch/fence */ }
pub struct ArtifactCommitIntent { /* all columns; commit_fence: Option<Vec<u8>>; receipt: Option<CommitReceipt> */
    pub id: u64, pub intent_epoch: u64, /* ... */ }
```

Repo trait + impl (struct `SqliteArtifactCommitIntentRepo`, `Repository`):
`create_pending_in_tx`, `require_intent_in_tx` (by id), `get_by_commit_record_in_tx`,
`authorize_in_tx(tx, id, now) -> ArtifactCommitIntent` — CAS
`WHERE id=? AND state='pending' AND intent_epoch=?`, sets
`state='authorized'`, mints nothing (fence passed in by caller? No — minting
belongs here for atomicity: generate 32 bytes via `rand::rngs::OsRng`
(`fill_bytes`), store + return), bumps `intent_epoch`,
sets `authorized_at`;
`record_receipt_in_tx(tx, id, receipt)` — requires `state='authorized'`
(CAS on epoch; applying may overwrite only a NULL receipt; applied/mismatched
may follow applying);
`append_supplemental_receipt_in_tx(tx, id, receipt)` — requires
`state='recovery_required'` (CAS on epoch); writes the typed
supplemental-receipt column so the original receipt survives alongside it;
`mark_completed_in_tx`, `mark_recovery_required_in_tx`,
`mark_aborted_in_tx` — CAS on prior state + epoch;
`list_open_for_roots_in_tx(tx, node_id)` — non-terminal intents
(`pending`/`authorized`/`recovery_required`) whose target root is owned by
`node_id` (join `library_roots` owner + epoch current), each row carrying
its state so the executor branches.
Row mapping with checked conversions; unknown state/receipt vocabulary →
`VoomError::database` (untrusted persisted data rule).

Lease-scope consultation: add `consult_artifact_intent_lock_in_tx(tx,
scope: &LeaseScope) -> Result<Option<String>, VoomError>` returning a
conflict reason when a `pending|authorized|recovery_required` intent pins
the scope (match on source file version / staging location / target root
scopes as encoded by `LeaseScope`), and call it beside
`consult_pending_commit_lock_in_tx` in `use_leases.rs` blocking-lease
acquisition (~line 604). Reuse the join style of
`commit_safety_gate.rs:631`.

Tests (`artifact_commit_intents_test.rs` + additions where lease tests live,
e.g. `use_leases_test.rs`): CAS transition matrix incl. wrong-state rejection;
fence uniqueness/unconsumed semantics via state machine; receipt ordering
(applying→applied ok; applied-before-applying rejected); lease acquisition
refused on pinned scope while pending/authorized, allowed after abort/
completion. Verification: `cargo test -p voom-store`.

## Task 3 — Events payloads

Files: `crates/voom-events/src/payload/artifact.rs` (+ `_test.rs` if present
pattern exists), `crates/voom-events/src/payload/mod.rs`.

New payloads (`deny_unknown_fields`, dotted rename matching
`EventKind::as_str()` — extend EventKind if the repo's kind enum requires):

```rust
ArtifactCommitIntentRecordedPayload { commit_record_id: u64, artifact_handle_id: u64,
    verification_id: u64, owner_node_id: u64, target_root_id: u64,
    target_provider_relative_locator: String, started_at: OffsetDateTime }
ArtifactCommitIntentAuthorizedPayload { commit_record_id: u64, artifact_handle_id: u64,
    owner_node_id: u64, incarnation_id: String, authorized_at: OffsetDateTime } // never the fence
ArtifactCommitReceiptReportedPayload { commit_record_id: u64, artifact_handle_id: u64,
    kind: String, /* applying|applied|mismatched|outcome_unknown */
    reason: Option<String>, observed_size_bytes: Option<u64>, observed_checksum: Option<String>,
    reported_at: OffsetDateTime }
```

Variants `ArtifactCommitIntentRecorded` ("artifact.commit_intent_recorded"),
`ArtifactCommitIntentAuthorized` ("artifact.commit_intent_authorized"),
`ArtifactCommitReceiptReported` ("artifact.commit_receipt_reported") at
`payload/mod.rs` (§138–153 region). Follow the explicit-rename convention —
no rename_all on the enum. Check `EventKind` enum in
`crates/voom-events/src/kind.rs` (or equivalent) and add matching kinds.

Verification: `cargo test -p voom-events`; round-trip serde tests proving
unknown-field rejection and kind/tag agreement.

## Task 4 — Control-plane commit rework

Files under `crates/voom-control-plane/src/artifact/commit/`: `mod.rs`,
`prepare.rs`, `promote.rs` (delete byte ops), `finalize.rs`, `recovery.rs`,
new `intent.rs` (+ siblings `*_test.rs`); `crates/voom-core/src/ids.rs`
(add `ArtifactCommitIntentId` if id-newtype convention applies);
`crates/voom-control-plane/src/lib.rs` exports for api.

4a. **Prepare** (`prepare.rs`): keep existing reads/gate/pending-record
creation; replace local observation (`prepare_commit_paths`) — expected
facts come from the pinned `ArtifactVerification` (size/hash fields it
already carries); create the pending intent row in the same transaction;
emit `ArtifactCommitIntentRecorded`. Remove host staging observation.

4b. **Authorize case** (`intent.rs`):
`ControlPlane::remote_authorize_commit_intent(RemoteCommitAuthorizeInput { intent_id, node_id, token, incarnation_id, idempotency_key, request_hash }) -> AuthorizeCommitOutcome`
following the remote_execution case pattern (`cases/execution/remote_execution/*`):
reserve-or-replay (`route_key = "artifact.commit.authorize"`); tx:
incarnation fence; load intent; require `pending`; requester owns target
root (`library_roots.owner_node_id == node_id`, root state active);
revalidate `staging_location.epoch` and `root_epoch` unchanged; staging
location still live; re-run `evaluate_commit_safety_gate` fail-closed; then
`authorize_in_tx` (mints fence); emit
`ArtifactCommitIntentAuthorized`; store replay outcome (includes hex fence);
commit. Drift → abort intent (`aborted`) + `Conflict`. Replay returns the
stored outcome verbatim (G3).

4c. **Receipt cases**: `remote_report_commit_applying(...)`,
`remote_report_commit_outcome(...)` carrying typed evidence
(applied+mismatched+outcome_unknown unified input with `deny_unknown_fields`
tagged payload). Guards: incarnation fence; intent `authorized`; epochs
still pinned; `applying` accepted before any other receipt. Emit
`ArtifactCommitReceiptReported`. A mismatched/outcome_unknown receipt
transitions intent → `recovery_required` and the commit record →
`recovery_required` (reuse `mark_recovery_required_with_event_in_tx`
vocabulary from `voom-artifact/src/commit_pipeline.rs`) in one tx.

4d. **Complete case**: validate fence hex → bytes equal + unconsumed
(state still `authorized`), receipt `applied` with observed facts equal to
expected facts, epochs pinned; run existing
`finalize_commit_in_tx` (result version/location, retire staging, mark
committed) and `mark_completed_in_tx` in the SAME transaction; emit
`ArtifactCommitCompleted`; consume fence via state transition; replay via
`remote_idempotency_keys` returns original report (G3, G4).

4e. **Driver rework**: `commit_artifact` = prepare (creates pending intent)
then bounded wait polling the record until `committed` (build report from
record; success) or `recovery_required`/`failed` (command error with report)
or deadline elapses (CommitFailure naming the pending intent; record stays
pending; recoverable). Constant `COMMIT_CONVERGENCE_TIMEOUT` in `mod.rs`
with doc comment. Hooks trait keeps `after_prepare`; install/temp hooks are
deleted with the byte ops.

4f. **Recovery** (`recovery.rs`): rewrite `prepare_commit_recovery`:
load record + intent; classify per spec step 7 — receipt-less authorized
(or stale-pending whose node is dead): safe abort via CAS then fresh
successor prepare; `applied` (original or supplemental) w/ matching facts:
finalizes directly (no mutation); `outcome_unknown`/stale `applying`
resolved as not-applied (supplemental receipt in the supplemental column:
target absent, no temp sibling): abort and re-drive fresh generation;
`mismatched`/unresolved `outcome_unknown`/epoch drift: return
operator-required Conflict carrying evidence (record stays). Delete local
path probing (`inspect_recovery_target` filesystem half, `observe_regular_file`
use).

4g. Delete `promote.rs` byte operations (`install_temp_no_replace`,
`fsync_parent_dir`, `copy_regular_file_with_expected` usage) and any now-dead
helpers in `artifact/fs.rs`; remove obsolete imports. Keep helpers still used
elsewhere (grep callers before deleting; e.g. backup/verify paths).

Tests: adapt `mod_test.rs`, `tests/staged_artifact_flow.rs`,
`tests/recover_commit_gate.rs`, `tests/commit_use_lease_gate.rs` to drive the
node half through the case functions (spawned tokio task or direct calls
between prepare and completion). New tests: authorize drift axes (lease live,
root reassigned, location epoch bumped, wrong node, stale incarnation);
receipt ordering; complete fence mismatch; replayed authorize/complete
idempotent; recovery four-state classification; timeout report.
Verification: `cargo test -p voom-control-plane` green.

## Task 5 — API routes

Files: `crates/voom-api/src/commit.rs` (+ `_test.rs`), `crates/voom-api/src/lib.rs` (merge routes).

Routes (POST, execution.rs handler pattern verbatim — HeaderMap credentials,
`stable_request_hash`, envelopes, `voom_route_error_response`):

```
POST /v1/artifact/commit/open                           -> list non-terminal (pending|authorized|recovery_required) intents for caller's owned roots, with state
POST /v1/artifact/commit/{intent_id}/authorize          -> fenced authorization
POST /v1/artifact/commit/{intent_id}/applying           -> journal receipt
POST /v1/artifact/commit/{intent_id}/outcome            -> applied|mismatched|outcome_unknown evidence
POST /v1/artifact/commit/{intent_id}/complete           -> fence + observed facts -> committed report
```

Command constants `artifact.commit.*`. Request/response structs
`deny_unknown_fields`; the fence travels as lowercase hex. Not-configured
404 mirrors `not_configured_response`. Tests mirror `execution_test.rs`:
auth 401, bad body 400, conflict 409 paths, envelope shape, replay headers.
Verification: `cargo test -p voom-api`.

## Task 6 — Node agent executor

Files: `crates/voom-node-agent/src/client.rs` (+ methods),
new `crates/voom-node-agent/src/commit.rs` (+ `_test.rs`),
`crates/voom-node-agent/src/runtime.rs` (wire periodic task),
`crates/voom-node-agent/src/lib.rs` (module export).

Client: `RetryRequest` wrappers for the five routes with typed
`deny_unknown_fields` requests/outcomes mirroring the API shapes (follow
existing endpoint-wrapper pattern client.rs:280–351; add to the
`ControlPlaneApi` test seam trait runtime.rs:1194–1244).

Executor (`commit.rs`): poll loop period = config `poll_interval_ms` with
`centered_jitter` (ADR 0064); per intent from the open listing, branching
on its state: `pending` → authorize → applying → verify staging facts
(observe + compare expected; drift → outcome=mismatched, no mutation) →
copy to unique temp sibling → install no-replace → fsync parent dirs →
observe target → complete. `authorized` → re-request authorize (replay
returns the same fence) → resume at the applying step.
Per `recovery_required` intent for an owned root: re-observe
staging/target read-only and file the supplemental typed receipt through
the outcome route (target absent, no temp sibling → outcome_unknown
resolved as not-applied; matching → applied; drifting → mismatched).
Port add-only install semantics verbatim from the retired host promote
(hard-link/rename no replacement, parent-dir fsync, symlink rejection).
Journal-before-mutate: if the applying report errors terminally, stand
down. Crash between apply and complete is safe: on restart the open
listing rediscovers the authorized intent, authorize replays the same
fence; observing an existing matching target yields applied evidence →
complete converges.

Runtime: spawn the executor task alongside the acquire coordinator sharing
shutdown (`watch::Receiver<ShutdownKind>`); graceful drain per ADR 0060.

Tests (`commit_test.rs`, temp-dir based): happy path; staging drift leaves
bytes untouched; existing matching target converges; existing mismatched
target reports operator-required evidence and mutates nothing.
Verification: `cargo test -p voom-node-agent`.

## Task 7 — Contract registrations and docs

Files: `docs/payload-contract-inventory.md`,
`scripts/payload-contract-scope.txt` (add
`crates/voom-store/src/repo/media/artifact_commit_intents.rs` +
`crates/voom-control-plane/src/artifact/commit/intent.rs` if they define
typed durable-column structs not already listed),
`docs/adr/README.md` row (already landed), AGENTS.md crate-map sentence only
if layering changed (it must not — no new crate edges beyond existing
manifest deps).

Final verification: `just ci` green end-to-end; `git rebase`-free clean
history; push and open PR (handled by `$deliver`).

Rollback: code is a single-feature revert; schema rollback additionally
requires an operator to drop `artifact_commit_intents` and the version-3
migration row — the prior binary refuses a version-3 database
(`SchemaTooNew`), and the 0038 preflight guard fails closed on
non-terminal legacy commit rows.

## Self-review checklist

- Every spec guarantee maps: G1→T2/T4b/T4d epoch+fence checks; G2→T4b gate
  rerun + T2 consultation; G3→replay in T4b/T4d; G4→same-tx finalize+fence
  consumption; G5→T4f classification + receipts; G6→no-replace port T6,
  events T3/T4, gate preserved T4b.
- No cross-task "as in Task N" shortcuts for signatures: interfaces restated
  above.
