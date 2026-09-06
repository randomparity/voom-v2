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
`.committed/<op>` onto the operator's `voom compliance execute --staging-root`
path. Both are staging addresses; only the later promotion into `--output-dir`
(`crates/voom-control-plane/src/workflow/coordinator/promotion.rs:723-760`)
produces a durable output address.

So the guard measures a staging address against the output root, and the two
roots have no stated path relationship. ADR 0074 requires only that the staging
root and the target root resolve to the same owner node — "one node must both
read the staging bytes and promote into the target" — and fails pre-mutation
otherwise. The stronger property the guard actually demands, that the staging
root's path be *nested inside* the output root's path, is written down nowhere.
Nothing enforces it at configuration time; it surfaces as
`COMMIT_FAILURE: artifact commit path escaped storage root <id>` after a transcode
has already run, which is the failure that broke the weekly `chaos-e2e` job
(#470, #491). The workaround in tree is to collapse the roots:
`crates/voom-cli/tests/support/voom_cli.rs:49` sets
`default_staging_root_id = default_backup_root_id = default_output_root_id = id`,
so a registered library root has to contain voom's own `.committed` scratch.

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
is therefore an accepted decision, not an emergent one — the issue's premise that
ADR 0055 does not state it is mistaken. What ADR 0055 does not state, and what
this record settles, is *which* root a pre-promotion address is contained by.

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
location, and the root that contains it is the **staging** root — the one the
address was built under — not the output root. Containment is a property of an
address inside its own addressing domain. It is never a property of two roots'
locators relative to each other.

Concretely, a resolver validating an address resolves its containment root from
the role the address serves: a pre-promotion staging address against the root
resolved for `DestinationRole::Staging`, a durable output address against the root
resolved from `default_output_root_id`. Applying the output root's containment to
a staging address is the defect; the guard is correct, and is applied one step too
early to the wrong root.

### The staging↔output relationship is ADR 0074's shared owner node, and nothing more

ADR 0074's requirement — that the staging root and the target root resolve to the
same owner node — is confirmed as the whole of the relationship. This record adds
no path-nesting requirement between them and forbids one being introduced. A
staging root that is a sibling of, or wholly unrelated to, the output root is a
correct configuration provided both resolve to the same owner node and the same
library, which `artifact_target_root` already requires
(`operation_source.rs:196-202`).

### The rule #616 enforces at configuration time

`voom library root add` and `voom library root update` reject a
`--staging-root <id>`, `--output-root <id>`, or `--backup-root <id>` value on root
R when the named root does not exist, does not belong to R's library, or resolves
to an owner node other than R's. They perform **no** filesystem path comparison
between roots.

Two completeness points bind that rule. First, `default_*_root_id` has a third
writer beyond the two #616 names: `assign_library_root_owner_in_tx`
(`crates/voom-store/src/repo/library/library_roots.rs:361-390`) changes an owner
without touching the default columns, so the owner-agreement check must also run
there, against every root that names the root being reassigned as a default.
Second, ADR 0055 quarantined migrated roots as `unassigned` with a null owner;
a pairing where either side has no assigned owner is accepted at configuration
time and remains the run-time pre-mutation check's to refuse, because a
configuration-time rule cannot decide agreement between an owner and an absence.

### An unaddressable destination fails closed

`artifact_target_root`'s silent fallback to the source root is removed.
When no `default_output_root_id` is configured for the source root, resolving a
durable commit target fails with an actionable error naming the root and the
`voom library root update --output-root <id>` that fixes it. This converges both
routes on `destination_root`'s fail-closed semantics, for the reason
`destination_root` already gives: an unaddressable destination must fail rather
than be guessed.

This **amends ADR 0055** in one clause only — "falling back to the same root" no
longer holds. ADR 0055's root identity model, its provider-relative location
model, and its containment requirement are untouched and still govern.

## Consequences

- The chaos-e2e workaround is retired in principle: a registered library root no
  longer has to contain voom's own `.committed` scratch. Retiring it in tree is
  #497's closure, not this record.
- #616 is unblocked to implement the same-library, same-owner-node configuration
  check above, at `create_library_root`, `update_library_root`, and
  `assign_library_root_owner_in_tx`. Its premise changes: it must **not**
  implement the path-containment check its body describes, and the filesystem
  canonicalization question that made it look migration-adjacent disappears with
  it. Its open question about pre-existing rows narrows to rows whose staging and
  output roots have different assigned owners.
- #618 is unblocked to reconcile `--staging-root` with `default_staging_root_id`
  by making the durable column the single source of truth, since this record
  makes the staging root the address's containment domain and a flag carrying a
  path that disagrees with it can no longer be correct. That reconciliation stays
  inside ADR 0050's constraint: no new control-plane-owned filesystem behavior,
  and `promotion_plan()`'s surface holds or shrinks.
- #623's question is answered: the two chaos harnesses should both use a staging
  root that is **not** nested inside the library root, which is
  `scripts/chaos-e2e-local.sh`'s current arrangement. The Rust harness's nesting
  at `crates/voom-cli/tests/chaos_librarian_e2e.rs:247-266`, and its comment
  claiming a staging root outside the storage root makes the commit path escape
  it, both encode the behavior this record rejects. #623 owns that reconciliation
  and must not run before the resolver change lands, or the harness will assert a
  layout the code still rejects.
- Removing the source-root fallback is a behavior change for any deployment that
  relied on the implicit "write beside the source" default; it must now set
  `--output-root` explicitly. The project is pre-release, so no migration or
  deprecation window is owed.
- Nothing in this record ships behavior. Until the resolver change lands, the
  emergent nesting requirement still binds at run time, and the in-tree workaround
  is still load-bearing.

## Considered & rejected

- **Keep the guard where it is and make staging↔output path nesting a stated
  contract.** verified: it makes a registered library root contain voom's
  `.committed` scratch, which is the in-tree workaround at
  `crates/voom-cli/tests/support/voom_cli.rs:49` and the arrangement the Rust
  chaos harness encodes at `chaos_librarian_e2e.rs:247-266`; ADR 0055 makes a root
  an addressing domain with its own epoch and retirement lifecycle, so nesting one
  inside another gives every scratch byte two containing domains and no rule for
  which epoch fences it.
- **Drop the containment check on the pre-promotion address entirely, treating
  `.committed` as rootless scratch.** verified: ADR 0055's migration 0034
  quarantined exactly the rootless case as ineligible for work, and ADR 0075
  carries only `(StorageRootId, ProviderRelativeLocator)` pairs across the
  control-plane↔agent boundary, so a dispatched staging destination has no
  rootless representation to express.
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
  contained in `default_output_root_id`'s path, which would freeze the workaround
  into a configuration-time contract and leave the two fallback routes
  (`operation_source.rs:185-188` versus `envelope.rs:197-228`) still disagreeing —
  the half-fixed outcome issue #615 names.
- **Settle only the containment question and leave the fallback to a later
  record.** judgment: the containment root for a durable output address *is* the
  fallback's result, so a rule stated without it is a rule #616 cannot implement
  without a second decision, which is the acceptance criterion this record owes.
