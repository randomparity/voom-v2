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
location and records it, whichever target shape the node carries. `scan_library` is the
complement: it addresses a root, so it carries no `source_location_id`. The two cases
partition the byte-touching operations, so the check is total rather than conditional.

Each entry's rights come from the operation, and the mapping is fixed: `scan_library` reads
its root; `probe_file`, `hash_file`, and `verify_artifact` read their location; the seven
output-producing operations read their location and write their root; `delete_artifact`
reads and deletes its location.

**The gate is equality, not shape.** The declaration must equal the one that mapping
produces for the ticket's operation and source, entry for entry and right for right. A
shape check would bind only targets, leaving a corrupted or hand-edited row free to give a
`probe_file` ticket `write` and `delete` on its source and still pass every canonical-form
rule — and #476 would then gate on it. Equality extends the untrusted-writer stance to
rights, and makes one mapping the single definition of a valid declaration on both sides.
It stays synchronous and read-only: it proves the declaration is well-formed for its
operation, never that the storage exists or is owned.

The source reaches every renderer through `BranchContext`, not `node.policy_target()` —
the `scan_library` arm never reads the policy target. `render_node_ticket` resolves the
target once, populates `BranchContext`, and hands the resolved source to the payload
renderers so no arm resolves it a second time. Fan-out children cannot inherit a location
from a `scan_library` parent, which has none, so the scanner result carries a
`file_location_id` per file and the child pairs it with the parent's declared root.

`source_location_id` sits in the request envelope `dispatch.rs` clones to a worker, and is
already read by the control plane's own adapter, so its dual role predates this change;
what is new is that scheduling correctness depends on it. #423 therefore inherits the
obligation to preserve or relocate it rather than drop it with the paths.

Recording it also collapses two resolutions into one: dispatch re-resolves "the single live
rooted location" only when the field is absent, and a second resolution could pick a
different row against a table that changed in between.

That collapse has a symmetric cost, and this decision takes it deliberately (D5). Today a
`TargetRef::FileVersion` ticket re-resolves at dispatch, so one whose location was retired
and recreated by an ADR 0055 rescan still runs. Once the location is frozen into the
payload, `require_live_rooted_location` rejects it and the ticket fails terminally. The
alternative — treat the recorded ID as a hint and re-resolve when it is retired — would let
the declaration name one location while the process opens another, which is precisely the
divergence ADR 0050 exists to remove. A byte-touching ticket outliving a rescan of its own
root should fail rather than silently retarget; #479 owns making that replay-safe.

Neither render path may emit an undeclared ticket. A byte-touching node whose
`BranchContext` carries no source — the `render_default_payload` fallback arms, reachable
only from the `#[cfg(test)]` demo plan — fails at render.

### One ticket-kind normalization

`TicketOperation::normalize` becomes the single implementation. It classifies and does not
raise; rejection belongs to the callers that handle one ticket at a time.
`synthetic.workflow.operation.` is a reserved namespace: a token inside it must have a
known `OperationKind` suffix or normalization fails. A token outside every reserved
namespace is a custom local operation and normalizes to itself.
`normalized_worker_operation` and `ticket_payload::ticket_operation` are deleted.

Failing closed means *denied*, not *aborted*. `remote_acquire_candidates_in_tx` evaluates
candidates in a loop that `?`-propagates each store call, so raising there would let one
corrupt `tickets.kind` row stall acquisition of every well-formed ticket — worse than today,
where an unrecognized kind merely matches no capability. So the capability and capacity
functions never raise: they match on the unmodified token, which matches no row, and the
worker is ineligible by the same mechanism as today. Rejection lives in the two callers that
handle exactly one ticket — `acquire_guarded` on the lease path and `parse_ticket` on
decode — where raising cannot spread.

## Consequences

- A byte-touching ticket that cannot name its storage cannot be created. Issue #476 gets
  a typed, locator-free reference set to resolve, and #477 through #479 get the same
  evidence without re-deriving it from paths.
- `synthetic.workflow.operation.<unknown>` stops matching any capability, so a ticket
  carrying one is never leased. It is scored ineligible rather than raising, so a single
  corrupt row cannot stall acquisition for the rest of the candidate set. Tests that lease
  such kinds must name a real operation. The residual is that fail-closed here is silent:
  the ticket never leases, so it never attempts, never terminates, and never opens an ADR
  0018 issue, and the ineligibility reason is in-memory only. That is today's behavior
  carried forward, not a regression, but it is not observable and this change does not make
  it so.
- The `#[cfg(test)]` demo plan carries nine byte-touching nodes and no policy targets, and
  `durable_workflow_test.rs`, `expansion_test.rs`, and `binding_test.rs` all run on it.
  Giving that fixture resolvable sources is part of this slice, not incidental cleanup —
  and the repair must not be to weaken the end-to-end scheduler coverage this decision most
  depends on.
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
- Only `remux`, `transcode_video`, `transcode_audio`, `extract_audio`, and
  `verify_artifact` can reach a production ticket today: `policy_bridge::execution_operation`
  maps those five and errors on the rest, and the only other `WorkflowPlan` constructor is
  `#[cfg(test)]`. `scan_library` has no production producer at all — ADR 0067 moved scanning
  to durable scan sessions. So `file_location` is the one target variant with a production
  producer; the other three, and the other seven byte-touching classifications, are enforced
  but exercised only by tests until #422 or #476 supplies one. This slice removes
  proportionally less of #420's risk than a twelve-way classification suggests.
- A byte-touching row written by an earlier binary no longer decodes, and no backfill can
  invent a root or location it never recorded. Rows referencing **pre-0034** locations were
  already undispatchable — migration 0034 quarantined those as unassigned legacy — but a
  ticket rendered **after** 0034 dispatches normally today, so skipping the upgrade step
  loses completable work rather than delaying it. The step: before rolling the new binary
  out, fail or delete every unfinished workflow ticket whose kind names a byte-touching
  operation — and quiesce ticket creation first, because the binary performing the drain is
  the one still rendering old-shape tickets, so a drain run against a live writer leaves
  everything rendered in the window between drain and swap undecodable. Skipping the step is
  loud — each such ticket opens a `terminal_failure` issue per ADR 0018 at its terminal
  transition — but loud is not the same as recovered.
- Rollback is the mirror image and is the harder direction, because it is already an
  incident. ADR 0013's blanket remedy is to restore the pre-upgrade database snapshot, and
  that remains the safe default. This change narrows it deliberately: the new shape is
  confined to one column, `tickets.payload`, so quiescing and then failing or deleting the
  byte-touching tickets the new binary wrote is sufficient and preserves every other row the
  new binary committed — which a snapshot restore would discard. It also leaves the
  workflows owning those tickets incomplete, so an operator who wants a clean revert of
  everything still takes the snapshot. Both options and the narrowing go in
  `docs/release-process.md` as ADR 0013 requires, alongside the forward step, which folds
  into ADR 0055's flag-day root-assignment and rescan procedure.
- A byte-touching ticket that outlives a rescan of its own root now fails terminally rather
  than re-resolving, because its location is frozen at render (D5). #479 owns making that
  replay-safe.

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
- **Two typed fields on the payload instead of a list.** `storage_root_id` plus
  `file_location_id`, rights derived from `OperationKind`, would satisfy every criterion
  this slice owns and make emptiness, duplication, conflict, and ordering uninhabitable
  rather than rejected — deleting the canonical-form section outright. Rejected because it
  cannot express what the next slices need: a source location and a *different* target root
  are two references with different rights, and `default_output_root_id` already makes them
  differ. #477 and #422 would each reopen the payload shape; the list form pays that once.
  That is a forecast, so name what falsifies it and what being wrong costs: if #477 and #422
  land without ever needing a second reference carrying different rights, the list was
  speculative, and collapsing a permanently one-entry list back to two fields is itself
  another breaking payload change, not a free simplification.
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
  ADR's largest costs: the breaking `tickets.payload` change and the drain step. Rejected on
  one ground, stated at the size it actually holds: this slice replaces a bare JSON number
  in a shape #423 owns and is about to rewrite with a typed, locator-free reference that
  #476 can gate on. The target-root and handle vocabulary is *not* part of that ground —
  nothing produces it yet. Nor is closing the second resolution point, which comes from
  widening `source_location_id` emission inside the untyped `rendered_payload` and is
  separable, so do-nothing could take it too.
- **Land the `voom-core` vocabulary now, defer the payload field to #477.** One coordinated
  break instead of two, no drain step, and every typed-vocabulary benefit this slice
  actually claims. Rejected on operator decision D6: criterion 1 requires that every
  byte-touching ticket *carries* the declaration, so deferring the field would ship a
  vocabulary nothing uses and satisfy criterion 4 alone — the slice would have to be
  re-scoped and its criteria rewritten rather than met.
- **Classify `scan_library` as not byte-touching, now that ADR 0067 owns scanning.** This
  deletes the scan special case, the two-arm partition, and `storage_root`'s only named
  producer. Rejected because scanning does read bytes and #421 moves exactly that work to
  the owner node; classifying it false to shed an unexercised branch would put the wrong
  answer in a closed vocabulary that #421 then has to correct.
