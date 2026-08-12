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
declaration and deserializing the result is therefore the identity, and no other *entry
ordering, rights ordering, or entry multiplicity* deserializes to an equal value. JSON
object-member order and whitespace remain serde's to accept, as they are everywhere else in
the payload contract; the canonical-form rules constrain list order, not object-member
order.

`new` rejects with these exact messages, so the criterion-3 tests assert a contract the spec
states rather than one the first implementation happens to produce. Every message is
`VoomError::Config`, is distinct, and names no locator, path, or host:

| Rule | Message | Rejected because |
|---|---|---|
| entry list is empty | `artifact access declaration must not be empty` | criterion 1 requires a non-empty declaration |
| entry list longer than `MAX_ENTRIES` (8) | `artifact access declaration has {n} entries, at most 8 are allowed` | a canonicalization sanity bound; see the threat model for what it is *not* |
| an entry's `rights` is empty | `artifact access entry {i} must declare at least one right` | an entry that intends nothing is not an intent |
| an entry's `rights` is not strictly ascending | `artifact access entry {i} rights must be strictly ascending read < write < delete` | catches duplicate and unordered rights in one rule |
| entries are not strictly ascending by `target` | `artifact access entries must be strictly ascending by target; entry {i} does not follow entry {i-1}` | catches duplicate and unordered entries in one rule |
| a `file_location_id` appears in more than one entry | `artifact access declares file location {id} in more than one entry` | one location, one intent — two entries for it conflict |
| an `artifact_handle_id` appears in more than one entry | `artifact access declares artifact handle {id} in more than one entry` | as above, for handles |
| any ID is zero | `artifact access entry {i} has a zero {field}` | `voom-core` ID newtypes are unvalidated `u64` wrappers (`ids.rs:1-7`), so a defaulted or truncated field would otherwise read as valid |

`{field}` is the struct field name (`storage_root_id`, `file_location_id`,
`target_storage_root_id`, `artifact_handle_id`), and `{i}` is the zero-based entry index.

A `storage_root_id` may repeat across entries: reading a source location and writing an
output into the same root is ordinary and unambiguous.

Ordering decides whether a persisted payload decodes, so the total order is part of the
wire contract and is fixed deliberately rather than left to whatever a derive happens to
produce: targets order by variant as
`storage_root < file_location < existing_artifact < planned_artifact`, and within a
variant by field in declaration order; rights order as `read < write < delete`. The
derived `Ord` impls must match that statement, and a frozen encoding fixture asserts it so
a later reorder of the enum fails a test instead of silently invalidating every stored
declaration.

Reading criterion 3's "non-canonical" to include entry order is a decision, recorded as D4
on issue #475 alongside D1-D3. It is cheap to reverse: a write-canonicalising producer
emits byte-identical output whether or not the reader insists on order, so relaxing the
reader later invalidates nothing already written.

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
    /// A known operation. `namespaced` records whether the token arrived inside
    /// `WORKFLOW_OPERATION_NAMESPACE` or as a bare wire token.
    Known { kind: OperationKind, namespaced: bool },
    /// An exact token outside every reserved namespace.
    CustomLocal(TicketOperation),
    /// Inside a reserved namespace, with a suffix no `OperationKind` claims.
    UnknownNamespaced(TicketOperation),
}

impl TicketOperation {
    /// Total and infallible. Classification only; rejection is the caller's.
    pub fn normalize(&self) -> NormalizedTicketOperation;
}
impl NormalizedTicketOperation {
    pub fn operation_kind(&self) -> Option<OperationKind>;
    /// The token to match `worker_capabilities.operation` and grant rows against.
    pub fn matching_token(&self) -> TicketOperation;
}
```

Classification rules, in order:

1. token inside `WORKFLOW_OPERATION_NAMESPACE` whose suffix `OperationKind::from_wire`
   recognizes → `Known { kind, namespaced: true }`;
2. token inside that namespace with any other suffix, including empty →
   `UnknownNamespaced`;
3. token equal to an `OperationKind` wire token → `Known { kind, namespaced: false }`;
4. any other token → `CustomLocal`, preserving today's handling of exact custom local kinds
   such as `disk.test` and `noop`.

`matching_token()` returns `kind.as_str()` for `Known` and the **original, unmodified**
token for the other two. That last point is the one behavior change at the capability seam:
`normalized_worker_operation` today strips the namespace off an unknown token and matches
capability rows against the bare suffix, so `synthetic.workflow.operation.bogus` is looked
up as `bogus`. Neither form matches any real row, so no capability, grant, or limit result
changes in practice — but the fabricated bare token stops being manufactured.

**Normalization is classification; rejection belongs to the caller.** This is why
`normalize` is infallible. The alternative — a fallible `normalize_stored` — puts the error
inside whichever function calls it, and the four current call sites do not all tolerate one:

| Call site | Disposition |
|---|---|
| `workers.rs:1152` `operation_capacity_in_tx` | `matching_token()`; never raises |
| `workers.rs:1218` `operation_capability_history` | `matching_token()`; never raises. Its caller (`spawn.rs:463`) passes an in-process token, so a `VoomError::Database` here would misclassify a caller bug as storage corruption |
| `workers.rs:1415` `operation_capability_details_in_tx` | `matching_token()`; never raises |
| `leases.rs:319` `acquire_guarded` | rejects `UnknownNamespaced` with `VoomError::Database`. This is the fail-closed point for criterion 4 on the lease path, and it is safe to raise here because `acquire_guarded` handles exactly one ticket (`input.ticket_id`) |

`remote_acquire_candidates_in_tx` (`acquire.rs:295-352`) loops over candidates and
`?`-propagates `operation_eligibility_in_tx` and `operation_capacity_in_tx`, so a raise in
either would abort evaluation of the whole set. Neither raises under this design.
Note that `operation_eligibility_in_tx` (`workers.rs:975-1037`) does **not** normalize today
and does not gain normalization here: it binds the raw token into the capability query and
grant comparison, so an `UnknownNamespaced` kind already yields `has_capability: false` and
is denied. Adding normalization there would flip `has_capability` and `has_grant` for every
`synthetic.workflow.operation.*` ticket and change scheduler candidate scoring — a
scheduling change #476 owns, not this slice.

`parse_ticket` is the other rejection point: it accepts only
`Known { namespaced: true }`. `CustomLocal`, `UnknownNamespaced`, and a **bare** known token
are all rejected, which keeps the accepted ticket-kind encoding exactly what
`ticket_payload::ticket_operation` accepts today. Without the `namespaced` flag, rule 3
would newly admit `probe_file` as a workflow ticket kind — a second accepted encoding for
the kind field, which is the thing criterion 6 forbids for the declaration body.

`normalized_worker_operation` and `ticket_payload::ticket_operation` are deleted; no third
normalization is added. Deleting the former edits `crates/voom-store/src/repo/execution/leases.rs`,
which is why that file is on the surface list — the edit there is a call-site swap plus the
`UnknownNamespaced` rejection, and it changes no lease, replay, or idempotency behavior.

The residual, stated rather than fixed: a ticket denied this way never leases, so it never
attempts, never terminates, and never opens an ADR 0018 `terminal_failure` issue, and
nothing durable records why. That is today's behavior for an unrecognized kind carried
forward — making it observable belongs to whoever owns scheduler decision persistence
(#477). No public store type gains a field: `WorkerOperationEligibility` and
`WorkerOperationCapacity` are unchanged, which matters because `max_parallel: 0` would trip
`candidate_from_ticket` (`acquire.rs:548-552`) and re-create the loop abort this design
avoids.

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
3. `operation == ScanLibrary` → `rendered_payload.source_location_id` must be absent, and
   the declaration must be exactly `declaration_for(ScanLibrary, Root { r })` where `r` is
   the root named by the declaration's single `storage_root` entry. A scan addresses a
   root, not a file, so a source location on it is evidence of a mis-rendered ticket.
4. every other byte-touching operation → `rendered_payload.source_location_id` must be
   present and a non-zero `u64`; call it `l`. The declaration must contain exactly one
   entry naming `l`, and that entry must name a root `r`. The declaration must then equal
   `declaration_for(operation, Location { r, l })` **entry for entry and right for right**.

Rule 4 is a total equality check, not a shape check, and that is deliberate. §6 fixes the
rights each operation declares, but a shape check would only bind *targets* — so a
hand-edited or corrupted row could give a `probe_file` ticket `read, write, delete` on its
source, or bolt on a `storage_root` write entry, and pass every canonical-form rule. The
threat model says the reader must treat the persisted writer as untrusted; equality is what
extends that stance to rights. It also subsumes the old shape rules, so there is one rule
where there were two, and it makes the operation-to-rights mapping in §6 the single
definition of a valid declaration on both the write and the read side.

Rules 3 and 4 partition the byte-touching operations, so the check binds for every one of
them rather than only when the field happens to be present. That matters: a check
conditional on presence would be absent for exactly the `TargetRef::FileVersion` nodes
whose location is ambiguous — the case operator decision D2 exists to resolve — and a
mis-resolved location would then be durable and unnoticed. §6 therefore makes every
non-scan byte-touching renderer emit `source_location_id`.

Equality is evaluated against `declaration_for`, which is pure and takes no database
handle, so `parse_ticket` stays synchronous and read-only. It proves the declaration is
*well-formed for its operation* — never that the referenced storage exists, is live, or is
owned by anyone, which remains #476's.

`parse_ticket` derives the expected operation through `TicketOperation::normalize` instead
of the deleted local helper, and accepts only `Known { namespaced: true }`.

### 6. Producing the declaration

Two renderers create workflow tickets, and both already have — or can cheaply obtain —
the identity a declaration needs.

`crates/voom-control-plane/src/operation_source.rs` already resolves a file version to
exactly one live rooted location and fails closed on zero or several (`select_location`,
lines 91-121). That private helper is widened to `pub(crate)`; nothing about its logic
changes. It is reused rather than duplicated, and it is the byte-free half of
`select_local_source` — the declaration path must not canonicalize or stat anything.

`select_location` returns a `FileLocation`, not a root. `storage_root_id` comes from
`FileLocation::rooted_address()` (`operation_source.rs:148`); its
`ProviderRelativeLocator` half is discarded, which is what keeps the declaration
locator-free. Its error arm is unreachable here because `require_live_rooted_location` has
already rejected every non-`Rooted` address, and the implementation propagates rather than
swallowing it. A byte-touching node whose `policy_target` is neither `FileVersion` nor
`FileLocation` keeps the existing `resolve_policy_file_source` error
(`executor/tickets.rs:268-270`) — this slice does not widen the accepted target shapes.

A new `crates/voom-control-plane/src/workflow/plan/artifact_access.rs` holds:

```rust
pub(crate) enum TicketStorageSource {
    /// A whole root. The only shape `scan_library` can carry.
    Root { storage_root_id: StorageRootId },
    /// A live location inside a root. Every other byte-touching operation.
    Location {
        storage_root_id: StorageRootId,
        file_location_id: FileLocationId,
    },
}

pub(crate) fn declaration_for(
    operation: OperationKind,
    source: Option<&TicketStorageSource>,
) -> Result<Option<ArtifactAccessDeclaration>, VoomError>;
```

The two variants exist because a single struct with both fields required cannot express a
scan: it would force every scan branch to carry a fabricated non-zero `file_location_id`
that `declaration_for` discards and the renderer is forbidden to emit — and under D1's
declaration-is-the-identity reading a fabricated ID is a live reference to somebody else's
location. A `scan_library` node given `Location`, or any other byte-touching operation given
`Root`, is a `VoomError::Config` naming the operation and the variant it received.

The mapping is total over `OperationKind` and each operation appears in exactly one row:

| Operation | Entries |
|---|---|
| `identify_media`, `score_quality`, `sync_external_system` | `Ok(None)` — not byte-touching |
| `scan_library` | `storage_root(root)` with `read` |
| `probe_file`, `hash_file`, `verify_artifact` | `file_location(root, loc)` with `read` |
| `remux`, `transcode_video`, `transcode_audio`, `extract_audio`, `edit_tracks`, `back_up_file`, `commit_artifact` | `file_location(root, loc)` with `read`, and `storage_root(root)` with `write` |
| `delete_artifact` | `file_location(root, loc)` with `read, delete` |

The rights column is part of the mapping, not a renderer choice. A renderer free to pick a
different rights set for the same operation would make the evidence #476 resolves
non-deterministic, and the canonical-form rules would not catch it, so `declaration_for` is
the single place rights are decided. `delete_artifact` is the only producer of the `delete`
right; like seven of the twelve byte-touching operations it has no production ticket
producer yet, so that right is exercised only by tests.

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
`insert_policy_file_source` always writes it.

That helper covers only the four policy renderers (`binding.rs:208, 233, 285, 319`). The
other renderer — `render_default_payload_with_fan_out` (`binding.rs:23-99`) — emits
`path`, `operation`, `branch_id`, `duration_ms`, and `progress_interval_ms` and nothing
else, and `executor/tickets.rs:209`'s `_ =>` arm plus every `expansion.rs` branch ticket
route through it. It therefore gains the same field: after the per-operation match, when
`operation.is_byte_touching() && operation != ScanLibrary`, insert `source_location_id`
from `branch.storage_source` alongside the existing `operation` / `branch_id` insertions,
and return a `BindingError` naming the operation when the branch carries no source. Without
this, rule 3 rejects at encode every ticket that renderer produces — including the nine
byte-touching nodes of the demo plan — and the only repairs available would be the two the
spec forbids.

The insertion is at the JSON-object level, alongside the loose keys the function already
adds. `TranscodeVideoRequest` (`binding.rs:105-145`) is serialized before that point and is
**not** modified: the worker-request shape belongs to #423.

Two things then follow. Rule 3 above binds for
every non-scan byte-touching ticket rather than for half of them. And the dispatch path —
`operation_adapters::source_location_id` feeding `select_local_source` — now receives the
render-time choice instead of independently re-resolving "the single live rooted location"
against a table that may have changed since, so the declaration and the bytes actually
opened cannot name different locations.

**The cost of freezing, taken deliberately (decision D5).** Today a `TargetRef::FileVersion`
ticket re-resolves at dispatch, so one whose location was retired and recreated by an ADR
0055 rescan still runs. With the location frozen into the payload,
`require_live_rooted_location` rejects it and the ticket fails terminally, opening an ADR
0018 issue. Treating the recorded ID as a hint and re-resolving on retirement would restore
the self-healing but let the declaration name one location while the process opens another
— the divergence ADR 0050 exists to remove. #479 owns making that replay-safe; this slice
adds no re-render path. A test covers it: a ticket whose recorded location is retired
before dispatch fails terminally rather than silently retargeting.

**Fan-out children get their location from the scanner result.** `expand_scanner_completion`
(`expansion.rs:52-83`) builds the probe / hash / identity children from `scanner_files`,
whose `ScannerFile { path, source_file }` comes from the scan ticket's *result* — path
strings with no IDs (`expansion.rs:365-402`). There is nothing to thread down: the parent is
`scan_library`, which by design holds a root and no location. So the scanner result carries
the missing half.

`ScannerFile` gains `file_location_id: FileLocationId`, parsed from a required, non-zero
`file_location_id` on each `result.files[]` object. The string form of a file entry
(`expansion.rs:381-385`) no longer satisfies a byte-touching expansion and is rejected with
`scanner result file entry requires file_location_id`. The root comes from the parent scan
ticket's own declaration — its single `storage_root` entry — so no new lookup is added and
the child's source is `Location { storage_root_id: parent_root, file_location_id }`. Reading
the parent's declaration for this is not an inversion: the declaration is the parent
ticket's typed storage identity under D1, which is exactly what a child needs to inherit.

This is the fixture migration the operator authorized. It touches `ScannerFile`,
`scanner_files`, `expand_scanner_completion`, and every test that seeds a scanner result;
none of it is reachable in production, where `policy_bridge::execution_operation` admits no
`scan_library` node at all.

`BranchContext` (`binding.rs:17`) gains `storage_source: Option<TicketStorageSource>`, and
it — not `node.policy_target()` — is how the source reaches every renderer.
`render_root_payload`'s `ScanLibrary` arm (`executor/tickets.rs:161`) calls
`render_default_payload_with_fan_out(operation, branch, …)` and never reads the policy
target, so a per-arm target lookup would not reach it.
`executor/tickets.rs::render_node_ticket` resolves `node.policy_target()` once through
`select_location`, populates `BranchContext`, and **passes the resolved `PolicyFileSource`
into `render_root_payload`** so the policy arms consume it instead of each calling
`resolve_policy_file_source` themselves (`tickets.rs:240-270`). Without that hand-off the
render performs two independent async reads of the same target, so "one resolution point"
would be false *within a single render*, and an ADR 0055 rescan landing between them would
make the declaration and `source_location_id` disagree — turning a renderable ticket into a
hard `VoomError::Config` at encode via rule 4. `resolve_policy_file_source` becomes a
private helper of `render_node_ticket` with a single call site;
`expansion.rs` threads a parent ticket's already-resolved source into the same field,
exactly as it already threads `source_file`, keeping that path synchronous and DB-free.

**Fixture rework is part of this slice, not incidental.** `WorkflowPlan::default_ci`
(`plan/model.rs:60-90`) has twelve nodes, nine of them byte-touching, all with
`policy_target: None`. `durable_workflow_test.rs` submits it through the real scheduler at
nine call sites, and `expansion_test.rs:423` and `binding_test.rs:27-30` build on it. Those
fixtures gain a deterministic synthetic `TicketStorageSource` — including a root for the
`scan_library` node. Weakening or deleting that end-to-end scheduler coverage is not an
acceptable repair: it is the coverage this change most depends on.

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
count is bounded by the `MAX_ENTRIES` rule in §2 rather than by trusting the producer this
section declares untrusted. Be precise about what that rule is: a **canonicalization sanity
bound, not a denial-of-service control**. Both the entry vector and each `rights` vector are
materialized by serde before `new` runs, so neither rule prevents the allocation — the real
bound on allocation is the size of the `tickets.payload` row, which this change does not
alter. `rights` needs no length rule of its own: strict ascent over a three-variant enum
caps a valid entry at three, and an invalid one is rejected after materialization exactly as
an over-long entry list is. No rule is quadratic in a way an adversarial payload could
exploit: duplicate detection uses sorted comparison and hash sets. Failure messages name the
rule, the field, and the rejected ID, and never a locator, path, hostname, or provider
string.

**In scope, and worth naming because it is easy to miss.** A declaration's *rights* are
bound to its operation on the read side, not only the write side: §5 rule 4 requires
equality with `declaration_for`, so a corrupted row cannot give a `probe_file` ticket a
`write` or `delete` intent. A rule that checked only targets would have left the untrusted
writer free to escalate rights past every canonical-form check.

**Explicitly out of scope.** Whether the referenced root, location, or handle exists, is
live, is owned by the acquiring node, or is at the expected epoch — all of that is #476,
and a declaration that passes validation here proves only that it is well-formed for its
operation. Rights
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
- Unknown token inside the reserved namespace → `normalize` classifies it
  `UnknownNamespaced` and raises nothing. Two callers reject it, each handling one ticket:
  `acquire_guarded` (`leases.rs:319`) with `VoomError::Database` naming the field, and
  `parse_ticket` with a `WorkflowTicketPayloadError`. The three `workers.rs` capability and
  capacity functions do not raise — see §4's disposition table. Today's behavior for such a
  token is that `normalized_worker_operation` strips the namespace and returns `Ok("bogus")`,
  which matches no capability row either; the change is that the bare token stops being
  manufactured and the lease path rejects explicitly.
- Policy target that resolves to zero or several live rooted locations → the existing
  `VoomError::Config` messages from `select_location`, raised at render time.

## Compatibility

Pre-release, one-way. A byte-touching ticket row written by an earlier binary has no
`artifact_access` field and no longer decodes; no backfill can invent a root or location
that was never recorded, so none is attempted.

A ticket referencing a **pre-0034** location was already undispatchable, because migration
0034 quarantined those locations as unassigned legacy. A ticket rendered **after** 0034
references a live rooted location and dispatches normally today, so for those rows the
step below is not tidiness: skipping it converts completable work into terminal failures.

The upgrade step is therefore: quiesce workflow ticket creation, then fail or delete every
unfinished workflow ticket whose kind names a byte-touching operation, then swap the binary.
Quiescing first is load-bearing — the binary running the drain is the one still rendering
old-shape tickets, so draining against a live writer leaves everything rendered in the
window between drain and swap undecodable. Each ticket that instead reaches a terminal
transition opens a `terminal_failure` issue (ADR 0018, deduped per ticket), so the loss is
loud, but loud is not recovered. The step folds into ADR 0055's flag-day root-assignment and
rescan procedure, which such a deployment already owes.

**No shipped command performs either half of that step today.** `TicketCommand`
(`voom-cli/src/cli.rs:1371-1388`) offers only `List` and `Show`; there is no fail, no
delete, no filter by kind, and no quiesce switch. `voom job cancel` is not equivalent — it
takes non-byte-touching tickets with the job. So the procedure currently requires direct SQL
against `tickets`. That is stated plainly here and in `docs/release-process.md` rather than
implied, and #480 tracks shipping the control. It does not block this change: the gap is in
operator tooling for a procedure this change documents, and pre-release deployments already
owe ADR 0055's flag-day rescan.

Rollback keeps ADR 0013's snapshot restore as the safe default. This change also permits a
narrower option, because its new shape is confined to one column: quiesce, then fail or
delete the byte-touching tickets the new binary wrote. That preserves every other row the
new binary committed and leaves those tickets' workflows incomplete — a trade the operator
makes, not one this change makes for them.

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
  `rendered_payload.source_location_id` is rejected; a non-scan byte-touching payload with
  no `source_location_id` is rejected; a `scan_library` payload carrying one is rejected.
- `ticket_payload_test.rs`: the rule-4 equality check bites on **rights**, not just
  targets — a `probe_file` payload whose declaration names the right location but carries
  `read, write` instead of `read` is rejected, as is one carrying an extra `storage_root`
  write entry. This is the case a target-only check would pass, and it is the one that
  matters for a corrupted or hand-edited row.
- `binding_test.rs` / `tickets_test.rs`: a `TargetRef::FileVersion` node renders a
  `source_location_id`, so the field is no longer conditional on the target shape and the
  dispatch path consumes the render-time choice rather than re-resolving.
- `binding_test.rs::default_payload_rendering_covers_default_ci_operations`: every non-scan
  byte-touching node rendered through `render_default_payload_with_fan_out` carries
  `source_location_id`, `scan_library` does not, and a byte-touching branch with no
  `storage_source` returns a `BindingError`.
- `expansion_test.rs`: a scanner result whose `files[]` entries carry `file_location_id`
  expands to probe / hash / identity children whose declarations name that location and the
  parent scan ticket's root; a string-form file entry, and an object entry with a zero or
  missing `file_location_id`, are each rejected with the stated message.
- `artifact_access_test.rs`: `declaration_for` rejects `ScanLibrary` given a `Location`
  source and every other byte-touching operation given a `Root` source, so the two variants
  cannot be crossed.
- `artifact_access_test.rs`: a **frozen canonical-encoding fixture** — the byte-exact JSON
  of a declaration carrying one entry of each of the four target variants, with multi-right
  entries — is asserted in both directions. Reordering variants or fields turns it red.
  Prose alone would be silently ignorable, which is the failure class ADR 0013 exists to
  stop, and `check-payload-deny-unknown.sh` cannot see an ordering change.
- `workers_test.rs`: a candidate set containing one ticket whose kind is
  `synthetic.workflow.operation.` — the empty-suffix token, which `from_stored` rejects
  **today**, so the current code aborts the loop — alongside one well-formed ticket. After
  the change the first scores ineligible and the second stays eligible. The empty-suffix
  token is the one that makes this test red before the change; `…operation.bogus` would not,
  because `normalized_worker_operation` returns `Ok("bogus")` for it today.

**Criterion 2 — existing handles require a location/root reference; unmaterialized output
handles require a matching target-root reference.**
- `artifact_access_test.rs`: JSON for `existing_artifact` missing `file_location_id` or
  `storage_root_id` fails to deserialize; `planned_artifact` missing
  `target_storage_root_id` fails to deserialize; each complete form round-trips.
- `artifact_access_test.rs`: `planned_artifact` carrying `file_location_id` is rejected as
  an unknown field, proving the shapes are not interchangeable.

**Criterion 3 — duplicate, conflicting, zero-ID, malformed, and non-canonical
declarations fail before scheduling.**
- `artifact_access_test.rs`: one case per rule in §2, asserting the specific message —
  including a nine-entry list rejected by `MAX_ENTRIES` and an eight-entry list accepted.
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
- `ticket_operation_test.rs`: `probe_file` → `Known { ProbeFile, namespaced: false }`;
  `synthetic.workflow.operation.probe_file` → `Known { ProbeFile, namespaced: true }`;
  `disk.test` and `noop` → `CustomLocal`; `synthetic.workflow.operation.bogus` and
  `synthetic.workflow.operation.` → `UnknownNamespaced`.
- `ticket_operation_test.rs`: every `OperationKind::ALL` value yields the same `kind` from
  its exact and its namespaced token, and `matching_token()` returns the bare wire token for
  both. For `CustomLocal` and `UnknownNamespaced`, `matching_token()` returns the original
  token unmodified — in particular `synthetic.workflow.operation.bogus` does **not** become
  `bogus`.
- `leases_test.rs`: acquiring a ticket whose kind is `synthetic.workflow.operation.bogus`
  fails closed with a database error naming the field.
- `workers_test.rs`: `operation_capacity_in_tx`, `operation_capability_history`, and
  `operation_capability_details_in_tx` return normally for `synthetic.workflow.operation.`
  rather than raising, and report zero capability rows and the wildcard limit. That token
  errors today, so all three cases are red before the change. A second case pins the
  `matching_token()` behavior for `…operation.bogus`: the lookups bind the full token, not
  the fabricated bare `bogus` the current helper produces.
- `ticket_payload_test.rs`: `parse_ticket("probe_file", ..)` — a bare known token — is
  rejected, so the accepted ticket-kind encoding is unchanged and rule 3 of normalization
  does not widen it.
- Existing tests using `synthetic.workflow.operation.test` and `.extract` are updated to
  real operations, since those kinds are exactly what this criterion makes invalid.

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
