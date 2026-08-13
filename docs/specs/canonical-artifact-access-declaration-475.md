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

New module `crates/voom-core/src/taxonomy/artifact_access_declaration.rs`, re-exported from
`voom_core`, following the `taxonomy/storage.rs` pattern of a validating constructor plus
a `Deserialize` impl that routes through it.

**Naming, deliberately not `artifact_access`.** `voom-core` already owns
`taxonomy/artifact_access_mode.rs` (ADR 0053: `ArtifactAccessMode` = `shared_mount`,
`control_plane_placeholder`, `staged_output_placeholder`), and `worker_capabilities` already
has an `artifact_access` column of those tokens that `operation_eligibility_in_tx`
(`workers.rs:982-1000`) reads — the very function #476 extends. Two unrelated vocabularies
sharing one name in one query path is how a *right* gets paired with a *placement mode*. So
the module is `artifact_access_declaration.rs`, the control-plane helper is
`workflow/plan/access_declaration.rs`, and the payload field is
`declared_artifact_access`. `ArtifactAccessMode` and its column are untouched.

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

No entry-count bound is imposed. Criterion 3 lists duplicate, conflicting, zero-ID,
malformed, and non-canonical declarations and says nothing about length, and the effective
bound at the persisted boundary is **two** anyway: §5 rule 4 requires equality with
`declaration_for`, whose mapping produces at most two entries, and under D3 no handle-bearing
entry can reach a ticket at all. A constant, a rule, a message, and two tests serving slices
that have not been designed is the repo's no-speculative-features rule in miniature. #422 and
#477 add a bound when they have a shape that needs one.

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
| `workers.rs:1152` `operation_capacity_in_tx` | `matching_token()`; never raises. The double bind at `workers.rs:1158` — which today re-prefixes the stripped token to count leases under both forms — becomes conditional: bind `matching_token()` always, and the namespaced form **only** for `Known`. For `UnknownNamespaced` the current code would otherwise produce `synthetic.workflow.operation.synthetic.workflow.operation.bogus`, a token nothing can match, and the arithmetic would come out right only by accident. The store's local `WORKFLOW_OPERATION_PREFIX` (`workers.rs:1489`) is deleted with the helper; `voom_core::WORKFLOW_OPERATION_NAMESPACE` replaces it |
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
pub declared_artifact_access: Option<ArtifactAccessDeclaration>,
```

`to_ticket_payload` (encode) and `parse_ticket` (decode) both call one shared
`validate_artifact_access`, so the same contract binds writers and readers:

1. `operation.is_byte_touching()` and `declared_artifact_access` is `None` → reject. Non-emptiness
   needs no separate check; the type cannot hold an empty declaration.
2. `!operation.is_byte_touching()` and `declared_artifact_access` is `Some` → reject. A ticket that
   touches no bytes has no access to declare, and a stray declaration would be evidence
   #476 must not act on.
3. `operation.is_byte_touching()` → `rendered_payload.source_storage_root_id` must be
   present and a non-zero `u64`; call it `r`. `rendered_payload.source_location_id` is
   present and non-zero for a location-addressed ticket; call it `l`. Absent means the
   ticket addresses the root itself.
4. the declaration must then equal, **entry for entry and right for right**,
   `declaration_for(operation, Location { r, l })` when `l` is present, or
   `declaration_for(operation, Root { r })` when it is absent.

Both `r` and `l` are anchored to independent `rendered_payload` fields, never read back out
of the declaration being validated. An earlier draft took the root from the declaration's
own entry, which made the check circular: a corrupted row could name any root and pass,
and the root is exactly the field #476 gates node ownership against. Note what this still
does not prove — that `r` is the root actually containing `l`. That is a database fact and
belongs to #476, which must re-derive the root from the location rather than trust the
declared one. The threat model says so explicitly.

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

`validate_artifact_access` rejects with these exact messages, for the same reason §2 pins
its own — a test that asserts "is rejected" ratifies whatever the first implementation
emits. All are `WorkflowPlanError`-wrapped `WorkflowTicketPayloadError`:

| Rule | Message |
|---|---|
| 1 — byte-touching, declaration absent | `operation {op} is byte-touching and requires declared_artifact_access` |
| 2 — not byte-touching, declaration present | `operation {op} is not byte-touching and must not declare artifact access` |
| 3 — root missing or zero | `byte-touching payload for {op} requires a non-zero rendered_payload.source_storage_root_id` |
| 3 — location present but zero | `rendered_payload.source_location_id must be non-zero` |
| 4 — equality fails | `declared_artifact_access does not match the canonical declaration for {op}` |

`declaration_for` is total over byte-touching operations and both source variants, so rule 4
has no "invalid pair" case to report — a mismatch is always a mismatch.

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
locator-free.

Because both target shapes now route through `select_location`, `require_live_rooted_location`
runs first and rejects a non-`Rooted` address with
`file_location {id} must have a rooted address`, so `rooted_address()`'s own error arm is
unreachable — but it is still **propagated, never `expect`ed**, because unreachability here
is an argument about callers, not a type-level guarantee.

That is a behavior change worth naming: a `TargetRef::FileLocation` node pointing at a
live-but-unrooted location — the migration-0034 `unassigned_legacy` shape the Compatibility
section invokes — is rejected at render today only by accident of later path resolution.
`resolve_policy_file_source`'s current branch (`executor/tickets.rs:255-266`) checks only
`retired_at`; after this change it also gets the `FileLocationAddress::Rooted` guard. Note
what it does *not* gain: `require_live_rooted_location`'s `file_version_id` cross-check is
vacuous on this path, because the current code derives `PolicyFileSource.file_version_id`
from `location.file_version_id` (`tickets.rs:262-265`), so the comparison is the field
against itself. The cross-check is real only for a `FileVersion` target, where the expected
version comes from the policy target independently. Moving the rooted guard from dispatch to
render is the same trade decision D5 makes for location freezing. A byte-touching node whose `policy_target` is neither `FileVersion` nor
`FileLocation` keeps the existing `resolve_policy_file_source` error
(`executor/tickets.rs:268-270`) — this slice does not widen the accepted target shapes.

A new `crates/voom-control-plane/src/workflow/plan/access_declaration.rs` holds:

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
root-addressed ticket: it would force a fabricated non-zero `file_location_id` that
`declaration_for` discards, and under D1's declaration-is-the-identity reading a fabricated
ID is a live reference to somebody else's location. No operation rejects a variant; see the
mapping table below for how each is interpreted.

The mapping is total over `OperationKind` × source variant. Rights are a property of the
operation; the source variant decides whether they attach to a location or to the root:

**No operation rejects a source variant.** Rights are a property of the operation; the
source variant decides only whether they attach to a location or to the root. `scan_library`
is the one operation that *projects*: given a `Location { r, l }` it declares
`storage_root(r)`, because a scan enumerates a root by definition and the location it was
resolved from is not what it addresses.

| Operation | rights | `Location { r, l }` | `Root { r }` |
|---|---|---|---|
| `identify_media`, `score_quality`, `sync_external_system` | — | `Ok(None)` — not byte-touching | `Ok(None)` |
| `scan_library` | `read` | `storage_root(r)` `read` — projected | `storage_root(r)` `read` |
| `probe_file`, `hash_file`, `verify_artifact` | `read` | `file_location(r, l)` `read` | `storage_root(r)` `read` |
| `remux`, `transcode_video`, `transcode_audio`, `extract_audio`, `edit_tracks`, `back_up_file`, `commit_artifact` | `read`, `write` | `storage_root(r)` `write` + `file_location(r, l)` `read` | `storage_root(r)` `read, write` |
| `delete_artifact` | `read`, `delete` | `file_location(r, l)` `read, delete` | `storage_root(r)` `read, delete` |

Entries in the two-entry row are written in canonical order — `storage_root` sorts before
`file_location` per §2 — so the table can be transcribed literally. `declaration_for` does
not sort; it constructs in this order and `new` accepts it.

**Why every operation accepts `Root`.** Three of the five `expand_*_completion` functions
produce byte-touching children whose bytes are staged and unnamed. `expand_transform_completion`
alone produces `back_up_file`, `commit_artifact`, and `edit_tracks` children, all operating
on the transform's output, which has no `file_locations` row until commit creates one.
Restricting `Root` to a hand-picked set of operations makes the demo plan unrenderable at
those children, and the two alternatives the spec has already rejected remain rejected:
inheriting the parent's `file_location` entry is a live reference to the wrong bytes, and
inventing a location ID is worse.

**Why `scan_library` projects rather than rejecting.** Nothing can produce a `Root` source
at render time: `select_location` returns a `FileLocation`, `voom_policy::TargetRef` has no
storage-root variant, and this slice does not widen the accepted target shapes. `scan` is
also a root node with no parent to thread from. Projection is what makes a root-addressed
declaration reachable at all for the one operation that must have one.

**The residual, stated rather than engineered away.** An untrusted persisted row can drop
`source_location_id` from, say, a `transcode_video` ticket and present a whole-root
declaration that validates cleanly, because that is a legitimate shape for the operation.
The threat model's stance on rights does not extend to target granularity, and no rule
available here would give it that reach — the addressing mode is not independently recorded
anywhere. What bounds the consequence is that a forged root is no more useful than a forged
location: #476 re-derives ownership from the database and must not trust either.

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
no artifact handle exists at render time. **That entry names the source root, not the
destination root.** An earlier draft of this paragraph said "the target root is what is
knowable", which implied the opposite and was wrong: selecting a distinct output root
from the source root's `default_output_root_id` is `artifact_target_root`'s job, and this
slice does not do it. So when an operator has set `default_output_root_id` to a second
root, the declared write names a root the ticket never writes and names nothing for the
root it does. #484 owns closing that; #476 must not read the entry as a destination. See
the ADR 0068 consequence of the same name for why it was not closed here.

`insert_policy_file_source` (`binding.rs:323-331`) currently writes `source_location_id`
only when `PolicyFileSource.location_id` is `Some`, which `resolve_policy_file_source`
sets only for a `TargetRef::FileLocation` node.

`PolicyFileSource` becomes `{ file_version_id, storage_root_id: StorageRootId,
location_id: FileLocationId }` — the root is added and the location stops being optional.
`resolve_policy_file_source` routes **both** target shapes through `select_location`
(decision D2), so a `TargetRef::FileVersion` resolves to its single live rooted location and
a `TargetRef::FileLocation` gains the `require_live_rooted_location` checks it does not run
today. `insert_policy_file_source` writes `source_storage_root_id` and `source_location_id`
on every policy-rendered payload.

This is the load-bearing half, not a detail: `policy_bridge::execution_operation` admits
exactly `remux`, `transcode_video`, `transcode_audio`, `extract_audio`, and
`verify_artifact` as production nodes, all five byte-touching and all five policy-rendered.
The policy renderers take a `PolicyFileSource`, never a `BranchContext`, so widening only
`render_default_payload_with_fan_out` would leave every production ticket rejected by rule 3
at encode — and the fixture suites, which go through the default renderer, would be green
while the first real workflow could not create a ticket.

That helper covers only the four policy renderers (`binding.rs:208, 233, 285, 319`). The
other renderer — `render_default_payload_with_fan_out` (`binding.rs:23-99`) — emits
`path`, `operation`, `branch_id`, `duration_ms`, and `progress_interval_ms` and nothing
else, and `executor/tickets.rs:209`'s `_ =>` arm plus every `expansion.rs` branch ticket
route through it. It therefore gains the same fields: after the per-operation match, insert
`source_storage_root_id` — and `source_location_id` when the source is a `Location` — from
`branch.storage_source` alongside the existing `operation` / `branch_id` insertions. This
happens for **every** operation whose branch carries a source, not only byte-touching ones,
because a non-byte-touching ticket's payload is what its byte-touching children thread from.
A byte-touching operation whose branch carries no source is a `BindingError` naming the
operation. Without this, rule 3 rejects at encode every ticket that renderer produces —
including the nine byte-touching nodes of the demo plan — and the only repairs available
would be the two the spec forbids.

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

**Threading, and why it is keyed on the rendered payload rather than the declaration.**
Five `expand_*_completion` functions build children from a parent ticket, and the naive rule
"inherit the parent's declaration" fails on three of them:

- `expand_scanner_completion` (`expansion.rs:52-83`) — the parent is `scan_library`, which
  holds a root and no location, while every child needs one.
- `expand_quality_completion` (`expansion.rs:111-135`) — the parent is `score_quality`, which
  is **not** byte-touching, so §5 rule 2 forbids it a declaration entirely. Its `remux` /
  `transcode_video` children are byte-touching and need the original file's location.
- `expand_transform_completion` / `expand_backup_completion` (`expansion.rs:138-180`) — the
  children operate on the transform's staged output and the backup artifact, not on the
  parent's source, so inheriting the parent's `file_location` entry would name the wrong
  bytes.

So the threaded identity lives in `rendered_payload`, not in the declaration, and every
workflow ticket carries it — byte-touching or not:

- `source_storage_root_id`: always present when the ticket has a source. This is also what
  gives §5 rule 3 an independent anchor for `r`.
- `source_location_id`: present when the ticket addresses a location rather than a root.

`score_quality` therefore carries both and declares nothing, which is exactly what makes its
byte-touching children renderable. `expand_*_completion` reads the two fields off the parent
payload and builds the child's `TicketStorageSource` — synchronous, DB-free, and uniform
across all five functions. The transform and backup children take
`Root { source_storage_root_id }` because their bytes are staged and unnamed.

`ScannerFile` gains `file_location_id: FileLocationId`, parsed from a required, non-zero
`file_location_id` on each `result.files[]` object, because a scan result is the one place a
child's location is discovered rather than inherited. The string form of a file entry
(`expansion.rs:381-385`) no longer satisfies a byte-touching expansion and is rejected with
`scanner result file entry requires file_location_id`.

This is the fixture migration the operator authorized (D7). It touches `ScannerFile`,
`scanner_files`, all five `expand_*_completion` functions, and every test that seeds a
scanner result or a workflow payload; none of it is reachable in production, where
`policy_bridge::execution_operation` admits no `scan_library` node at all.

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
private helper of `render_node_ticket` with a single call site. Two consequences of
hoisting, both deliberate: `render_node_ticket` resolves whenever the node carries a policy
target, byte-touching or not. Resolving only for byte-touching nodes would contradict the
threading invariant above — a non-byte-touching root node's payload is what its byte-touching
children thread from, so skipping its resolution leaves them with nothing. `default_ci`'s only
root node is `scan` and `policy_bridge::execution_operation` admits no non-byte-touching
production node, so the case is latent today; the invariant is asserted unconditionally, so
the resolution is unconditional too. And
`render_root_remux_payload`'s own target-shape rejection (`executor/tickets.rs:233-236`)
becomes unreachable once the hoisted arm fires first, so it is **deleted** rather than left
as dead code with a message no path can produce.
`expansion.rs` threads a parent ticket's already-resolved source into the same field,
exactly as it already threads `source_file`, keeping that path synchronous and DB-free.

**Fixture rework is part of this slice, and the mechanism is seeded rows, not a new field.**
`WorkflowPlan::default_ci` (`plan/model.rs:60-90`) has twelve nodes, nine of them
byte-touching, all built by helpers that hardcode `policy_target: None` (`model.rs:201, 219,
236`). `durable_workflow_test.rs` submits it through the real scheduler at nine call sites,
and `expansion_test.rs:423` and `binding_test.rs:27-30` build on it. That end-to-end
scheduler coverage is what this change most depends on; weakening or deleting it is not an
acceptable repair.

The source reaches those nodes the same way it reaches a production node: through
`policy_target`. `default_ci`'s byte-touching nodes gain
`policy_target: Some(TargetRef::FileLocation { id })`, and the tests **seed a real storage
root and file-location row** for that ID. This repository's store tests already run against
real on-disk SQLite with an injected clock, so seeding two rows is the ordinary fixture
shape, and `voom-test-support` is where the helper belongs.

The rejected alternative is giving `OperationNode` a `storage_source` field:
`OperationNode` is a production, `Serialize`-deriving type (`model.rs:16-24`), so a field
whose only producer is `#[cfg(test)]` code would change the serialized plan shape to serve a
fixture.

Two claims an earlier draft made are withdrawn as false. This slice **does** resolve against
the database — `select_location` is a real read, hoisted into `render_node_ticket` — so
synthetic non-zero IDs are *not* valid inputs to it, and a fixture that supplies an
unseeded ID gets `NotFound`. "Routing evidence only" describes what the declaration is used
for downstream, not an absence of reads while producing it.

### 6a. Every `parse_ticket` consumer

Criterion 6 says "all direct serializers and consumers", so here is the complete set. Five
non-test call sites exist: `plan/expansion.rs:533`, `executor/tickets.rs:284`,
`executor/expansion.rs:157`, `executor/errors.rs:45`, and `summary.rs:176`. The first two
fail per ticket and need no change. The other three do not:

- **`workflow/execution/executor/expansion.rs:141-166`** loops a batch of ready workflow
  tickets and `?`-propagates a `VoomError::Internal` per ticket, so one undecodable
  old-shape row aborts `ready_workflow_tickets` for the entire job. That is the same
  batch-scoped hazard ADR 0068 argues against at `remote_acquire_candidates_in_tx`, and this
  change is what makes decode failures newly possible there — so it is ours to contain. The
  loop **skips** an undecodable ticket and records it, exactly as the acquisition candidate
  loop scores rather than raises. Without this, skipping the upgrade drain produces a stall
  rather than the per-ticket `terminal_failure` issues ADR 0068, this spec, and
  `docs/release-process.md` all describe as "loud".

- `workflow/summary.rs:175-179` uses `let Ok(payload) = … else { continue; }`, so an
  undecodable ticket vanishes from `branch_count` and `per_operation[..].ticket_count` while
  the raw `ticket_count` (`summary.rs:159`) still counts it — an internally inconsistent
  summary rather than an error. This bites hardest on **completed** old-shape rows, which the
  upgrade drain does not cover (it names unfinished tickets) and which never take the
  terminal-failure path the Compatibility section calls "loud".
- `workflow/execution/executor/errors.rs:44-53` maps a decode failure to
  `VoomError::Internal("…payload decode…")`, so a terminally failed old-shape ticket reports
  a decode error instead of `workflow ticket {node_id} failed`.

Neither is redesigned here, and — after checking what a counter would actually cost — neither
gains a field.

`branch_count` and `ticket_count` are durable columns on the summary row
(`voom-store/.../workflow_summaries.rs:241` `SUMMARY_COLS`, bound at `:381-382`, read at
`:835-836`) under ADR 0006. A `skipped_undecodable` that an operator can see therefore needs a
column and a migration, which this slice forbids outright; one that stays in memory leaves the
persisted row exactly as inconsistent as before, so it buys nothing but a new
`merge_invocation` rule (`summary.rs:29-57`) that a later ADR 0009 resume can get wrong. An
earlier draft specified the counter without noticing either cost.

So the behavior is left as it is and recorded instead: after the upgrade, a **completed**
old-shape byte-touching row — which the drain does not cover, since the drain names unfinished
tickets — is skipped by `summary.rs` forever, and historical workflow summaries under-report
`branch_count` and per-operation counts while `ticket_count` still includes it. The
Compatibility section's claim that the loss is "loud" covers the terminal-failure path only;
completed rows never take it. A test in
`voom-control-plane/.../workflow/summary_test.rs` pins the current behavior so a later change
to it is deliberate: one undecodable row leaves `ticket_count` unchanged and reduces the
per-operation total by one.

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
count is not separately bounded, and that is deliberate rather than an oversight. Both the
entry vector and each `rights` vector are materialized by serde before `new` runs, so a
length rule would not prevent the allocation it appears to guard — the real bound is the size
of the `tickets.payload` row, which this change does not alter. `rights` is capped at three by
strict ascent over a three-variant enum, and §5 rule 4's equality with `declaration_for` caps
a *persisted* declaration at two entries, which is a far tighter bound than any constant. No rule is quadratic in a way an adversarial payload could
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
`declared_artifact_access` field and no longer decodes; no backfill can invent a root or location
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
narrower option, because its new shape is confined to `tickets.payload` for every production
row: quiesce, then fail or
delete the byte-touching tickets the new binary wrote. That preserves every other row the
new binary committed and leaves those tickets' workflows incomplete — a trade the operator
makes, not one this change makes for them.

This is a breaking `tickets.payload` change under ADR 0013, which requires the
binary-before-DB ordering to be recorded in `docs/release-process.md`. That file gains the
precondition in this change; the surface list grows by that one file for that reason.

One narrowing is **not** in `tickets.payload`: `tickets.result` for `scan_library` now
requires each `result.files[]` entry to be an object with a non-zero `file_location_id`, and
the string form is rejected. `tickets.result` is an inventoried payload-contract column
(`docs/payload-contract-inventory.md`), though this region is decoded from an untyped
`Value` rather than a `deny_unknown_fields` struct. No production row is affected —
`policy_bridge::execution_operation` admits no `scan_library` node — so the confinement claim
above holds for production rows, which is what the narrowed rollback option rests on. The
rollback direction is safe regardless: the old parser reads `path` by name and ignores extra
fields.

No schema change, no migration, and no new dependency.

## Test strategy and acceptance criteria

Every criterion below maps to at least one test that fails before the change.

**Criterion 1 — every byte-touching ticket carries a non-empty canonical declaration
whose stable references match the ticket's typed identity.**
- `voom-control-plane/.../plan/ticket_payload_test.rs`: encoding a byte-touching payload with `declared_artifact_access: None`
  is rejected; encoding a non-byte-touching payload with `Some(..)` is rejected; both
  round-trip successfully in the correct configuration.
- `voom-control-plane/.../plan/ticket_payload_test.rs`: table-driven over all fifteen `OperationKind` values, so the
  requirement cannot be satisfied for one operation and missed for another.
- `voom-control-plane/.../plan/ticket_payload_test.rs`: a declaration naming a different `FileLocationId` than
  `rendered_payload.source_location_id` is rejected; a byte-touching payload with no
  `source_storage_root_id` is rejected; a `probe_file` payload with no `source_location_id`
  yields a whole-root declaration and is accepted, which is the documented residual; a
  `scan_library` payload carrying one projects to `storage_root` and is accepted; every
  byte-touching operation is accepted in *both* source forms.
- `voom-control-plane/.../plan/ticket_payload_test.rs`: the rule-4 equality check bites on **rights**, not just
  targets — a `probe_file` payload whose declaration names the right location but carries
  `read, write` instead of `read` is rejected, as is one carrying an extra `storage_root`
  write entry. This is the case a target-only check would pass, and it is the one that
  matters for a corrupted or hand-edited row.
- `voom-control-plane/.../plan/binding_test.rs` and
  `voom-control-plane/.../execution/executor/tickets_test.rs` (new sibling file): a `TargetRef::FileVersion` node renders a
  `source_location_id`, so the field is no longer conditional on the target shape and the
  dispatch path consumes the render-time choice rather than re-resolving.
- `voom-control-plane/.../plan/binding_test.rs::default_payload_rendering_covers_default_ci_operations`: every non-scan
  byte-touching node rendered through `render_default_payload_with_fan_out` carries
  `source_location_id`, `scan_library` does not, and a byte-touching branch with no
  `storage_source` returns a `BindingError`.
- `voom-control-plane/.../plan/expansion_test.rs`: a scanner result whose `files[]` entries carry `file_location_id`
  expands to probe / hash / identity children whose declarations name that location and the
  parent scan ticket's root; a string-form file entry, and an object entry with a zero or
  missing `file_location_id`, are each rejected with the stated message.
- `voom-control-plane/.../plan/access_declaration_test.rs`: `declaration_for` is total —
  table-driven over all fifteen `OperationKind` values times both source variants, asserting
  the exact entry list and rights of every cell in the §6 mapping table, and that every
  produced declaration is accepted by `ArtifactAccessDeclaration::new` in the order
  `declaration_for` builds it. `ScanLibrary` given a `Location` projects to `storage_root`.
- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: a **frozen canonical-encoding fixture** — the byte-exact JSON
  of a declaration carrying one entry of each of the four target variants, with multi-right
  entries — is asserted in both directions. Reordering variants or fields turns it red.
  Prose alone would be silently ignorable, which is the failure class ADR 0013 exists to
  stop, and `check-payload-deny-unknown.sh` cannot see an ordering change.
- `voom-control-plane/.../cases/execution/remote_execution/acquire_test.rs` (new sibling file, per ADR 0004): a candidate set containing one ticket whose kind is
  `synthetic.workflow.operation.` — the empty-suffix token, which `from_stored` rejects
  **today**, so the current code aborts the loop — alongside one well-formed ticket. After
  the change the first scores ineligible and the second stays eligible. The empty-suffix
  token is the one that makes this test red before the change; `…operation.bogus` would not,
  because `normalized_worker_operation` returns `Ok("bogus")` for it today.

**Criterion 2 — existing handles require a location/root reference; unmaterialized output
handles require a matching target-root reference.**
- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: JSON for `existing_artifact` missing `file_location_id` or
  `storage_root_id` fails to deserialize; `planned_artifact` missing
  `target_storage_root_id` fails to deserialize; each complete form round-trips.
- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: `planned_artifact` carrying `file_location_id` is rejected as
  an unknown field, proving the shapes are not interchangeable.

**Criterion 3 — duplicate, conflicting, zero-ID, malformed, and non-canonical
declarations fail before scheduling.**
- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: one case per rule in §2, asserting the specific message —

- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: a deterministic exhaustive test builds a fixed set of four
  distinct entries, enumerates all 24 orderings in code, and proves exactly the ascending
  ordering is accepted and `serialize → deserialize` is the identity on it. No new
  dependency: the permutations are generated by an in-test loop, since the workspace
  carries no property-testing crate and this change adds none.
- `voom-control-plane/.../plan/expansion_test.rs`: a ticket whose payload carries a non-canonical declaration is
  rejected by `parse_ticket` before `mark_ready_if_unblocked` can promote it.

**Criterion 4 — exact custom local operations remain supported; known exact and
namespaced operations normalize deterministically; unknown namespaced operations fail
closed.**
- `voom-core/src/taxonomy/ticket_operation_test.rs`: `probe_file` → `Known { ProbeFile, namespaced: false }`;
  `synthetic.workflow.operation.probe_file` → `Known { ProbeFile, namespaced: true }`;
  `disk.test` and `noop` → `CustomLocal`; `synthetic.workflow.operation.bogus` and
  `synthetic.workflow.operation.` → `UnknownNamespaced`.
- `voom-core/src/taxonomy/ticket_operation_test.rs`: every `OperationKind::ALL` value yields the same `kind` from
  its exact and its namespaced token, and `matching_token()` returns the bare wire token for
  both. For `CustomLocal` and `UnknownNamespaced`, `matching_token()` returns the original
  token unmodified — in particular `synthetic.workflow.operation.bogus` does **not** become
  `bogus`.
- `voom-store/src/repo/execution/leases_test.rs`: acquiring a ticket whose kind is `synthetic.workflow.operation.bogus`
  fails closed with a database error naming the field.
- `voom-store/src/repo/execution/workers_test.rs`: `operation_capacity_in_tx`, `operation_capability_history`, and
  `operation_capability_details_in_tx` return normally for `synthetic.workflow.operation.`
  rather than raising, and report zero capability rows and the wildcard limit. That token
  errors today, so all three cases are red before the change. A second case pins the
  `matching_token()` behavior for `…operation.bogus`: the lookups bind the full token, not
  the fabricated bare `bogus` the current helper produces.
- `voom-control-plane/.../plan/ticket_payload_test.rs`: `parse_ticket("probe_file", ..)` — a bare known token — is
  rejected, so the accepted ticket-kind encoding is unchanged and rule 3 of normalization
  does not widen it.
- Existing tests using `synthetic.workflow.operation.test` and `.extract` are updated to
  real operations, since those kinds are exactly what this criterion makes invalid.

**Criterion 5 — rights describe intended access only and cannot independently authorize
mutation.**
This criterion is negative — it asserts an absence — so it is proved behaviorally rather
than by a test that inspects the type. Note that the obvious phrasing is *unwritable* under
rule 4: two tickets differing only in rights cannot both exist, because exactly one rights
set is valid per operation and source. That is a stronger property than the one the
criterion asks for, and the tests assert it directly:
- `crates/voom-control-plane/src/workflow/plan/ticket_payload_test.rs`: a `probe_file`
  payload whose declaration escalates to `read, write` — or to `read, write, delete` — is
  rejected at both `to_ticket_payload` and `parse_ticket`. A ticket cannot carry a right its
  operation does not have, so no right can reach a consumer that was not put there by
  `declaration_for`.
- `crates/voom-store/src/repo/execution/leases_test.rs`: a valid byte-touching ticket whose
  declaration carries `write` leases exactly as one carrying only `read` does — same
  eligibility, same capacity, same outcome. Nothing in the lease path reads a right.
- The existing commit-authorization and use-lease gate tests in `voom-control-plane` pass
  unchanged, proving no authorization path gained the declaration as an input.
- `artifact_access_declaration.rs` exposes only `new`, the accessors named in §2, and its serde impls.
  Nothing in the module reads a `ArtifactAccessRight` to decide anything, and no caller
  outside it may, which the ADR records as the governing constraint for later slices.

**Criterion 6 — strict payload tests cover all direct serializers and consumers without a
second accepted wire format.**
- `just check-payload-deny-unknown` covers the new core module after it is added to
  `scripts/payload-contract-scope.txt`; its `-selftest` sibling proves the guard still bites.
- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: an unknown field at every level — entry, each content struct,
  and an unknown `kind` — is rejected.
- `voom-core/src/taxonomy/artifact_access_declaration_test.rs`: reordered entries, reordered rights, and duplicated rights all
  fail to deserialize, which is what "no second accepted wire format" means operationally.
- `crates/voom-control-plane/tests/`: the existing `remux_flow`, `video_transcode_flow`,
  `audio_extract_flow`, `staged_artifact_flow`, and `phase_barrier_flow` integration suites
  pass with declarations flowing end to end.

**Guardrails.** `just ci` passes with zero failures and zero warnings. No test is added
ignored, and no existing test is deleted except the two obsolete
unknown-namespaced-operation cases named above, whose behavior is replaced by the
fail-closed tests in criterion 4.
