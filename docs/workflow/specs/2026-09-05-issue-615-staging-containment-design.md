# Issue #615 — where the pre-promotion `.committed` address is contained

Design record for the decision issue #615. The deliverable is one Architecture
Decision Record, `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md`,
plus its row in `docs/adr/README.md`. No behavior ships from this change.

## Problem

`ControlPlane::commit_artifact` validates the intermediate
`<staging-root>/.committed/<op>/…` address against a target root resolved from the
source root's `default_output_root_id`, falling back silently to the source root
(`crates/voom-control-plane/src/operation_source.rs:175-205`), then requires
containment (`operation_source.rs:290-302`). The emergent effect is that the
staging root's path must be nested inside the output root's path — a coupling
stronger than ADR 0074's shared-owner-node requirement and stated in no record.
It surfaces only after a transcode, as
`COMMIT_FAILURE: artifact commit path escaped storage root <id>`.

A second route resolves the same concept with the opposite fallback:
`destination_root` (`crates/voom-control-plane/src/workflow/plan/envelope.rs:197-228`)
is fail-closed. Deciding containment while leaving the two fallbacks divergent
leaves the defect half-fixed, so both are settled in one record.

## What the record decides

1. The pre-promotion address is storage-root-contained, and the containing root is
   the **staging** root — the root the address was built under — not the output
   root. Containment is a property of an address inside its own addressing domain,
   never a property of two roots' locators relative to each other.
2. ADR 0074's shared-owner-node requirement is the whole of the staging↔output
   relationship. No path nesting between the two roots is required, and
   introducing one is forbidden.
3. The rule #616 enforces at configuration time is same-library and
   same-owner-node agreement between a root and each root it names as a default,
   with no filesystem path comparison — plus the two completeness points the
   record states (the third writer, `assign_library_root_owner_in_tx`, and the
   null-owner case ADR 0055's migration left behind).
4. The unconfigured-output-root fallback converges on fail-closed, amending one
   clause of ADR 0055.

## Alternatives

Carried in the record's own `Considered & rejected` section rather than duplicated
here; that is where a later reader looks.

## Scope and exclusions

In scope: `docs/adr/0097-*.md` (new), one new row in `docs/adr/README.md`, and
these two design artifacts.

Out of scope, with owners: the resolver change itself and the configuration-time
validation (#616); the `--staging-root` / `default_staging_root_id` reconciliation
(#618); the chaos-harness layout reconciliation (#623); retiring the in-tree
workaround (#497's closure); anything that extends the transitional
control-plane filesystem-promotion path (#416–#425, forbidden by ADR 0050).

No Rust source, migration, or `justfile` change. `operation_source.rs` and
`operation_source_test.rs` are owned by parallel issue #617 and are read here, not
modified.

## Not applicable

No AI surface is added or modified, so no eval plan is owed. The change is not
security-relevant under the `$quest` step 6 triggers judged on intent: it adds no
entry point, touches no authn/authz or tenancy logic, handles no secret, parses no
foreign input, builds no command or query from a non-literal, widens no permission
grant, and changes no dependency or security-relevant default. It adds two
Markdown files and one table row. So no threat model is owed either.

## Verification

`just ci` green, which includes `check-adr-index` — the gate that couples the new
record to exactly one index row.
