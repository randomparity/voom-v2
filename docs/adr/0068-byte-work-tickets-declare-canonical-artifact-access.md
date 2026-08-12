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

### Byte-touching is a closed property of the operation

`OperationKind::is_byte_touching` classifies every variant through an exhaustive match, so
adding an operation without classifying it fails to compile. `identify_media`,
`score_quality`, and `sync_external_system` are false; the remaining twelve are true.

`WorkflowTicketPayload` gains `artifact_access: Option<ArtifactAccessDeclaration>`,
required exactly when the operation is byte-touching and rejected otherwise. Both
`to_ticket_payload` and `parse_ticket` enforce it, so a ticket cannot be written or read
without it. Where `rendered_payload.source_location_id` is present, the declaration must
contain exactly one entry naming that location with `read` among its rights; the typed
declaration and the untyped rendered payload therefore cannot drift.

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
  render time instead of at dispatch.
- A byte-touching ticket row written by an earlier binary has no `artifact_access` field
  and no longer decodes. This is a deliberate coordinated payload change, not a silent
  default: no backfill can invent a root or location that was never recorded. Migration
  0034 already quarantined every pre-existing file location as unassigned legacy, so such
  a ticket was already ineligible for byte work; it now fails terminally at decode
  instead of at dispatch. Operators drain byte-touching tickets before upgrading.
- `existing_artifact` and `planned_artifact` have no producer yet. They are defined now
  because the vocabulary is durable: adding a variant later would be a coordinated
  binary-before-data change under ADR 0013, and their field shapes are what make the
  handle rules structural rather than another validation branch.

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
- **Ship only `storage_root` and `file_location` now.** Rejected on operator decision:
  the handle rules would be vacuous, and the later variant addition would be a coordinated
  change to a durable payload rather than a compile-time-checked addition today.
- **Keep both normalizations and make them agree by review.** Rejected because they
  already disagree on unknown namespaced tokens, and the disagreement is invisible until
  a corrupt or hand-written ticket kind reaches acquisition.
- **Do nothing and let #476 infer intent from the operation and rendered payload.**
  Rejected because inference reintroduces path-derived reasoning at exactly the seam ADR
  0050 removes it from.
