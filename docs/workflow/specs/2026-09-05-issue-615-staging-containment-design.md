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
`CONFIG_INVALID: artifact commit path escaped storage root <id>`
(`crates/voom-control-plane/src/operation_source_test.rs:181` pins the code; the
issue bodies that call it `COMMIT_FAILURE` are wrong).

A second route resolves the same concept with the opposite fallback:
`destination_root` (`crates/voom-control-plane/src/workflow/plan/envelope.rs:197-228`)
is fail-closed. Deciding containment while leaving the two fallbacks divergent
leaves the defect half-fixed, so both are settled in one record.

## What the record decides

1. The pre-promotion address is storage-root-contained, and the containing root is
   the **staging** root — specifically the registered `library_roots` row resolved
   for `DestinationRole::Staging`, not merely the directory prefix the address was
   built under. Those two readings diverge on the transitional coordinator path,
   where `--staging-root` is a raw operator path no `library_roots` row describes;
   the record picks the registered-root reading and makes the unregistered path a
   fail-closed configuration error.
2. ADR 0074's shared-owner-node requirement is the whole of the staging↔output
   relationship. No path nesting between the two roots is required, and
   introducing one is forbidden.
3. The rule #616 enforces at configuration time is same-library and
   same-owner-node agreement between a root and each root it names as a default,
   plus the requirement that a named staging root itself carry a
   `default_output_root_id`, with no filesystem path comparison — plus the two completeness points the
   record states: the owner-assignment path (`assign_library_root_owner_in_tx`)
   can invalidate a pairing without writing a default column and must be checked
   in both directions, and a pairing is undecidable only while an owner is still
   absent, owner assignment being where it becomes decidable.
4. The unconfigured-output-root fallback converges on fail-closed, amending one
   clause of ADR 0055.

The record also names what it does *not* authorize: the resolver change in
`operation_source.rs` that implements decisions 1 and 4 is owned by #625, filed
from this record's consequences, and #623 is sequenced behind it.

## Alternatives

Carried in the record's own `Considered & rejected` section rather than duplicated
here; that is where a later reader looks.

## Scope and exclusions

In scope: `docs/adr/0097-*.md` (new), one new row in `docs/adr/README.md`, and
these two design artifacts.

Out of scope, with owners: the resolver change itself (#625); the
configuration-time validation (#616); the `--staging-root` /
`default_staging_root_id` reconciliation (#618); the chaos-harness layout
reconciliation (#623); retiring the in-tree fixture dependency on the fallback
(the resolver change); anything that extends the transitional control-plane
filesystem-promotion path (#416–#425, forbidden by ADR 0050).

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

Two contracts, both machine-checkable:

1. **Index coupling.** `just check-adr-index` exits 0 — exactly one
   `docs/adr/README.md` row for the new record. This is the arm of `just ci` that
   decides this change, and the pre-commit hook enforces it at commit time, so the
   record and its row cannot land separately.
2. **Citation accuracy.** Every `file:line` the record cites into `crates/`,
   `scripts/`, and `docs/adr/` resolves, at the branch base, to a line containing
   a token drawn from the fact the citing sentence asserts — not merely a token
   that happens to appear on the cited line. Claims the record states without a
   `file:line` are outside this check and remain a reviewer's to verify; that is
   where its residual risk lives.
   That distinction is the contract's substance: a check keyed to an incidental
   token passes while the citation is wrong, which is how the first draft's table
   confirmed `chaos-e2e-local.sh:49` for a claim living on line 166. A record whose
   evidence is its whole argument is worth nothing if a citation is misread — one
   was, on the first draft, and it inverted a Consequences bullet. The plan carries
   the exact asserting command and a row for every location the record cites.

`just ci` green overall.
