# Apply ADR 0097 containment in the commit/promotion target resolver

Issue: #625. Scope: q625-5d20d907. Lane: full-spec, M (250 lines).
Plan: `docs/workflow/plans/2026-10-01-resolver-staging-containment.md`.
Governing decision: [ADR 0097](../../adr/0097-pre-promotion-addresses-contained-by-their-own-root.md).

## Problem

`resolve_artifact_target` (`crates/voom-control-plane/src/operation_source.rs`) serves two
callers with one rule: commit (`artifact/commit/prepare.rs`, the pre-promotion
`<staging>/.committed/<op>/…` address) and coordinator promotion
(`workflow/coordinator/promotion.rs`, the durable output address). Both resolve the target root
as `default_output_root_id.unwrap_or(<passed root>)`. ADR 0097 decides that the commit address is
contained by the registered root `destination_root` resolves for `DestinationRole::Staging`, that
a durable output address is contained by the passed root's own `default_output_root_id`, and that
the source-root fallback is removed (fail closed).

Reading adopted: ADR 0097's Decision cites `prepare.rs` beside the output-default route, but its
title, Context, first decision, the #615 spec, and the promotion clause ("the artifact's own root
… is the staging root") only hold if commit records the location under the staging root. Commit
therefore resolves the staging root; promotion resolves the output default of the artifact's
root (the staging root after commit). Recorded in `WORK:SCOPE` on #625.

## Decision

1. **Two entry points replace `resolve_artifact_target`.** Both return the existing
   `(StorageRootId, ProviderRelativeLocator, PathBuf)` triple.
   - `resolve_pre_promotion_target(cp, label, source_root_id, path)` — containment root is
     `destination_root(cp, DestinationRole::Staging, source_root_id)`. Called by `prepare.rs`.
   - `resolve_output_target(cp, label, root_id, path)` — containment root is `root_id`'s own
     `default_output_root_id`; `None` fails with `VoomError::Config`:
     `<label>: storage root <id> has no default output root; configure one with
     `voom library root update --root-id <id> --output-root <id>``. No leaf lookup and no
     fallback to `root_id`. Called by `promotion.rs`.
2. **Shared tail unchanged in substance.** Both run the existing checks against the resolved
   root: it exists, belongs to the naming root's library (else `VoomError::Database`, message now
   naming the role), is available and owned by the local node, and the canonical target is
   contained by it (`require_contained`, unchanged).
3. **`destination_root`'s unconfigured message becomes actionable and context-free**:
   `no default <role> root configured for storage root <id>; configure one with
   `voom library root update --root-id <id> --<role>-root <id>``. It is no longer
   envelope-only, so the `media dispatch envelope:` prefix goes. Error kind stays `Config`.
4. **`DestinationRole::Output` loses its `expect(dead_code)`** — it is now used in non-test code
   (role naming in the library-mismatch message).
5. **Fixtures.** Each test root that reaches a commit names a staging default, and each that
   reaches a promotion names an output default. A new helper
   `voom_store::test_support::set_test_storage_root_self_defaults(pool)` makes the shared test
   root (9000001) its own staging and output default — a configuration ADR 0097's #616 rule
   accepts. Measured blast radius before fixtures: 60 failing tests in 9 suites (plan file map).
   `crates/voom-cli/tests/support/voom_cli.rs` `configure_local_root` adds `default_output_root_id
   = id` beside its staging/backup defaults so the chaos harness keeps working unchanged.

Rejected: routing promotion through `destination_root(Output, …)` — its leaf lookup is a
fallback ADR 0097 forbids ("must carry its own output default"); one entry point with a role
parameter — the two routes share no resolution logic, only the tail; making the shared seed root
self-defaulting globally — executor tests document it "ships with no default destinations" and
opt in, so the change would silently alter unrelated envelope tests.

## Failure model

- **Actors and deployments:** a local operator running `voom artifact commit` or
  `voom compliance execute`; the owner-node commit flow; the coordinator's transitional
  promotion path (ADR 0050, not extended).
- **Invariants at stake:** a committed location is recorded under the root that contains it;
  no target resolves to a root the operator did not configure; every resolution failure happens
  before any durable mutation (prepare's pre-mutation error path, promotion before the move).
- **Accepted:** deployments relying on the implicit source-root fallback now fail closed until
  they set `--staging-root`/`--output-root` (pre-release, no migration owed — ADR 0097). A staging
  root without an output default still fails at promotion, after a transcode (bounded: one
  failed promotion with an actionable message; config-time prevention is #616). A
  `voom artifact commit --target` outside the staging root is rejected even when it lies in the
  output root — the decision itself.
- **Covered elsewhere:** config-time pairing validation (#616); `--staging-root` flag
  reconciliation (#618); chaos harness layout (#623); #484 criterion wording (#484); retirement
  of a referenced root (#626, merged).

## Success

1. Commit resolves its containment root via `destination_root(Staging, source)`; a target
   under the staging root resolves to the staging root id; a target under the output root but
   outside the staging root is rejected `CONFIG_INVALID` "path escaped storage root".
2. Commit with no staging default fails `CONFIG_INVALID` naming the root and `--staging-root`.
3. Promotion/output resolution with no output default fails `CONFIG_INVALID` naming the root and
   `voom library root update --root-id <id> --output-root <id>`, even when the target lies inside
   the root itself (no fallback).
4. Existing cross-library and escape checks keep their behavior on the new entry points.
5. The 9 suites listed in the plan pass; `just ci` exits 0.

## Validation

Focused tests in `crates/voom-control-plane/src/operation_source_test.rs` cover Success 1–4
(each observed red against the pre-change resolver or a deliberate fault); Success 5 is the
suite run. `destination_root` message: covered by Success 2's assertion.
