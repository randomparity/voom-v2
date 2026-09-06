# 0097 — Pre-promotion artifact addresses are contained by their own storage root

## Status

Accepted

## Context

Issue #615 asks whether the pre-promotion `<staging-root>/.committed/<op>/…`
address belongs inside a registered storage root, and which root the containment
guard should measure it against.

Today one guard answers both questions at once, and gets the second one wrong.
`ControlPlane::commit_artifact` reaches `resolve_artifact_target`
(`crates/voom-control-plane/src/operation_source.rs:158-172`), which resolves a
single target root from the *source* root's `default_output_root_id` — falling
back to the source root itself when none is configured
(`operation_source.rs:185-188`) — and then requires the commit path to start with
that root's canonical locator (`require_contained`, `operation_source.rs:290-302`).

The address it is handed is not the durable commit address. In the owner-node
flow the staged output is written under the root the media-dispatch envelope
resolved for `DestinationRole::Staging`
(`crates/voom-cli/tests/support/owner_node.rs:490-494`), and the commit target is
built beneath that same path as `<root>/.committed/<op>/tN-<name>`
(`owner_node.rs:579-583`). In the transitional coordinator promotion path the
working dir is `committed_working_dir()`
(`crates/voom-control-plane/src/cases/policy/compliance.rs:686-696`), which joins
`.committed/<op>` onto the raw filesystem path the operator passed to
`voom compliance execute --staging-root`; no storage root row is created for it.
Both are staging addresses; only the later promotion into `--output-dir`
(`crates/voom-control-plane/src/workflow/coordinator/promotion.rs:723-760`)
produces a durable output address.

So the guard measures a staging address against the output root, and the two
roots have no stated path relationship. ADR 0074 requires only that the staging
root and the target root resolve to the same owner node — "one node must both
read the staging bytes and promote into the target" — and fails pre-mutation
otherwise. The stronger property the guard actually demands, that the staging
root's path be *nested inside* the output root's path, is written down nowhere,
and nothing enforces it at configuration time. It surfaces as
`CONFIG_INVALID: artifact commit path escaped storage root <id>` only after a
transcode has already run — `require_contained` returns `VoomError::Config`,
which `crates/voom-core/src/error.rs:371` maps to `ConfigInvalid` and `:183`
renders, and `crates/voom-control-plane/src/operation_source_test.rs:181` pins it
for this exact escape. Issue #497
attributes the weekly `chaos-e2e` failures #470 and #491 to this failure mode;
both of those issue bodies carry only an Actions run URL, so that attribution is
inherited here rather than verified.

The in-tree fixture at `crates/voom-cli/tests/support/voom_cli.rs:48-50` leaves
`default_output_root_id` NULL, so containment measures against the source root
through the `unwrap_or(source_storage_root_id)` fallback that the fourth decision
below removes — not through the three roots being collapsed onto one id.

A second, related divergence has to be settled with it. Two resolution routes for
one concept disagree about an unconfigured output root: `artifact_target_root`
falls back silently to the source root, while `destination_root`
(`crates/voom-control-plane/src/workflow/plan/envelope.rs:197-228`) is fail-closed
— its doc comment states the intent, that "the bundled fallback that used to
execute such tickets is gone, so an unaddressable destination must fail the
render." ADR 0055 is the source of the first behavior: it records that artifact
finalization "selects an explicit target root from that source root's configured
output-root relationship, falling back to the same root," and that the resolver
"proves the target path is contained by that target root." Containment at commit
is therefore an accepted decision, not an emergent one. What ADR 0055 does not
state, and what this record settles, is *which* root a pre-promotion address is
contained by.

Constraints in play. ADR 0050 makes storage-owner node agents perform every byte
read and mutation, and marks the control plane's filesystem-promotion path
transitional pending #416–#425; nothing here may extend it. ADR 0055 makes a
storage root the addressing domain: every live file location is
`(storage_root_id, provider_relative_locator)`, and locations that could not be
rooted were quarantined by migration 0034 as ineligible for work. ADR 0075
carries only `(StorageRootId, ProviderRelativeLocator)` pairs across the
control-plane↔agent boundary, so a dispatched staging destination is a rooted
address by construction. The durable model has no rootless-scratch concept to
appeal to.

## Decision

### A pre-promotion address is contained by the root that owns it

The `<staging-root>/.committed/<op>/…` address is a storage-root-contained
location, and the root that contains it is the **staging** root, not the output
root. Containment is a property of an address inside its own addressing domain.
It is never a property of two roots' locators relative to each other.

"The staging root" means the registered `library_roots` row resolved for
`DestinationRole::Staging` — the root `destination_root`
(`envelope.rs:197-228`) returns — and not merely whatever directory prefix an
address happens to have been built under. The two readings coincide only when the
staging path is a registered root, and on the transitional coordinator path they
do not: `committed_working_dir` joins `.committed/<op>` onto an operator-supplied
path that no `library_roots` row describes. Under this decision such a path has
**no** containment root, and that is a fail-closed error — not an address
trivially contained by itself.

Concretely, a resolver validating an address resolves its containment root from
the role the address serves: a pre-promotion staging address against the root
`destination_root` resolves for `DestinationRole::Staging`, a durable output
address against the `default_output_root_id` of whichever root its caller passes —
the source media root at `crates/voom-control-plane/src/artifact/commit/prepare.rs:229-236`,
the artifact's own root at `promotion.rs:742`, which under this decision is the
staging root and must therefore carry its own output default. Applying the output
root's containment to a staging address is the defect; the guard is correct and is
applied against the wrong root.

### The staging↔output relationship is ADR 0074's shared owner node, and nothing more

ADR 0074's requirement — that the staging root and the target root resolve to the
same owner node — is confirmed as the whole of the relationship. This record adds
no path-nesting requirement between them and forbids one being introduced. A
staging root that is a sibling of, or wholly unrelated to, the output root is a
correct configuration provided both are registered roots resolving to the same
owner node and the same library.

The two halves stand differently today. The same-library half is already enforced
at configuration time on both write paths: `require_default_ids_in_library`
(`crates/voom-store/src/repo/library/library_roots.rs:614-634`) forces every
`default_*_root_id` into the naming root's library, reached from
`update_library_root` (`:309`) and from `create_library_root` through
`require_defaults_in_library` (`:602`). The owner-node half is enforced nowhere at
configuration time. At run time `artifact_target_root` checks only that the
*output* root shares the *source* root's library (`operation_source.rs:196-202`)
and is owned by the control plane's own local node
(`require_effective_local_root_path`, `operation_source.rs:275`); the staging root
is never passed to that function. So #616's genuinely new work is the owner-node
condition and the owner-assignment path below, not the whole check.

### The rule #616 enforces at configuration time

`voom library root add` and `voom library root update` reject a
`--staging-root <id>`, `--output-root <id>`, or `--backup-root <id>` value on root
R when the named root does not exist, does not belong to R's library, resolves to
an owner node other than R's, or — for a `--staging-root <id>` value — does not
itself carry a `default_output_root_id`. That last clause is what the first
decision makes necessary: promotion resolves the durable output address from the
staging root's own output default, so a staging root without one is a
configuration that fails after a transcode rather than at configuration time.
They perform **no** filesystem path comparison between roots.

Two completeness points bind that rule. First, a third path can invalidate a
pairing without writing a default column. `require_default_ids_in_library`
(`crates/voom-store/src/repo/library/library_roots.rs:614-634`) admits any
non-retired same-library root as a default, with no state or owner requirement,
and `assign_library_root_owner_in_tx` (`:361-390`) then permits that root's owner
to change while it is `unassigned` or `configured` with no activation identity. So
the owner-agreement check must also run at owner assignment, **in both
directions** — against every root that names the reassigned root as a default, and
against every default the reassigned root itself names. One direction alone leaves
the reassigned root's own pairings unchecked, which is the reachable case: a
migrated root R is quarantined with a null owner, `update_library_root` accepts
`R.default_staging_root_id = S` because same-library is all that is checked, and
`assign_library_root_owner_in_tx(R, N)` then succeeds without ever looking at S.
#616's body is right that `create_library_root` and `update_library_root` are the
only writers of the default columns; the owner-assignment path writes none of them
and is still a way a valid pairing goes stale.

Second, ADR 0055 quarantined migrated roots as `unassigned` with a null owner. A
pairing is undecidable at configuration time only while an owner is genuinely
absent, because no rule can decide agreement between an owner and an absence. Owner
assignment is the moment the absence becomes a presence and the pairing first
becomes decidable, which is why the check must run there; only pairings whose owner
is still absent are inherited by the run-time pre-mutation check.

### An unaddressable destination fails closed

`artifact_target_root`'s silent fallback to the source root is removed. When no
`default_output_root_id` is configured for the root whose output default is being
resolved — the source media root at commit, the artifact's own staging root at
promotion — resolving a durable commit target fails with an actionable error
naming that root and the `voom library root update --output-root <id>` that fixes
it. This converges both
routes on `destination_root`'s fail-closed semantics, for the reason
`destination_root` already gives: an unaddressable destination must fail rather
than be guessed.

This **amends ADR 0055** in one clause only — "falling back to the same root" no
longer holds. ADR 0055's root identity model, its provider-relative location
model, and its containment requirement are untouched and still govern. ADR 0069's
Consequences (`:255-257`) restate that same clause descriptively — "ADR 0055
resolves a destination as `default_output_root_id.unwrap_or(source)`, and
`artifact_target_root` implements it" — so that sentence goes stale with it. ADR
0069's own decision, that byte-work tickets declare canonical artifact access, is
untouched.

## Consequences

- This record decides the rule; it does not implement it. Applying containment
  against the staging root and removing `artifact_target_root`'s source-root
  fallback are edits to `crates/voom-control-plane/src/operation_source.rs`, owned
  by **#625**, which is blocked on this record landing. No other child of the #497
  epic covers that work — #616 is configuration-time validation and is forbidden
  the path change by the rule above, #617 is an error-message change, #618 is the
  `--staging-root` reconciliation, #623 is the harness reconciliation — and read
  together they imply a coverage that did not exist until #625 was filed.
- Removing the source-root fallback carries an in-tree cost. Two paths that reach
  a commit or promotion set `default_output_root_id`
  (`crates/voom-control-plane/src/artifact/commit/mod_test.rs:1467`,
  `crates/voom-control-plane/src/operation_source_test.rs:67`); eighteen fixtures
  construct it as `None` and depend on the fallback, including
  `crates/voom-cli/tests/support/voom_cli.rs:48-50`. Each of the latter that
  reaches a commit or promotion must gain an explicit output root, and that work
  belongs to #625, not to #497's closure.
- **The amendment is discoverable from the records it amends.** This repository's
  convention is a `## Later decision:` section added to the amended record in the
  same commit as the amending one — ADR 0050's commit `b3ccc609` did exactly that
  to ADRs 0019 (`:99`), 0025 (`:154`), 0027 (`:194`), and 0034 (`:202`). This
  change follows it: ADR 0055 gains `## Later decision: pre-promotion address
  containment` and ADR 0069 gains `## Later decision: fail-closed destination
  resolution`, both appended, both naming this record and issue #615 and the exact
  clause amended. Neither record is otherwise altered, and `docs/adr/README.md`
  could not have substituted — it is a two-column `| ADR | Title |` table with no
  status field.
- Open issue #484 is affected and must be restated. Its acceptance criterion names
  the rule this record removes: the byte-work declaration should name the resolved
  destination root "using the same `default_output_root_id.unwrap_or(source)` rule
  as `artifact_target_root`, so the two cannot disagree." After this record there
  is no such rule, so #484's criterion becomes "name the resolved output root"
  without the fallback clause. Its own subject, `declaration_for`, is untouched by
  #625; only the criterion's wording is.
- #616 is unblocked to implement the same-library, same-owner-node configuration
  check above, at `create_library_root`, `update_library_root`, and the
  owner-assignment path. Its premise changes: it must **not** implement the
  path-containment check its body describes, and the filesystem canonicalization
  question that made it look migration-adjacent disappears with it. Its open
  question about pre-existing rows narrows to rows whose staging and output roots
  have different assigned owners.
- #618 is unblocked to make `default_staging_root_id` the single source of truth,
  and this record makes that reconciliation load-bearing rather than tidying: the
  containment root is now a registered root, so a `--staging-root` path that names
  no registered root has no containment root at all. The reconciliation stays
  inside ADR 0050's constraint — no new control-plane-owned filesystem behavior,
  and `promotion_plan()`'s surface holds or shrinks. Until #618 lands there is a
  residual worth stating: no configuration-time surface can see the `--staging-root`
  path, since #616's rule validates only the three `default_*_root_id` columns, so
  an unregistered staging path is still rejected at commit time — the same class of
  late failure this record's Context names as the harm.
- #623's question is answered, but not on the axis either harness's author framed
  it on. What matters is not whether the staging tree is nested inside the library
  root; it is whether the staging path is a registered storage root that some root
  names as its `default_staging_root_id`. The Rust harness
  (`crates/voom-cli/tests/chaos_librarian_e2e.rs:247-266`, with
  `crates/voom-cli/tests/support/voom_cli.rs:48-50`) satisfies that, because the
  scan root is a registered root made its own staging default — while its comment's
  stated reason, that a staging root outside the storage root makes the commit path
  escape it, describes the containment this record rejects. The shell harness
  (`scripts/chaos-e2e-local.sh:161-167`) passes `$workdir/staging-<checkpoint>`,
  which sits outside `$run_dir` entirely (`library_dir="$run_dir/library"`, `:49`)
  and which no `library_roots` row describes — only `$library_dir` is registered,
  by `voom scan --path "$library_dir"` at `:113`. Under this decision that is a
  fail-closed error. #623's report that it nonetheless "does not fail" has a plain
  cause: `:11` defaults `CHAOS_EXECUTE_POLICY` to `0` and `:160` gates the
  `compliance execute --staging-root` call on it, so a default run never reaches a
  commit. That layout is untested rather than passing, and no CI run exercises it —
  the weekly job runs `just chaos-e2e-ci` (`justfile:249-252`), which is the Rust
  harness only. #623 must not run before #625 lands, or it will assert a layout
  the code still rejects.
- Removing the fallback is a behavior change for any deployment relying on the
  implicit "write beside the source" default; it must now set `--output-root`
  explicitly. Because promotion resolves the output default from the staging root,
  every root named as a `default_staging_root_id` must now carry an output default
  too — the configuration-time clause above is what stops that surfacing after a
  transcode. The project is pre-release, so no migration or deprecation window is
  owed.
- **Accepted residual: a staging root can be retired while it is another root's
  staging default.** `retire_library_root_in_tx`
  (`crates/voom-store/src/repo/library/library_roots.rs:438-458`) guards only that
  the root is not already retired; it consults no default column, and
  `require_default_ids_in_library`'s `state != 'retired'` predicate gates the write
  of a default, never the later retirement of a root already named as one. Moving
  the containment root onto the staging root — the root whose purpose is
  transience — means a committed-but-unpromoted artifact can now outlive its
  containment root. The first rejected alternative below argues from exactly this
  epoch-and-retirement lifecycle against path nesting, and honesty requires
  applying it to the option taken as well. This is accepted and unowned: no issue
  covers it, and closing it would need a retirement-time referential check that no
  criterion here asks for.
- Nothing in this record ships behavior. Until #625 lands, the
  emergent nesting requirement still binds at run time and the in-tree fixtures
  still depend on the fallback.

## Considered & rejected

- **Keep the guard where it is and make staging↔output path nesting a stated
  contract.** verified: `crates/voom-cli/tests/chaos_librarian_e2e.rs:247-266`
  already encodes that arrangement, passing the scan root as `--staging-root` so
  the commit path stays inside the library root, and its comment states the
  nesting as a requirement; ADR 0055 makes a root an addressing domain with its
  own epoch and retirement lifecycle, so nesting one inside another gives every
  scratch byte two containing domains and no rule for which epoch fences it.
- **Drop the containment check on the pre-promotion address entirely, treating
  `.committed` as rootless scratch.** verified: ADR 0055's migration 0034
  quarantined exactly the rootless case as ineligible for work, and ADR 0075
  carries only `(StorageRootId, ProviderRelativeLocator)` pairs across the
  control-plane↔agent boundary, so a dispatched staging destination has no
  rootless representation to express.
- **Read "the staging root" as the directory prefix the address was built under,
  rather than a registered root.** verified: that reading makes every address
  trivially contained by its own prefix, so the guard decides nothing — and it is
  unavailable to the envelope path regardless, where `destination_root`
  (`envelope.rs:197-228`) returns a `StorageRootId` and there is no prefix to read.
- **Contain the pre-promotion address in the source root, stating today's fallback
  behavior as the contract.** judgment: this is what actually runs — with no output
  default configured, `unwrap_or(source_storage_root_id)`
  (`operation_source.rs:185-188`) already makes the source root the containment
  root, and every in-tree fixture and the Rust chaos harness satisfy it. It names a
  registered root for every pre-promotion address, needs no nesting between staging
  and output, and would avoid the fourth decision and its fixture cost entirely.
  Rejected because once the staging and source roots genuinely differ, "contained by
  whichever root the source media happens to live in" is arbitrary — the staging
  bytes have no relationship to the source root beyond provenance — and it preserves
  the silent fallback that made the two resolution routes diverge in the first place.
- **Supersede ADR 0074 and restate the staging↔target relationship.** judgment:
  ADR 0074's shared-owner-node rule is the correct and sufficient relationship and
  needs no change; superseding a record to confirm it would retire a live decision
  to say nothing new.
- **Keep `artifact_target_root`'s source-root fallback and make
  `destination_root` fall back the same way.** judgment: it converges the two
  routes at the cost of the property that made the divergence visible — with no
  output root configured, containment silently measures against a root the
  operator never nominated as a destination, and the resulting diagnostic names
  that root. Convergence is worth having in the direction that keeps the error at
  the configuration mistake.
- **Do nothing and let #616 encode the emergent coupling.** verified: issue #616's
  body proposes validating that `default_staging_root_id` resolves to a root
  contained in `default_output_root_id`'s path, which would freeze the current
  arrangement into a configuration-time contract and leave the two fallback routes
  (`operation_source.rs:185-188` versus `envelope.rs:197-228`) still disagreeing —
  the half-fixed outcome issue #615 names.
- **Settle only the containment question and leave the fallback to a later
  record.** judgment: issue #615's fifth acceptance criterion requires this record
  to settle the fallback, and splitting it out would leave the defect half-fixed in
  exactly the way that criterion names.
