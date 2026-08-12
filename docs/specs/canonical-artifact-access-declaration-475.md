# Canonical artifact access on byte-work tickets

Status: draft
Date: 2026-08-12
Issue: #475 (parent #420, epic #413)
ADR: [0068](../adr/0068-byte-work-tickets-declare-canonical-artifact-access.md)
Base: `main`

## Context

ADR 0050 makes the storage-owner node the only actor that touches bytes. ADR 0055 gives
every live file location a stable `(storage_root_id, provider_relative_locator)`
identity. Issue #420 must stop non-owner and mixed-owner workers acquiring byte work, and
its first slice — this one — has to make a ticket state which storage it intends to
touch, because nothing downstream can resolve or gate an intent that was never recorded.

Today a workflow ticket carries `operation: OperationKind`, an untyped
`rendered_payload: Value`, and an untyped `source_file: Option<Value>`. Storage identity
reaches the payload as bare JSON numbers and as host-local path strings. Nothing marks a
ticket as byte-touching and nothing bounds which storage it may name.

Two implementations already normalize a ticket kind into an operation and disagree:
`voom_store::repo::execution::workers::normalized_worker_operation` (`workers.rs:1491`)
accepts any suffix after `synthetic.workflow.operation.`, while
`voom_control_plane::workflow::plan::ticket_payload::ticket_operation`
(`ticket_payload.rs:131`) requires a known `OperationKind`.

## Goal

Byte-touching workflow tickets carry one strict, canonical declaration of the artifacts
and storage references they intend to access, expressed only in stable IDs, validated
identically on write and on read, and normalized consistently with the ticket's operation.

## Non-goals

Each is owned elsewhere and must not appear in this change:

- granting or checking mutation permission;
- resolving owner nodes, root state, epochs, or database ownership (#476);
- scheduler eligibility, candidate scoring, or ordering (#476);
- durable artifact access plans or any schema change (#477);
- lease acquisition, replay, or idempotency behavior (#478, #479);
- physical mount validation, path canonicalization, transfer synthesis (#421);
- worker-protocol or worker-request changes (#423).

## Design

### 1. `voom-core` — the declaration vocabulary

New module `crates/voom-core/src/taxonomy/artifact_access.rs`, re-exported from
`voom_core`, following the `taxonomy/storage.rs` pattern of a validating constructor plus
a `Deserialize` impl that routes through it.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessRight {
    Read,
    Write,
    Delete,
}
```

Declaration order is the canonical order: `read < write < delete`. `as_str` and
`from_wire` mirror the existing taxonomy types.

```rust
#[serde(deny_unknown_fields)] pub struct StorageRootAccess {
    pub storage_root_id: StorageRootId,
}
#[serde(deny_unknown_fields)] pub struct FileLocationAccess {
    pub storage_root_id: StorageRootId,
    pub file_location_id: FileLocationId,
}
#[serde(deny_unknown_fields)] pub struct ExistingArtifactAccess {
    pub artifact_handle_id: ArtifactHandleId,
    pub storage_root_id: StorageRootId,
    pub file_location_id: FileLocationId,
}
#[serde(deny_unknown_fields)] pub struct PlannedArtifactAccess {
    pub artifact_handle_id: ArtifactHandleId,
    pub target_storage_root_id: StorageRootId,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactAccessTarget {
    StorageRoot(StorageRootAccess),
    FileLocation(FileLocationAccess),
    ExistingArtifact(ExistingArtifactAccess),
    PlannedArtifact(PlannedArtifactAccess),
}
```

The internally tagged enum with newtype variants over annotated content structs is the
shape ADR 0013 requires and `scripts/check-payload-deny-unknown.sh` enforces; an inline
tagged struct-variant is a silent no-op and is forbidden. `AcquireOutcome`
(`crates/voom-node-agent/src/client.rs:572`) is the existing example.

Acceptance criterion 2 is satisfied structurally, not by a validation branch: an existing
handle cannot be expressed without both `storage_root_id` and `file_location_id`, and a
planned output handle cannot be expressed without `target_storage_root_id`. A
non-conforming entry has no encoding.

No target carries a path, provider locator, mount name, or host string. This is the
property that stops a declaration being mistaken for proof of locality (#420: "No path
equality or shared-mount naming is used as proof").

```rust
#[serde(deny_unknown_fields)]
pub struct ArtifactAccessEntry {
    pub target: ArtifactAccessTarget,
    pub rights: Vec<ArtifactAccessRight>,
}

#[serde(transparent)]
pub struct ArtifactAccessDeclaration(Vec<ArtifactAccessEntry>);
```

### 2. Canonical form, and the single accepted encoding

`ArtifactAccessDeclaration::new(Vec<ArtifactAccessEntry>) -> Result<Self, VoomError>` is
the only constructor, and the hand-written `Deserialize` impl calls it. Serializing a
declaration and deserializing the result is therefore the identity, and no other byte
sequence deserializes to an equal value.

`new` rejects, each with a distinct message and none naming a locator or path:

| Rule | Rejected because |
|---|---|
| entry list is empty | criterion 1 requires a non-empty declaration |
| an entry's `rights` is empty | an entry that intends nothing is not an intent |
| an entry's `rights` is not strictly ascending | catches duplicate and unordered rights in one rule |
| entries are not strictly ascending by `target` | catches duplicate and unordered entries in one rule |
| a `file_location_id` appears in more than one entry | one location, one intent — two entries for it conflict |
| an `artifact_handle_id` appears in more than one entry | as above, for handles |
| any ID is zero | `voom-core` ID newtypes are unvalidated `u64` wrappers (`ids.rs:1-7`), so a defaulted or truncated field would otherwise read as valid |

A `storage_root_id` may repeat across entries: reading a source location and writing an
output into the same root is ordinary and unambiguous.

Ordering decides whether a persisted payload decodes, so the total order is part of the
wire contract and is fixed deliberately rather than left to whatever a derive happens to
produce: targets order by variant as
`storage_root < file_location < existing_artifact < planned_artifact`, and within a
variant by field in declaration order; rights order as `read < write < delete`. The
derived `Ord` impls must match that statement, and a test asserts the variant ordering
directly so a later reorder of the enum fails a test instead of silently invalidating
every stored declaration.

Errors are `VoomError::Config`, matching `ProviderRelativeLocator::new`. The declaration
is validated identically wherever it enters the process, so a corrupt persisted payload
and a mis-built in-memory one are rejected by the same code.

`entries()`, `file_location_ids()`, and `storage_root_ids()` are the read accessors. There
is no method that resolves, authorizes, mutates, or grants anything, which is how
criterion 5 is met: rights are data, and the only code that can act on them is the code a
later slice writes.

### 3. `OperationKind::is_byte_touching`

```rust
pub const fn is_byte_touching(self) -> bool
```

An exhaustive match with no wildcard arm, so adding an `OperationKind` variant without
classifying it is a compile error.

| Byte-touching | Not byte-touching |
|---|---|
| `scan_library`, `probe_file`, `hash_file`, `back_up_file`, `remux`, `transcode_video`, `transcode_audio`, `edit_tracks`, `extract_audio`, `verify_artifact`, `commit_artifact`, `delete_artifact` | `identify_media`, `score_quality`, `sync_external_system` |

The three excluded operations derive from facts an earlier operation already recorded, or
talk to an external system; none opens an artifact. `scan_library` is included because it
enumerates a root's contents, which #421 moves to the owner node.

### 4. One ticket-kind normalization

```rust
pub const WORKFLOW_OPERATION_NAMESPACE: &str = "synthetic.workflow.operation.";

pub enum NormalizedTicketOperation {
    Known(OperationKind),
    CustomLocal(TicketOperation),
}

impl TicketOperation {
    pub fn normalize_stored(&self, field: &str)
        -> Result<NormalizedTicketOperation, VoomError>;
}
impl NormalizedTicketOperation {
    pub fn operation_kind(&self) -> Option<OperationKind>;
    pub fn into_ticket_operation(self) -> TicketOperation;
}
```

Rules, in order:

1. token inside `WORKFLOW_OPERATION_NAMESPACE` with a suffix `OperationKind::from_wire`
   recognizes → `Known(kind)`;
2. token inside that namespace with any other suffix, including empty → `VoomError::Database`
   naming `field`, the namespace, and the rejected suffix. **This is the behavior change**:
   `normalized_worker_operation` currently returns the bare suffix as an operation and lets
   it match worker capabilities;
3. token equal to an `OperationKind` wire token → `Known(kind)`;
4. any other token → `CustomLocal(self.clone())`, which preserves today's handling of
   exact custom local kinds such as `disk.test` and `noop`.

`normalize_stored` takes `field` and reports `VoomError::Database` because both call sites
receive a `TicketOperation` read from SQLite, and AGENTS.md requires persisted values to be
treated as untrusted with corruption reported as a database error. `normalized_worker_operation`
and `ticket_payload::ticket_operation` are deleted; no third normalization is added.

**Where the failure lands.** `remote_acquire_candidates_in_tx`
(`cases/execution/remote_execution/acquire.rs:295-352`) loops over candidate tickets and
`?`-propagates `operation_eligibility_in_tx` and `operation_capacity_in_tx`, so an error
raised inside those calls ends evaluation for the entire candidate set. Today an
unrecognized kind merely matches no capability row and that one ticket scores ineligible.
To avoid making one corrupt `tickets.kind` value stall acquisition for every well-formed
ticket, `operation_eligibility_in_tx` and `operation_capacity_in_tx` catch a normalization
failure and report the operation as ineligible with a reason, rather than propagating it.
Fail-closed here means denied, never leased — not aborted. `parse_ticket` keeps the hard
error, because it already runs per ticket and cannot spread. `acquire.rs` itself is not
modified; the containment lives in `workers.rs`, inside the frozen surface.

### 5. The ticket payload gate

`WorkflowTicketPayload` (`crates/voom-control-plane/src/workflow/plan/ticket_payload.rs`)
gains one field:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub artifact_access: Option<ArtifactAccessDeclaration>,
```

`to_ticket_payload` (encode) and `parse_ticket` (decode) both call one shared
`validate_artifact_access`, so the same contract binds writers and readers:

1. `operation.is_byte_touching()` and `artifact_access` is `None` → reject. Non-emptiness
   needs no separate check; the type cannot hold an empty declaration.
2. `!operation.is_byte_touching()` and `artifact_access` is `Some` → reject. A ticket that
   touches no bytes has no access to declare, and a stray declaration would be evidence
   #476 must not act on.
3. `operation.is_byte_touching()` and `operation != ScanLibrary` →
   `rendered_payload.source_location_id` must be present and a non-zero `u64`, and the
   declaration must contain exactly one entry naming that `FileLocationId` (as
   `file_location` or `existing_artifact`) with `read` among its rights. This is the
   anti-drift check: the typed declaration and the untyped rendered payload cannot
   disagree about the source.
4. `operation == ScanLibrary` → `rendered_payload.source_location_id` must be absent, and
   the declaration must contain exactly one `storage_root` entry and nothing else. A scan
   addresses a root, not a file, so a source location on it would be evidence of a
   mis-rendered ticket.

Rules 3 and 4 partition the byte-touching operations, so the check binds for every one of
them rather than only when the field happens to be present. That matters: a check
conditional on presence would be absent for exactly the `TargetRef::FileVersion` nodes
whose location is ambiguous — the case operator decision D2 exists to resolve — and a
mis-resolved location would then be durable and unnoticed. §6 therefore makes every
non-scan byte-touching renderer emit `source_location_id`.

Rule 3 is one-way in the other direction: the declaration may name references the rendered
payload does not, because a write intent names a target root that the worker-facing
payload has no field for.

`parse_ticket` derives the expected operation through `TicketOperation::normalize_stored`
instead of the deleted local helper; a `CustomLocal` kind reaching
`WorkflowTicketPayload::parse_ticket` is rejected, as it is today.

### 6. Producing the declaration

Two renderers create workflow tickets, and both already have — or can cheaply obtain —
the identity a declaration needs.

`crates/voom-control-plane/src/operation_source.rs` already resolves a file version to
exactly one live rooted location and fails closed on zero or several (`select_location`,
lines 91-121). That private helper is widened to `pub(crate)`; nothing about its logic
changes. It is reused rather than duplicated, and it is the byte-free half of
`select_local_source` — the declaration path must not canonicalize or stat anything.

A new `crates/voom-control-plane/src/workflow/plan/artifact_access.rs` holds:

```rust
pub(crate) struct TicketStorageSource {
    pub(crate) storage_root_id: StorageRootId,
    pub(crate) file_location_id: FileLocationId,
}

pub(crate) fn declaration_for(
    operation: OperationKind,
    source: Option<&TicketStorageSource>,
) -> Result<Option<ArtifactAccessDeclaration>, VoomError>;
```

The mapping is total over `OperationKind` and each operation appears in exactly one row:

| Operation | Entries |
|---|---|
| `identify_media`, `score_quality`, `sync_external_system` | `Ok(None)` — not byte-touching |
| `scan_library` | `storage_root(root)` with `read` |
| `probe_file`, `hash_file`, `verify_artifact` | `file_location(root, loc)` with `read` |
| `remux`, `transcode_video`, `transcode_audio`, `extract_audio`, `edit_tracks`, `back_up_file`, `commit_artifact` | `file_location(root, loc)` with `read`, and `storage_root(root)` with `write` |
| `delete_artifact` | `file_location(root, loc)` with `read, delete` |

A byte-touching operation reached with `source: None` is a `VoomError::Config` naming the
operation. That is the rule for the `render_default_payload` fallback arms
(`executor/tickets.rs:176,188,199,207` and the `_ =>` catch-all at 209), which emit paths
and codecs but no IDs: a byte-touching node reaching them without a threaded or resolved
source fails at render instead of producing an undeclared ticket. Those arms are reachable
only from the `#[cfg(test)]` demo plan, whose fixtures supply a synthetic source.

The write intent is a `storage_root` entry rather than a `planned_artifact` entry because
no artifact handle exists at render time; the target root is what is knowable, and it is
what #476 needs to check ownership against. Selecting a distinct output root from the
source root's `default_output_root_id` is `artifact_target_root`'s job at commit time and
is out of scope here (#477 owns durable plans).

`insert_policy_file_source` (`binding.rs:323-331`) currently writes `source_location_id`
only when `PolicyFileSource.location_id` is `Some`, which `resolve_policy_file_source`
sets only for a `TargetRef::FileLocation` node. `PolicyFileSource.location_id` becomes a
plain `FileLocationId`: `resolve_policy_file_source` resolves a `TargetRef::FileVersion`
through `select_location` (decision D2) instead of leaving the field empty, and
`insert_policy_file_source` always writes it. Two things follow. Rule 3 above binds for
every non-scan byte-touching ticket rather than for half of them. And the dispatch path —
`operation_adapters::source_location_id` feeding `select_local_source` — now receives the
render-time choice instead of independently re-resolving "the single live rooted location"
against a table that may have changed since, so the declaration and the bytes actually
opened cannot name different locations.

`BranchContext` (`binding.rs:17`) gains `storage_source: Option<TicketStorageSource>`, so
`expansion.rs` threads a parent ticket's resolved source into its children exactly as it
already threads `source_file`. `executor/tickets.rs::render_node_ticket` resolves
`node.policy_target()` once through `select_location` and passes the result to both the
payload and `declaration_for`.

`WorkflowPlan::default_ci` is `#[cfg(test)]` (`model.rs:59`); every production plan node
carries `policy_target: Some(..)` (`policy_bridge.rs:97`). Test plans supply a
deterministic synthetic `TicketStorageSource`. This slice resolves nothing against the
database beyond the existing `select_location` read, so synthetic non-zero IDs are valid
inputs — which is precisely what "routing evidence only" means.

### 7. Guardrail manifests

`ticket_payload.rs` is already in `scripts/payload-contract-scope.txt`. The new core
module joins it, and `docs/payload-contract-inventory.md` gains the declaration structs,
because they are typed content of the durable `tickets.payload` column.
`docs/adr/README.md` gains the ADR 0068 row: `check-adr-index` is part of `just ci`, and
CI runs `just ci`, so the row is a merge precondition. `docs/release-process.md` gains the
breaking-payload upgrade step, because ADR 0013 requires the binary-before-DB ordering to
live there rather than only in a decision record.

## Threat model

**Boundaries added.** One: a new typed region inside `tickets.payload`, deserialized from
SQLite on every ticket read. No new entry point, route, CLI argument, env var, or config
key. No boundary is widened.

**Actors.** The writer is this control-plane process. The reader is the same process
reading a row that a *different, possibly older or corrupted* writer produced. SQLite
content is untrusted per AGENTS.md, so the persisted declaration is attacker-controlled
for the purposes of this design even though no remote actor writes it directly.

**Control per boundary.** Deserialization routes through `ArtifactAccessDeclaration::new`,
so every rule in §2 applies to persisted input, not only to freshly built values.
`deny_unknown_fields` on each content struct and on `ArtifactAccessEntry` rejects
unrecognized fields; the internally tagged enum rejects unrecognized `kind` values. Entry
count is bounded in practice by the producer (at most two entries), and no rule is
quadratic in a way an adversarial payload could exploit: duplicate detection uses sorted
comparison and hash sets. Failure messages name the rule, the field, and the rejected ID,
and never a locator, path, hostname, or provider string.

**Explicitly out of scope.** Whether the referenced root, location, or handle exists, is
live, is owned by the acquiring node, or is at the expected epoch — all of that is #476,
and a declaration that passes validation here proves only that it is well-formed. Rights
authorize nothing: this change adds no code path that reads a right and performs an
action, and #478 still owns the atomic capability, grant, and concurrency checks at
acquisition. Denial of service through very large ticket payloads is bounded by the
existing `tickets.payload` write path and is unchanged here.

## Failure behavior

- Malformed, duplicated, conflicting, zero-ID, or non-canonical declaration →
  `VoomError::Config` from `ArtifactAccessDeclaration::new`, surfaced through
  `WorkflowTicketPayloadError` at encode and decode. Ticket creation fails before the row
  is written; ticket read fails before the ticket can be scheduled or leased.
- Missing declaration on a byte-touching ticket, or present on a non-byte-touching one →
  same path, distinct message.
- Unknown token inside the reserved namespace → `VoomError::Database` from
  `normalize_stored`. At `operation_capacity_in_tx` this now rejects the acquisition
  instead of matching a fabricated capability.
- Policy target that resolves to zero or several live rooted locations → the existing
  `VoomError::Config` messages from `select_location`, raised at render time.

## Compatibility

Pre-release, one-way. A byte-touching ticket row written by an earlier binary has no
`artifact_access` field and no longer decodes; no backfill can invent a root or location
that was never recorded, so none is attempted.

Such a ticket cannot be drained by completing it: migration 0034 already quarantined every
pre-existing file location as unassigned legacy, so it was already undispatchable. The
available action before upgrading is to identify and fail or delete pre-upgrade
byte-touching workflow tickets. Each one that instead reaches a terminal transition opens a
`terminal_failure` issue (ADR 0018, deduped per ticket), so upgrading over a non-empty
queue converts silently stuck tickets into a burst of issues. This adds no new operator
procedure: ADR 0055's flag-day migration already requires deliberate root assignment and a
rescan before byte work can resume, and this precondition rides with it.

This is a breaking `tickets.payload` change under ADR 0013, which requires the
binary-before-DB ordering to be recorded in `docs/release-process.md`. That file gains the
precondition in this change; the surface list grows by that one file for that reason.

No schema change, no migration, and no new dependency.

## Test strategy and acceptance criteria

Every criterion below maps to at least one test that fails before the change.

**Criterion 1 — every byte-touching ticket carries a non-empty canonical declaration
whose stable references match the ticket's typed identity.**
- `ticket_payload_test.rs`: encoding a byte-touching payload with `artifact_access: None`
  is rejected; encoding a non-byte-touching payload with `Some(..)` is rejected; both
  round-trip successfully in the correct configuration.
- `ticket_payload_test.rs`: table-driven over all fifteen `OperationKind` values, so the
  requirement cannot be satisfied for one operation and missed for another.
- `ticket_payload_test.rs`: a declaration naming a different `FileLocationId` than
  `rendered_payload.source_location_id` is rejected; naming it without `read` is rejected;
  naming it with `read` is accepted; a non-scan byte-touching payload with no
  `source_location_id` at all is rejected; a `scan_library` payload carrying one is
  rejected, and one declaring anything other than a single `storage_root` entry is
  rejected.
- `binding_test.rs` / `tickets_test.rs`: a `TargetRef::FileVersion` node renders a
  `source_location_id`, so the field is no longer conditional on the target shape and the
  dispatch path consumes the render-time choice rather than re-resolving.
- `artifact_access_test.rs`: a **frozen canonical-encoding fixture** — the byte-exact JSON
  of a declaration carrying one entry of each of the four target variants, with multi-right
  entries — is asserted in both directions. Reordering variants or fields turns it red.
  Prose alone would be silently ignorable, which is the failure class ADR 0013 exists to
  stop, and `check-payload-deny-unknown.sh` cannot see an ordering change.
- `workers_test.rs`: a candidate set containing one ticket with an unnormalizable kind and
  one well-formed ticket scores only the first ineligible; the second remains eligible.
  This is the containment that keeps a corrupt row from stalling acquisition, and it fails
  if the normalization error is propagated instead of caught.

**Criterion 2 — existing handles require a location/root reference; unmaterialized output
handles require a matching target-root reference.**
- `artifact_access_test.rs`: JSON for `existing_artifact` missing `file_location_id` or
  `storage_root_id` fails to deserialize; `planned_artifact` missing
  `target_storage_root_id` fails to deserialize; each complete form round-trips.
- `artifact_access_test.rs`: `planned_artifact` carrying `file_location_id` is rejected as
  an unknown field, proving the shapes are not interchangeable.

**Criterion 3 — duplicate, conflicting, zero-ID, malformed, and non-canonical
declarations fail before scheduling.**
- `artifact_access_test.rs`: one case per rule in §2, asserting the specific message.
- `artifact_access_test.rs`: a deterministic exhaustive test builds a fixed set of four
  distinct entries, enumerates all 24 orderings in code, and proves exactly the ascending
  ordering is accepted and `serialize → deserialize` is the identity on it. No new
  dependency: the permutations are generated by an in-test loop, since the workspace
  carries no property-testing crate and this change adds none.
- `expansion_test.rs`: a ticket whose payload carries a non-canonical declaration is
  rejected by `parse_ticket` before `mark_ready_if_unblocked` can promote it.

**Criterion 4 — exact custom local operations remain supported; known exact and
namespaced operations normalize deterministically; unknown namespaced operations fail
closed.**
- `ticket_operation_test.rs`: `probe_file` → `Known(ProbeFile)`;
  `synthetic.workflow.operation.probe_file` → `Known(ProbeFile)`; `disk.test` and `noop` →
  `CustomLocal`; `synthetic.workflow.operation.bogus` and
  `synthetic.workflow.operation.` → `VoomError::Database` naming the field.
- `ticket_operation_test.rs`: every `OperationKind::ALL` value normalizes identically from
  its exact and its namespaced token.
- `workers_test.rs` / `leases_test.rs`: acquiring a ticket whose kind is
  `synthetic.workflow.operation.bogus` fails closed. Existing tests that use
  `synthetic.workflow.operation.test` and `.extract` are updated to real operations, since
  those kinds are exactly what this criterion makes invalid.

**Criterion 5 — rights describe intended access only and cannot independently authorize
mutation.**
This criterion is negative — it asserts an absence — so it is proved behaviorally rather
than by a test that inspects the type.
- `expansion_test.rs`: two byte-touching tickets identical except that one declares
  `read` and the other `read, write, delete` on the same target reach the same ticket
  state, the same readiness, and the same lease eligibility. Rights therefore change no
  outcome in this slice, which is what "cannot independently authorize mutation" means
  while #476 through #479 are unbuilt.
- The existing commit-authorization and use-lease gate tests in `voom-control-plane` pass
  unchanged, proving no authorization path gained the declaration as an input.
- `artifact_access.rs` exposes only `new`, the accessors named in §2, and its serde impls.
  Nothing in the module reads a `ArtifactAccessRight` to decide anything, and no caller
  outside it may, which the ADR records as the governing constraint for later slices.

**Criterion 6 — strict payload tests cover all direct serializers and consumers without a
second accepted wire format.**
- `just check-payload-deny-unknown` covers the new core module after it is added to
  `scripts/payload-contract-scope.txt`; its `-selftest` sibling proves the guard still bites.
- `artifact_access_test.rs`: an unknown field at every level — entry, each content struct,
  and an unknown `kind` — is rejected.
- `artifact_access_test.rs`: reordered entries, reordered rights, and duplicated rights all
  fail to deserialize, which is what "no second accepted wire format" means operationally.
- `crates/voom-control-plane/tests/`: the existing `remux_flow`, `video_transcode_flow`,
  `audio_extract_flow`, `staged_artifact_flow`, and `phase_barrier_flow` integration suites
  pass with declarations flowing end to end.

**Guardrails.** `just ci` passes with zero failures and zero warnings. No test is added
ignored, and no existing test is deleted except the two obsolete
unknown-namespaced-operation cases named above, whose behavior is replaced by the
fail-closed tests in criterion 4.
