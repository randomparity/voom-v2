# ADR 0069: Byte-work tickets declare canonical artifact access

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

`WorkflowTicketPayload` gains `declared_artifact_access: Option<ArtifactAccessDeclaration>`,
required exactly when the operation is byte-touching and rejected otherwise. Both
`to_ticket_payload` and `parse_ticket` enforce it, so a ticket cannot be written or read
without it.

For the cross-check against `rendered_payload.source_location_id` to bind, that field has
to be present, and today it is emitted only for a `TargetRef::FileLocation` node. So the
renderer resolves every non-scan byte-touching node's source to exactly one live rooted
location and records it, whichever target shape the node carries. `scan_library` is the
one operation whose declaration does not name that location: it enumerates a root by
definition, so `entries_for` projects a location source to the location's root and emits a
root-only entry.

That makes the cross-check **not** total, and the gap is worth stating rather than
glossing. A scan node resolves its target like any other node, so `insert_storage_source`
writes both `source_storage_root_id` and `source_location_id` into its rendered payload,
and the declaration then binds only the root. A scan row can therefore carry any
`source_location_id` — corrupted, hand-edited, or stale — and still satisfy
`validate_artifact_access`. Only the root is cross-checked for that one operation. Nothing
consumes the unbound field (`expand_scanner_completion` reads `parent_storage_root`), and
`scan_library` has no production producer at all, so the exposure today is confined to
fixtures; it is recorded because the "total cross-check" claim is what the criterion 1
argument rests on, and that claim holds for every byte-touching operation except this one.

Each entry's rights come from the operation, and the mapping is fixed: `scan_library` reads
its root; `probe_file`, `hash_file`, and `verify_artifact` read their location; the seven
output-producing operations read their location and write their root; `delete_artifact`
reads and deletes its location.

Those readings describe a **location-addressed** render. A root-addressed one — a ticket
whose source is `TicketStorageSource::Root`, because it operates on staged output that has
no `file_locations` row yet — has no location to name, so every right it declares attaches
to the root: an output-producing operation collapses to a single `storage_root` entry
carrying `[read, write]`, a read-only operation to `[read]` on the root, and
`delete_artifact` to `[read, delete]` on the root.

**The gate is equality, not shape.** The declaration must equal the one that mapping
produces for the ticket's operation and source, entry for entry and right for right. A
shape check would bind only targets, leaving a corrupted or hand-edited row free to give a
`probe_file` ticket `write` and `delete` on its source and still pass every canonical-form
rule — and #476 would then gate on it. Equality extends the untrusted-writer stance to
rights, and makes one mapping the single definition of a valid declaration on both sides.
It stays synchronous and read-only: it proves the declaration is well-formed for its
operation, never that the storage exists or is owned.

A source is either a whole root or a location inside one, and **every byte-touching
operation accepts both**. No operation rejects a variant; `scan_library` alone *projects*,
declaring the root of a location source because a scan enumerates a root by definition.

Totality is not a nicety — it is what makes the workflow lattice renderable.
`expand_transform_completion` produces `back_up_file`, `commit_artifact`, and `edit_tracks`
children that all operate on the transform's **staged output**, which has no
`file_locations` row until commit creates one, so all three must be root-addressed. And
nothing can construct a root source directly: `select_location` returns a location,
`voom_policy::TargetRef` has no storage-root variant, and this slice does not widen the
accepted target shapes — so projection is the only way `scan_library`, a root node with no
parent to thread from, gets a root declaration at all. Restricting the variant per operation
was tried and breaks both cases. The alternatives stay rejected: inheriting the parent's
location entry is a live reference to the wrong bytes, and inventing a location ID is worse.

The residual is that an untrusted persisted row can drop `source_location_id` and present a
whole-root declaration that validates, because that is a legitimate shape for any
byte-touching operation. Addressing mode is not independently recorded, so no read-side rule
here reaches it. What bounds the consequence is that a forged root is no more useful than a
forged location: #476 re-derives ownership from the database and trusts neither.

The source reaches every renderer through `BranchContext`, not `node.policy_target()` —
the `scan_library` arm never reads the policy target. `render_node_ticket` resolves the
target once, populates `BranchContext`, and hands the resolved source to the payload
renderers so no arm resolves it a second time.

Fan-out children thread their source from the parent's `rendered_payload`, which carries
`source_storage_root_id` and, when the source is a location, `source_location_id` — for
**every** workflow ticket, byte-touching or not. Keying the threading on the declaration
instead would fail on three of the five expansion functions: a `scan_library` parent has no
location, a `score_quality` parent is not byte-touching and so is forbidden a declaration
at all, and the transform and backup children address different bytes than their parent.
Those two payload fields are also what give the read-side equality check an independent
anchor, so neither the root nor the location is read back out of the declaration being
validated. A scan result is the one place a child's location is discovered rather than
inherited, so `ScannerFile` carries a `file_location_id` per file.

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
`normalized_worker_operation` is deleted. `ticket_payload::ticket_operation` stays and is
rewritten onto `normalize`, because it is a seam rather than a duplicate: it is where a
workflow ticket kind is required to be namespaced, accepting only
`Known { namespaced: true }` and rejecting a custom local or unknown token.

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
- Rendering a ticket resolves its policy target to exactly one live rooted location, so a
  file version with zero or several live rooted locations fails closed at render time
  instead of at dispatch. For **workflow-rendered tickets** source resolution now has one
  point, not two; CLI- and API-initiated operations still pass an optional
  `source_location_id` and keep the dispatch-time resolution in
  `operation_source::select_location`, so that branch stays live and must not be deleted
  as dead.

  Resolution is keyed on the node **having** a policy target, not on its operation being
  byte-touching: a non-byte-touching node's payload is what its byte-touching children
  thread a source from, so skipping it there would strand them. The cost is that a target
  naming no file — `MediaWork`, `MediaVariant`, `AssetBundle`, `FileAsset`, `Synthetic` —
  now fails ticket render for any operation, where before only the five policy-rendered
  byte-touching arms resolved at all. That is unreachable today and deliberately not
  guarded: `policy_bridge::execution_operation` admits exactly five operations, all
  byte-touching and all already requiring a `FileVersion` or `FileLocation` target, and it
  is the only production `WorkflowPlan` constructor. Nothing in the types encodes that
  coupling, and `WorkflowPlan::validate` does not check it, so a sixth operation or a
  second constructor would surface the gap as a run-time render failure rather than a
  validation error. Adding the check now would be a guard for a plan shape no producer
  can emit; the coupling is recorded here instead.
- **An output-producing operation declares `write` on the root it reads from, which is
  not necessarily the root it writes to.** ADR 0055 resolves a destination as
  `default_output_root_id.unwrap_or(source)`, and `artifact_target_root` implements it,
  so whenever an operator has set that column to a second root the declared write names
  a root the ticket never writes and names nothing for the root it does. This is a known
  limitation of this slice, not an oversight: resolving the destination needs an
  `effective_library_root` lookup, which is cheap on the root render path but not on the
  expansion path, where `spec_for_branch` is deliberately synchronous and database-free.
  Fixing only the root path would make root-rendered and expansion-rendered tickets make
  different kinds of claim, which is worse than one uniform documented limitation. #484
  owns closing it. Until it does, **#476 must not read the write entry as a
  destination** — it names read locality, not write locality. The vocabulary is already
  sufficient for the fix: two `storage_root` entries with distinct ids are canonical
  today, so closing #484 adds an entry rather than a shape.
- **A root-addressed render declares the broadest claim the vocabulary can express, and
  its `read` is no more a locality statement than its `write` is.** The bullet above warns
  #476 off reading the write entry as a destination; the same warning applies to the read
  half, and to every right on a root-addressed entry. With no location to name, an
  output-producing operation collapses to one `storage_root` entry carrying
  `[read, write]` — a claim over every artifact in the root, for a ticket that reads one
  staged file. This is the *common* path rather than a corner: `expand_transform_completion`
  builds a `Root` source for its `back_up_file`, `commit_artifact` and `edit_tracks`
  children, and `expand_backup_completion` for its `verify_artifact` child, because staged
  output has no `file_locations` row to point at.

  Narrowing it needs a name for a not-yet-materialized artifact, which is exactly the
  `planned_artifact` handle variant this slice ships but does not produce (D3) — no handle
  ID exists at render time. So the breadth is a consequence of the vocabulary being ahead
  of its producers, not of the mapping being careless, and it resolves when #422/#476 make
  handle IDs available rather than by changing `entries_for`. Until then #476 and #477 must
  treat a root-addressed entry as "this ticket touches something in this root" and must not
  derive read locality, co-scheduling, or serialization scope from it — the over-broad
  reading would show up as unnecessary serialization, and only once a consumer exists.
- **The two producers of a `file_location` declaration entry do not validate it equally.**
  The policy path resolves through `resolve_policy_file_source`, which requires a live,
  rooted, non-retired location and takes the root from `location.rooted_address()` rather
  than trusting a second source. The scan path pairs a `storage_root_id` from the scan
  ticket's own payload with a `file_location_id` taken from the worker's result, checking
  only that it is a non-zero integer — so a declaration can assert that a location lives in
  a root without either being verified to exist or to belong together. Verifying it here is
  deliberately not done: the only shipped producer of those ids is the fake scanner, whose
  ids name no row by design, so the check would fail the fake flows immediately and the
  repair belongs with #476, which is where resolution starts and where that note is
  recorded. Nothing in production emits a `ScanLibrary` node today, so no live row carries
  such a pair.
- **Every future change to the operation→rights mapping repeats this release's payload
  break.** Validation compares a stored declaration against the whole entry list
  `declaration_for` computes from the mapping compiled into the reading binary, and the
  payload carries no version marker, so an edit to that mapping makes every in-flight
  persisted ticket for the affected operations undecodable — no backfill possible, drain
  required. The preceding bullet schedules exactly such an edit: closing #484 adds an
  entry for the seven output-producing operations, which is cheap in the vocabulary and
  not cheap in the stored rows, because the equality check binds the list. Versioning the
  declaration and tolerating older mappings would remove the recurrence, and is rejected
  here: it is more machinery than the risk warrants, and a reader accepting two mappings
  is the second accepted wire format acceptance criterion 6 forbids. The cost is paid at
  deployment instead, and `docs/release-process.md` carries the standing drain procedure
  rather than describing this release as a one-off.
- Variant and field declaration order in `ArtifactAccessTarget` is durable payload
  contract. Reordering either silently reclassifies every previously written declaration
  as non-canonical; `deny_unknown_fields` and `check-payload-deny-unknown.sh` give no
  signal, because no field is unknown and no attribute moved. A written rule would be
  silently ignorable — the failure class ADR 0013 exists to stop — so two sibling
  tests guard it, and it takes both. A frozen canonical-encoding fixture asserts the
  encoding of a multi-entry, multi-variant declaration by **string** comparison, which
  catches a variant reorder, a field reorder, and a tag rename; comparing `serde_json`
  `Value`s would not, because the workspace enables `preserve_order` and `IndexMap`'s
  `PartialEq` ignores key order. A second fixture holds two entries of the *same*
  variant differing only in a later field, which is what pins the derived `Ord`: the
  frozen fixture holds one entry per variant, so no comparison in it reaches past the
  variant discriminant, and a within-variant field swap would leave it green.
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
  loud at the granularity of the **run**, not the ticket: the first undecodable ticket in a
  ready batch aborts the workflow run it belongs to. It opens no `terminal_failure` issue
  (ADR 0018), because it never leases and so never reaches a terminal transition.

  Containing that to the ticket was specified and then withdrawn, and the reason is worth
  recording because the obvious fix does not work. Skipping the undecodable ticket alone
  livelocks: the row stays `ready`, so `workflow_idle_state` keeps reporting `Ready`,
  `wait_or_fail_idle` keeps returning `Ok`, and the loop spins on a batch that filters to
  empty. Raising once a poll batch holds no decodable ticket instead aborts while siblings
  are still leased — the store query sees only `ready` rows — forfeiting expansion children
  that a completing sibling would have produced. Both variants are worse than the abort.
  What makes skipping correct is a terminal transition that does not require a lease, so
  the row leaves `ready` for good; #486 owns that and carries the skip with it.
- Rollback is the mirror image and is the harder direction, because it is already an
  incident. ADR 0013's blanket remedy is to restore the pre-upgrade database snapshot, and
  that remains the safe default. This change narrows it deliberately: the new shape is
  confined to `tickets.payload` for every production row, so quiescing and then failing or deleting the
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
