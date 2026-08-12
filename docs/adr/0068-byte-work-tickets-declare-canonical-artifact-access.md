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
exactly one `storage_root` entry, taken from the source its renderer was given. The two
rules partition the byte-touching operations, so the check is total rather than conditional.

Widening that emission puts one more control-plane field inside the request envelope that
`dispatch.rs` clones to a worker, which is why the first rejected alternative below rejects
*untyped-only* storage identity rather than routing evidence in the payload as such.
`source_location_id` is already read by the control plane's own adapter
(`operation_adapters::source_location_id`), so its dual role predates this change; what is
new is that scheduling correctness now depends on it. #423, which replaces path-bearing
worker requests, inherits the obligation to preserve or relocate the field rather than drop
it with the paths.

A byte-touching node reaches a renderer through one of two paths, and neither may produce
an undeclared ticket. `executor/tickets.rs` resolves `node.policy_target()`; every
production plan node carries one. `plan/expansion.rs` is synchronous and touches no
database, so it threads the parent ticket's already-resolved source through
`BranchContext` exactly as it threads `source_file` today. A byte-touching node that
yields a source from neither — the `render_default_payload` fallback arms, reachable only
from the `#[cfg(test)]` demo plan — fails at render rather than emitting a ticket without
a declaration.

Resolution also happens once: the dispatch path already
consumes `source_location_id` and re-resolves only when it is absent, so recording it
removes a second, later resolution of "the single live rooted location" that could pick a
different row against a table that changed in between.

### One ticket-kind normalization

`TicketOperation::normalize_stored` becomes the single implementation.
`synthetic.workflow.operation.` is a reserved namespace: a token inside it must have a
known `OperationKind` suffix or normalization fails. A token outside every reserved
namespace is a custom local operation and normalizes to itself.
`normalized_worker_operation` and `ticket_payload::ticket_operation` are deleted.

Failing closed means *denied*, not *aborted*, and where the distinction lands matters.
`remote_acquire_candidates_in_tx` evaluates candidate tickets in a loop and
`?`-propagates each store call, so an error raised there ends evaluation for the whole
candidate set. One corrupt `tickets.kind` row would stall acquisition of every
well-formed ticket for that worker until an operator found it — a worse outcome than
today, where an unrecognized kind simply matches no capability row. So the store's
eligibility and capacity predicates report an unnormalizable kind as **ineligible with a
reason**, keeping the failure scoped to the offending ticket. Payload decode is the
opposite case and stays an error: `parse_ticket` already runs per ticket, so raising
there cannot spread.

## Consequences

- A byte-touching ticket that cannot name its storage cannot be created. Issue #476 gets
  a typed, locator-free reference set to resolve, and #477 through #479 get the same
  evidence without re-deriving it from paths.
- `synthetic.workflow.operation.<unknown>` stops matching any capability, so a ticket
  carrying one is never leased. It is scored ineligible rather than raising, so a single
  corrupt row cannot stall acquisition for the rest of the candidate set. Tests that lease
  such kinds must name a real operation.
- The declaration lives inside the durable `tickets.payload` column, so it joins the ADR
  0013 payload contract: its structs are listed in
  `docs/payload-contract-inventory.md` and `scripts/payload-contract-scope.txt`.
- Rendering a byte-touching ticket resolves its policy target to exactly one live rooted
  location, so a file version with zero or several live rooted locations fails closed at
  render time instead of at dispatch. For **workflow-rendered tickets** source resolution
  now has one point, not two; CLI- and API-initiated operations still pass an optional
  `source_location_id` and keep the dispatch-time resolution in
  `operation_source::select_location`, so that branch stays live and must not be deleted
  as dead.
- Variant and field declaration order in `ArtifactAccessTarget` is durable payload
  contract. Reordering either silently reclassifies every previously written declaration
  as non-canonical; `deny_unknown_fields` and `check-payload-deny-unknown.sh` give no
  signal, because no field is unknown and no attribute moved. A written rule would be
  silently ignorable — the failure class ADR 0013 exists to stop — so a frozen
  canonical-encoding fixture asserts the byte-exact encoding of a multi-entry,
  multi-variant declaration, and any reorder turns red.
- Of the twelve byte-touching operations, only `remux`, `transcode_video`,
  `transcode_audio`, `extract_audio`, and `verify_artifact` can reach a production ticket
  today: `policy_bridge::execution_operation` maps those five and errors on the rest, and
  the only other `WorkflowPlan` constructor is `#[cfg(test)]`. `scan_library` in particular
  has no production ticket producer at all — ADR 0067 moved scanning to durable scan
  sessions — so its rule and its `storage_root` entry are exercised only by fabricated test
  data. Three of the four target variants therefore ship without a production producer, and
  the fourth has five operations. The other seven byte-touching
  classifications are enforced but unexercised outside tests until their producers exist,
  so this slice removes proportionally less of #420's risk than the twelve-way
  classification suggests.
- A byte-touching ticket row written by an earlier binary has no `artifact_access` field
  and no longer decodes. This is a deliberate coordinated payload change, not a silent
  default: no backfill can invent a root or location that was never recorded. Such a
  ticket referencing a **pre-0034** location was already undispatchable, because migration
  0034 quarantined those locations as unassigned legacy. A ticket rendered **after** 0034
  references a live rooted location and dispatches normally today, so for those rows the
  drain step is not tidiness: skipping it converts completable work into terminal failures
  and loses it. The upgrade therefore gains one concrete step: before rolling the new binary
  out, fail or delete every unfinished workflow ticket whose kind names a byte-touching
  operation. Skipping it is loud rather than silent — each such ticket opens a
  `terminal_failure` issue per ADR 0018 when it reaches its terminal transition. This is a
  breaking `tickets.payload` change under ADR 0013, so the
  binary-before-DB ordering and this step are recorded in `docs/release-process.md`
  alongside the existing payload-compatibility rules; the step folds into ADR 0055's
  flag-day root-assignment and rescan procedure rather than standing alone.
- `existing_artifact` and `planned_artifact` have no producer yet, so their validation is
  exercised only by tests until #422 or #476 supplies one.

## Considered and rejected

- **Leave storage identity untyped, in the rendered payload only.** Rejected because
  ownership resolution would then read bare JSON numbers out of a request shape #423 owns
  and is about to rewrite, with no vocabulary for a target root or a handle. The decision
  does keep one routing field there — `source_location_id`, widened to every non-scan
  byte-touching ticket — because the cross-check needs something to bind against; the
  typed declaration, not that field, is the evidence #476 resolves.
- **A store-owned declaration type.** Rejected for the reason ADR 0053 gives: it reverses
  the crate layering and makes a domain decision depend on SQLite infrastructure.
- **Accept any entry order and deduplicate on read.** Rejected because a reader that
  repairs its input hides the producer bug that wrote a malformed declaration.
- **Accept any entry order, reject duplicates and conflicts, canonicalise only on write.**
  This repairs nothing, keeps one wire format, and removes the reorder hazard above, so it
  is the strongest alternative to the chosen rule. Rejected because criterion 3 requires
  that "non-canonical" declarations fail before scheduling, and an order-insensitive reader
  by definition does not. That reading of "non-canonical" is recorded as decision D4 on
  issue #475. It is cheap to reverse if wrong: a write-canonicalising producer emits
  byte-identical output either way, so relaxing the reader later invalidates nothing.
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
- **Ship only `storage_root` and `file_location` now.** Rejected on operator decision D3:
  the four content-struct shapes are what make criterion 2 structural — an existing handle
  without a location, or a planned one without a target root, has no encoding — where a
  two-variant vocabulary would re-add those rules as validation branches later. Deferring
  would have been cheap, though: adding a variant to an internally tagged enum is
  forward-additive, so old rows still decode and only rollback is affected, which ADR 0013
  already charges for every payload change.
- **Keep both normalizations and make them agree by review.** Rejected because they
  already disagree on unknown namespaced tokens, and the disagreement is invisible until
  a corrupt or hand-written ticket kind reaches acquisition.
- **Do nothing and let #476 derive intent from the operation and `source_location_id`.**
  This is genuinely cheap — the operation-to-entries mapping is total, and the location is
  an ID one lookup from its root, so nothing here needs paths. It also avoids both of this
  ADR's largest costs: the breaking `tickets.payload` change and the pre-upgrade drain step.
  Rejected because it leaves intent re-derived at scheduling time from a field the worker
  contract owns and #423 is about to rewrite, and because it keeps the second resolution
  point this decision closes — the ticket would still name no location, so dispatch would
  resolve one independently.
