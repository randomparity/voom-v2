# ADR 0068: Byte-work tickets declare canonical artifact access

## Status

Accepted

## Context

ADR 0050 makes the storage-owner node the only actor that touches bytes, and ADR 0055
gives every live file location a stable root relationship. Issue #420 must stop
non-owner and mixed-owner workers acquiring byte-touching work. Before ownership can be
resolved or gated, a ticket has to say which storage it intends to touch — today it does
not.

A workflow ticket currently carries `operation: OperationKind` plus an untyped
`rendered_payload: Value`. Storage identity reaches the payload as bare JSON numbers
(`source_file_version_id`, `source_location_id`) and as path strings meaningful only on
the host that produced them. Nothing distinguishes a ticket that reads bytes from one
that does not, and nothing constrains which storage a ticket may name.

Two independent implementations already normalize a ticket kind into an operation:
`voom_store::repo::execution::workers::normalized_worker_operation` strips the
`synthetic.workflow.operation.` prefix and accepts whatever remains, and
`voom_control_plane::workflow::plan::ticket_payload::ticket_operation` strips the same
prefix and requires a known `OperationKind`. They disagree on unknown tokens: the store
turns `synthetic.workflow.operation.bogus` into the operation `bogus` and matches it
against worker capabilities, while the control plane rejects it.

Artifact handle rows are created at stage and commit time, after a worker has produced
output. No ticket renderer can name an artifact handle, and `voom_policy::TargetRef` has
no artifact-handle variant.

## Decision

### One canonical declaration, owned by `voom-core`

`voom-core` gains `ArtifactAccessDeclaration`: a non-empty, canonically ordered list of
`ArtifactAccessEntry { target, rights }`. It follows ADR 0053's precedent — shared
domain vocabulary used by scheduling, durable payloads, and later access plans belongs in
core, not in persistence.

`ArtifactAccessTarget` is a closed, internally tagged vocabulary whose variants are
newtype variants over `deny_unknown_fields` content structs, per ADR 0013:

| Variant | Fields | Meaning |
|---|---|---|
| `storage_root` | `storage_root_id` | whole-root access: enumeration, or write of an artifact not yet named |
| `file_location` | `storage_root_id`, `file_location_id` | an existing live location inside a root |
| `existing_artifact` | `artifact_handle_id`, `storage_root_id`, `file_location_id` | a materialized artifact handle |
| `planned_artifact` | `artifact_handle_id`, `target_storage_root_id` | an output handle whose bytes do not exist yet |

Targets carry stable IDs only. No path, locator, mount name, or host string appears in a
declaration, so nothing in it can be mistaken for proof of locality.

`ArtifactAccessRight` is `read`, `write`, or `delete`. Rights state intent. The type
exposes no method that resolves, authorizes, or performs anything; authorization lives in
lease acquisition and commit authority, which this vocabulary does not reach.

### Canonical form is the only accepted form

Construction and deserialization run the same validation, so there is exactly one
accepted wire shape:

- the entry list is non-empty;
- each entry's rights are non-empty and strictly ascending in `read < write < delete`
  order, which rejects duplicate and unordered rights together;
- entries are strictly ascending by target, which rejects duplicate and unordered entries
  together;
- no `file_location_id` and no `artifact_handle_id` appears in more than one entry;
- every ID is non-zero.

Zero is rejected because `voom-core` ID newtypes are deliberately unvalidated
`u64` wrappers, so a defaulted or truncated field would otherwise read as a valid
reference.

Because ordering decides whether a persisted payload decodes, the total order is part of
the wire contract and is stated here rather than left to a derive: targets order by
variant as `storage_root < file_location < existing_artifact < planned_artifact`, and
within a variant by field in declaration order. Rights order as `read < write < delete`.

### Byte-touching is a closed property of the operation

`OperationKind::is_byte_touching` classifies every variant through an exhaustive match, so
adding an operation without classifying it fails to compile. `identify_media`,
`score_quality`, and `sync_external_system` are false; the remaining twelve are true.

`WorkflowTicketPayload` gains `artifact_access: Option<ArtifactAccessDeclaration>`,
required exactly when the operation is byte-touching and rejected otherwise. Both
`to_ticket_payload` and `parse_ticket` enforce it, so a ticket cannot be written or read
without it.

For the cross-check against `rendered_payload.source_location_id` to bind, that field has
to be present, and today it is emitted only for a `TargetRef::FileLocation` node. So the
renderer resolves every non-scan byte-touching node's source to exactly one live rooted
location and records it, whichever target shape the node carries. The declaration must then
contain exactly one entry naming that location with `read` among its rights. `scan_library`
is the complement: it addresses a root, so it carries no `source_location_id` and declares
exactly one `storage_root` entry. The two rules partition the byte-touching operations, so
the check is total rather than conditional, and the resolution happens once: the dispatch
path already
consumes `source_location_id` and re-resolves only when it is absent, so recording it
removes a second, later resolution of "the single live rooted location" that could pick a
different row against a table that changed in between.

### One ticket-kind normalization

`TicketOperation::normalize_stored` becomes the single implementation.
`synthetic.workflow.operation.` is a reserved namespace: a token inside it must have a
known `OperationKind` suffix or the call fails closed as a database error. A token
outside every reserved namespace is a custom local operation and normalizes to itself.
`normalized_worker_operation` and `ticket_payload::ticket_operation` are deleted.

## Consequences

- A byte-touching ticket that cannot name its storage cannot be created. Issue #476 gets
  a typed, locator-free reference set to resolve, and #477 through #479 get the same
  evidence without re-deriving it from paths.
- `synthetic.workflow.operation.<unknown>` stops being accepted at lease acquisition.
  Tests that lease such kinds now fail closed and must name a real operation.
- The declaration lives inside the durable `tickets.payload` column, so it joins the ADR
  0013 payload contract: its structs are listed in
  `docs/payload-contract-inventory.md` and `scripts/payload-contract-scope.txt`.
- Rendering a byte-touching ticket resolves its policy target to exactly one live rooted
  location, so a file version with zero or several live rooted locations fails closed at
  render time instead of at dispatch. Source resolution now has one point, not two.
- Variant and field declaration order in `ArtifactAccessTarget` is durable payload
  contract. Reordering either silently reclassifies every previously written declaration
  as non-canonical; `deny_unknown_fields` and `check-payload-deny-unknown.sh` give no
  signal, because no field is unknown and no attribute moved. Treat a reorder as a
  breaking change under ADR 0013.
- Of the twelve byte-touching operations, only `remux`, `transcode_video`,
  `transcode_audio`, `extract_audio`, and `verify_artifact` can reach a production ticket
  today: `policy_bridge::execution_operation` maps those five and errors on the rest, and
  the only other `WorkflowPlan` constructor is `#[cfg(test)]`. The other seven
  classifications are enforced but unexercised outside tests until their producers exist,
  so this slice removes proportionally less of #420's risk than the twelve-way
  classification suggests.
- A byte-touching ticket row written by an earlier binary has no `artifact_access` field
  and no longer decodes. This is a deliberate coordinated payload change, not a silent
  default: no backfill can invent a root or location that was never recorded. Such a
  ticket cannot be drained by completing it — migration 0034 already quarantined every
  pre-existing file location as unassigned legacy, so it was already undispatchable — so
  the available action before upgrading is to identify and fail or delete pre-upgrade
  byte-touching workflow tickets. Each one that instead reaches a terminal transition
  opens a `terminal_failure` issue per ADR 0018, so an upgrade over a non-empty queue
  turns silently stuck tickets into a burst of issues. This creates no new operator
  procedure: ADR 0055's flag-day migration already requires deliberate root assignment and
  a rescan before byte work can resume, and this precondition rides with it.
- `existing_artifact` and `planned_artifact` have no producer yet, so their validation is
  exercised only by tests until #422 or #476 supplies one.

## Considered and rejected

- **Leave storage identity in the untyped rendered payload.** Rejected because the
  rendered payload is the worker-facing request shape owned by #423; overloading it with
  routing evidence gives ownership resolution an input that changes whenever a worker
  contract changes.
- **A store-owned declaration type.** Rejected for the reason ADR 0053 gives: it reverses
  the crate layering and makes a domain decision depend on SQLite infrastructure.
- **Accept any entry order and deduplicate on read.** Rejected because two byte-identical
  intents would have several accepted encodings, which is the second wire format
  criterion 6 forbids, and because a reader that repairs its input hides the producer bug.
- **Two typed fields on the payload instead of a list.** Since the declaration *is* the
  ticket's typed storage identity and the operation-to-entries mapping is total and
  static, `storage_root_id` plus `file_location_id` with rights derived from
  `OperationKind` would satisfy every criterion this slice owns, and would make
  emptiness, duplication, conflict, and ordering uninhabitable instead of rejected —
  deleting the whole canonical-form section. Rejected because it cannot express the
  shape the next slices need: a source location and a *different* target root are two
  references with different rights, and `default_output_root_id` already makes them
  differ (`operation_source::artifact_target_root`). #477's durable access plans and
  #422's commit intents would each require reopening the payload shape, which is the
  coordinated change the list form pays for once.
- **Ship only `storage_root` and `file_location` now.** Rejected on operator decision D3.
  The honest reason is that the four content-struct shapes are what make criterion 2
  structural — an existing handle without a location, or a planned one without a target
  root, has no encoding — where a two-variant vocabulary would have to re-add those rules
  as validation branches later. The cost of deferring is smaller than it first appears:
  adding a variant to an internally tagged enum is forward-additive, so old rows still
  decode and only rollback is affected, which ADR 0013 already charges for every payload
  change.
- **Keep both normalizations and make them agree by review.** Rejected because they
  already disagree on unknown namespaced tokens, and the disagreement is invisible until
  a corrupt or hand-written ticket kind reaches acquisition.
- **Do nothing and let #476 infer intent from the operation and rendered payload.**
  Rejected because inference reintroduces path-derived reasoning at exactly the seam ADR
  0050 removes it from.
