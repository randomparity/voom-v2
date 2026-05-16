---
status: accepted
date: 2026-05-16
deciders: [VOOM core]
---

# 0004 — Sibling unit tests with `cargo-llvm-cov` and SonarCloud

## Context

Unit tests in this workspace lived in inline `#[cfg(test)] mod tests
{ ... }` blocks at the bottom of each source file. This creates two
recurring frictions:

1. SonarQube / SonarCloud's duplication detection flags the shared
   boilerplate inside test blocks (assertion shapes, fixture setup)
   that does not justify extracting helpers but trips automated
   duplication gates. Suppressing the signal globally also hides real
   duplication in production code.
2. Source files grow as tests grow; large files invite refactor
   pressure on otherwise cohesive modules.

We also lack a coverage report; without one we cannot wire SonarCloud
quality gates around the workspace.

## Decision

Unit tests live in a sibling file named `<source>_test.rs`, linked
from the parent source by:

```rust
#[cfg(test)]
#[path = "<source>_test.rs"]
mod tests;
```

The `#[path]` attribute is the only mechanism that keeps the test as
a child module of the source (so `super::*` reaches private items)
while putting its code in a sibling file.

Coverage is produced by `cargo-llvm-cov` (`just coverage` →
`lcov.info`) and consumed by SonarCloud via `sonar.rust.lcov.reportPaths`.

A `just check-test-layout` recipe enforces the convention. The check
uses `ast-grep` so it inspects real Rust syntax tree items rather than
text patterns; it catches both inline tests in `src/` and orphaned
`*_test.rs` files that lack their parent `#[path]` declaration.

## Consequences

- Source files cap their growth at the size of their production code.
- SonarCloud `sonar.test.inclusions` classifies `*_test.rs` and the
  pre-existing `test_support.rs` helper as test code, keeping
  production duplication and coverage gates clean.
- Coverage runs once per CI invocation on `ubuntu-latest` only;
  the cross-platform test matrix is unchanged.
- New unit tests must remember the three-line `#[path]` declaration;
  `check-test-layout` catches the failure mode immediately.

## Alternatives Considered

- **Keep inline tests, ignore SonarQube duplication.** Rejected: would
  suppress real duplication signal in production code.
- **Move tests into `crates/*/tests/` only.** Rejected: integration
  tests cannot access private items.
- **`cargo-tarpaulin` for coverage.** Rejected: Linux-only, in
  maintenance mode, slower instrumentation than llvm-cov.
- **Ripgrep-based layout check.** Rejected after adversarial review:
  text patterns can be fooled by comments, string literals, or
  disabled cfgs, which is exactly the silent-skip failure mode the
  check is meant to prevent. Replaced by `ast-grep`.
- **Custom clippy-driver lint for layout.** Rejected: orders of
  magnitude more code than the `ast-grep` script for the same
  outcome.
- **Self-hosted SonarQube.** Rejected: SonarCloud SaaS chosen.
