# ADR 0015 — Decompose oversized control-plane modules along cohesion seams

- Status: Accepted
- Date: 2026-06-11
- Issue: #230 ([audit L10] Decompose 1,000+ line voom-control-plane modules)
- Related: ADR-0004 (sibling unit tests), ADR-0007 (phase-barrier coordinator),
  ADR-0012 (paused-time DB guard), ADR-0013 (payload evolution contract)

## Context

Four `voom-control-plane` modules exceed 1,000 lines and concentrate several
distinct responsibilities in one file:
`cases/execution/remote_execution.rs` (2255), `workflow/coordinator.rs` (1896),
`workflow/execution/executor.rs` (1438), `artifact/commit.rs` (1140). The Fable
audit (#230) flags them as the primary drag on auditability and per-unit test
focus. The decomposition is a pure refactor: behavior, public API, schema, and
dependencies are unchanged.

Three module-organization decisions have viable alternatives and are settled
here.

## Decision

### 1. File-per-responsibility directory modules, not flat files or one mega-module

Each oversized `foo.rs` becomes a directory module `foo/` with `foo/mod.rs`
holding the module's public surface and shared glue, and child files
(`foo/acquire.rs`, `foo/complete.rs`, ...) each owning one responsibility from
the cohesion map. Items move **verbatim**: bodies, signatures, SQL, event
names, and error codes are untouched.

This matches the established directory-module convention already used by
`transcode/`, `scan/`, `remux/`, and `audio/` in the same crate
(`mod.rs` + `mod_test.rs` + per-responsibility children).

### 2. Children are referenced by qualified path; `mod.rs` owns shared glue and DTOs

`mod.rs` declares children with `mod child;` / `pub(crate) mod child;` and keeps:

- the module's public DTOs and error types (so the `pub`/`pub(crate)`
  re-exports in `artifact/mod.rs`, `workflow/mod.rs`,
  `workflow/execution/mod.rs` keep resolving the same names);
- the public `ControlPlane` entry points that are the module's API contract;
- the genuinely shared private helpers two or more children call (replay/
  idempotency glue, conversion helpers, shared traits).

Children reference each other and `mod.rs` items by **qualified path**
(`acquire::score_remote_candidates`, `super::ReplayRoute`), not `use child::*;`
glob re-exports. Cross-child private items get the minimum visibility that
compiles — `pub(super)` for one-module-wide use, never wider than `pub(crate)`.
This keeps the pedantic `wildcard_imports` lint quiet and the symbol provenance
explicit, matching `transcode/mod.rs`.

### 3. Tests follow their items into per-child `*_test.rs`; cross-cutting tests stay in `mod_test.rs`

The sibling-test convention (ADR-0004, `scripts/check-test-layout.sh`) pairs
each `X_test.rs` with `X.rs` carrying a `#[path]` decl, and forbids inline
`#[cfg(test)] mod tests`. A directory module therefore uses `foo/mod.rs` +
`foo/mod_test.rs`, and a child with unit tests uses `foo/child.rs` +
`foo/child_test.rs`.

The existing monolithic `foo_test.rs` is split so each test moves to the
`*_test.rs` beside the child that owns the items it exercises; its `use super::*;`
then resolves against that child. Tests that drive the module through its public
entry point (and so legitimately exercise several children) stay in
`foo/mod_test.rs` and resolve against `mod.rs`. Where such a kept test reaches a
child's private item, `mod.rs` adds a narrow named `pub(crate) use child::item;`
re-export — never a glob. No test is dropped, merged away, or un-gated; the
count of `#[test]`/`#[tokio::test]` functions is preserved.

The paused-time guard (ADR-0012) is re-checked per split: a `tokio::time::pause`
test is never co-located with a `ControlPlane`/`SqlitePool` test in the same
file.

## Consequences

- Each child file holds one auditable responsibility; a reviewer reads only the
  acquire path without scrolling past replay/heartbeat/complete/fail/recover.
- `impl ControlPlane` is split across child files. Rust allows inherent-impl
  methods for one type across modules of the same crate, so methods move without
  becoming public — this is what makes the split behavior-preserving.
- The public surface is unchanged: no caller outside a split module is edited.
  Verified by the unchanged re-export lists and a clean cross-crate build.
- History stays bisectable: each module is decomposed in small, independently
  `just ci`-green commits (move a cluster + its tests, wire `mod.rs`, verify),
  not one mega-commit.
- More files and more `#[path]` decls to maintain, accepted as the cost of the
  per-responsibility layout the rest of the crate already follows.

## Considered & rejected

- **Leave the files as-is.** Rejected: the audit identifies these modules as the
  top maintainability lever; the cost is only growing.
- **Split into flat sibling files (`remote_execution_acquire.rs`) instead of a
  directory module.** Rejected: the crate's convention is directory modules
  (`transcode/`, `scan/`), and flat siblings pollute the parent namespace and
  read worse.
- **Keep one monolithic `mod_test.rs` and re-export every private item via
  `use child::*;` in `mod.rs`.** Rejected: trips the pedantic `wildcard_imports`
  lint, hides symbol provenance, and leaves the largest test files (3,114-line
  `executor_test.rs`) undecomposed — defeating half the auditability goal.
- **Change visibilities to `pub`/`pub(crate)` broadly to make moves trivial.**
  Rejected: widening visibility is a public-surface change; we use the narrowest
  visibility that compiles (`pub(super)`).
- **Rewrite or simplify logic while moving it.** Rejected: out of scope and
  defeats verbatim-move verification. Behavior must not change (AGENTS.md Rule 4,
  global "surgical changes").
- **Move artifact/promotion helpers into `voom-artifact`.** Rejected: the
  crate-layering invariant keeps filesystem promotion, worker dispatch, and
  use-case assembly in `voom-control-plane`; only narrow store-facing helpers
  live in `voom-artifact`, and this issue introduces none.
