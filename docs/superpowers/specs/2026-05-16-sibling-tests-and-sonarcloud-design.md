# Sibling unit tests and SonarCloud coverage — design

**Date:** 2026-05-16
**Status:** Approved (design); implementation pending
**Branch:** `sibling-tests-and-sonarcloud`

## Summary

Adopt a project convention that Rust unit tests live in a sibling file
(`<source>_test.rs`) rather than in an inline `#[cfg(test)] mod tests { ... }`
block. Refactor the ten existing source files that contain inline tests. Add
`cargo-llvm-cov`-based coverage and wire the lcov report into a SonarCloud
analysis job in CI.

## Motivation

- **SonarQube/SonarCloud duplication noise.** Inline `mod tests { ... }`
  blocks repeat boilerplate (assertion shapes, fixture setup, env-var
  toggling) that doesn't justify extracting helpers, but does trip
  duplication detectors. Splitting tests into a sibling file lets the
  duplication scanner ignore the `*_test.rs` files via
  `sonar.test.inclusions` while still flagging real duplication in
  production code.
- **File-size pressure.** Source files grow as tests grow. A sibling file
  caps the growth of each `*.rs` file at the size of its production code,
  delaying or removing the need to split modules that are otherwise
  cohesive.
- **Related-content locality preserved.** The sibling file sits next to its
  source. `git log --follow`, IDE go-to-definition, and `super::*` all
  continue to work because the test file is still a child module of the
  source — just declared via `#[path]`.

## Non-goals

- Touching integration tests in `crates/*/tests/`. Those are already
  separate files and follow Rust's standard layout.
- Touching `crates/voom-store/src/test_support.rs`. That is a feature-gated
  helper module compiled into the library, not a test file.
- Replacing any test framework. `cargo test` remains the runner; `insta`
  snapshots stay where they are.
- Coverage thresholds or quality gates beyond what SonarCloud applies by
  default. (Setting thresholds is a follow-up once we have a baseline.)

## Design

### 1. Sibling-file convention

For every Rust source file under `crates/*/src/` that contains unit tests,
tests move to a sibling file named `<source>_test.rs`, linked from the
source via the `#[path]` attribute:

```rust
// At the bottom of crates/voom-core/src/version.rs
#[cfg(test)]
#[path = "version_test.rs"]
mod tests;
```

```rust
// crates/voom-core/src/version_test.rs
use super::*;

#[test]
fn parses_semver() { /* ... */ }
```

**Why `#[path]`.** Rust's module resolver looks for `tests.rs` or
`tests/mod.rs` when it sees `mod tests;`. The `#[path]` attribute is the
only mechanism that keeps the test module as a child of the source file
(so `super::*` reaches private items) while putting its code in an
arbitrarily named sibling file. There is no idiomatic alternative.

**Naming.** Singular `_test`, matching the example the user specified:
`lib.rs` → `lib_test.rs`, `version.rs` → `version_test.rs`. The naming is
mechanical; no per-file judgment required.

### 2. Refactor scope

Ten source files currently contain inline `#[cfg(test)] mod tests { ... }`
blocks. Each gets refactored to the sibling pattern:

| Crate | Source file | New sibling file |
|---|---|---|
| voom-core | `clock.rs` | `clock_test.rs` |
| voom-core | `config.rs` | `config_test.rs` |
| voom-core | `error.rs` | `error_test.rs` |
| voom-core | `ids.rs` | `ids_test.rs` |
| voom-core | `version.rs` | `version_test.rs` |
| voom-store | `init.rs` | `init_test.rs` |
| voom-store | `pool.rs` | `pool_test.rs` |
| voom-store | `schema.rs` | `schema_test.rs` |
| voom-control-plane | `lib.rs` | `lib_test.rs` |
| voom-cli | `envelope.rs` | `envelope_test.rs` |

**Mechanics, per file:**

1. Copy the body of the inline `mod tests { ... }` block (the inside,
   without the wrapping declaration and braces) into the new
   `<source>_test.rs` file.
2. If the inline block declared `use super::*;` inside, it stays as-is —
   the new file is still a child module, so `super` still resolves to the
   parent source module.
3. Replace the inline block in the source file with the three-line
   `#[cfg(test)] #[path = "<source>_test.rs"] mod tests;` declaration.
4. Run `cargo test -p <crate>` to confirm the move was clean.

**Order:** crate by crate (`voom-core`, then `voom-store`, then
`voom-control-plane`, then `voom-cli`), one commit per crate, so any
breakage is bisected to a single crate.

### 3. Enforcement check

A `just check-test-layout` recipe fails CI on two failure modes:

1. **Inline tests in `src/`.** Any file under `crates/*/src/` containing
   an inline `mod tests {` block or any other `#[cfg(test)]`-gated
   inline module body.
2. **Orphaned `*_test.rs` files.** Any `crates/*/src/**/*_test.rs`
   without a matching `#[path = "<x>_test.rs"]` declaration in its
   sibling source file. This catches the silent-skip failure mode where
   a test file exists but is never compiled because its `mod tests;`
   declaration was never added.

The check is implemented as a ripgrep invocation — purely structural,
no Rust parsing required. A custom clippy lint would be the
heaviest-weight alternative and is rejected as premature: a small
script captures the same intent.

The recipe is added to the `ci:` dependency list in the `justfile` and
runs in the GitHub Actions `ci` workflow alongside `fmt-check`, `lint`,
and `test`.

Allowed exceptions: none initially. An allowlist file is a YAGNI
follow-up.

### 4. Coverage tooling

**Tool:** `cargo-llvm-cov`. It uses the same LLVM source-based
instrumentation that `rustc -Z coverage` produces, emits lcov natively,
and runs on all three host platforms. Tarpaulin is rejected as
Linux-only and in maintenance mode.

**Local recipes (added to `justfile`):**

```just
coverage:
    cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info

coverage-html:
    cargo llvm-cov --workspace --all-features --html
```

**Install path:**

- Local: `cargo install --locked cargo-llvm-cov` added to `just setup`.
- CI: `taiki-e/install-action` pinned to a SHA, installing the prebuilt
  binary (seconds, not minutes).

**`.gitignore` additions:** `lcov.info`, `target/llvm-cov/`.

**Coverage semantics:** the `#[path]`-linked test files still execute the
same source code, so the refactor does not change coverage numbers.
"Test code" classification for SonarCloud happens via the
`sonar.test.inclusions` glob in section 5, independent of coverage.

### 5. SonarCloud wiring

**`sonar-project.properties` at repo root:**

```properties
sonar.organization=<org-slug>
sonar.projectKey=<project-key>

sonar.sources=crates
sonar.tests=crates
sonar.inclusions=crates/**/src/**/*.rs
sonar.test.inclusions=crates/**/src/**/*_test.rs,crates/**/tests/**/*.rs

sonar.rust.lcov.reportPaths=lcov.info

sonar.exclusions=target/**,**/target/**
```

The `org-slug` and `project-key` are placeholders filled in during
SonarCloud project creation (a manual prerequisite — see section 7).

**Note on the coverage report key.** SonarCloud's Rust support has
evolved; the exact property name (`sonar.rust.lcov.reportPaths` vs. a
generic coverage key) needs to be confirmed against current SonarCloud
docs at implementation time. This is configuration, not a load-bearing
design choice.

**New `coverage` job in `.github/workflows/ci.yml`:**

```yaml
coverage:
  name: coverage
  runs-on: ubuntu-latest
  # Skip on fork PRs: secrets are not available to forks.
  if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository
  steps:
    - uses: actions/checkout@<sha>  # vX.Y.Z
      with:
        persist-credentials: false
        fetch-depth: 0  # SonarCloud uses git history for blame.
    - uses: Swatinem/rust-cache@<sha>  # vX.Y.Z
    - uses: taiki-e/install-action@<sha>  # vX.Y.Z
      with: { tool: cargo-llvm-cov }
    - name: Generate lcov
      run: cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info
    - uses: SonarSource/sonarqube-scan-action@<sha>  # vX.Y.Z
      env:
        SONAR_TOKEN: ${{ secrets.SONAR_TOKEN }}
```

All action SHAs pinned with `# vX.Y.Z` comments per project policy;
exact versions resolved at implementation time and verified with
`zizmor` + `actionlint`.

**Fork PR handling.** The `if:` clause skips the coverage job on fork
PRs because GitHub does not expose secrets to fork workflows. The main
test matrix still runs on those PRs, so the signal is not lost; only
coverage is deferred to the post-merge commit. Using
`pull_request_target` to bypass this is rejected as a known security
footgun.

### 6. Documentation updates

- **`CLAUDE.md` (project):** add a short Testing subsection stating the
  sibling-file rule, the `#[path]` snippet, and a one-liner pointing
  at `just check-test-layout`.
- **`docs/adr/0004-sibling-unit-tests.md`:** new ADR recording the
  decision and the alternatives considered (inline tests, integration
  tests only, custom clippy lint, tarpaulin). Sits next to the existing
  ADRs.
- **`justfile`:** new `coverage`, `coverage-html`, and
  `check-test-layout` recipes; `check-test-layout` added to the `ci:`
  recipe.

The global `~/.claude/CLAUDE.md` is not modified — this is a project
convention, and project CLAUDE.md already overrides global.

### 7. Manual prerequisites

Before the coverage job can run cleanly:

1. Create the project on sonarcloud.io.
2. Capture the org slug and project key; fill them into
   `sonar-project.properties`.
3. Add `SONAR_TOKEN` to the repository's GitHub Actions secrets.

The implementation plan surfaces these as an explicit pre-merge
checklist so the first CI run after merge does not fail noisily.

### 8. Rollout order

Single PR off `sibling-tests-and-sonarcloud`, staged so every commit
leaves the workspace green:

1. Tooling + docs (no tests moved): `cargo-llvm-cov` install, new
   `just` recipes, ADR 0004, project CLAUDE.md note.
2. Refactor crate by crate: one commit each for `voom-core`,
   `voom-store`, `voom-control-plane`, `voom-cli`. After each:
   `just test` passes.
3. Wire enforcement: `check-test-layout` added to `just ci` and to
   `.github/workflows/ci.yml`. From this point on, drift is impossible.
4. Wire coverage + SonarCloud: add `sonar-project.properties`, add the
   `coverage` job to `ci.yml`. Gated on the section 7 prerequisites
   being completed first.

## Risks and mitigations

- **Forgetting the `#[path]` link.** A developer adds tests to a new
  `foo_test.rs` but forgets to declare `mod tests;` in `foo.rs` — the
  tests silently never run, because `cargo test` never sees them.
  Mitigation: the `check-test-layout` recipe asserts both directions —
  no inline `mod tests {` in `src/`, *and* every `<x>_test.rs` has a
  matching `#[path = "<x>_test.rs"]` declaration in its sibling source
  file. The ADR and project CLAUDE.md note also make the convention
  discoverable.
- **SonarCloud Rust analyzer maturity.** The Rust analyzer is newer
  than the JVM/JS ones. Mitigation: design treats SonarCloud as
  observational (no quality gate yet). If a feature is missing, we
  surface it via SonarCloud's own tooling and adjust the properties
  file.
- **Coverage flakes.** llvm-cov can occasionally drop counters on
  cancelled tests. Mitigation: the coverage job runs the full
  workspace test suite, same as the test matrix; cancellation is
  unlikely. If it happens, the run is re-tried.
- **First-merge CI failure.** If the section 7 prerequisites are not
  completed before merge, the coverage job fails. Mitigation: the
  implementation plan calls out the prerequisites explicitly; the
  coverage job lands in the final commit of the PR so reviewers can
  confirm prerequisites before approving.

## Alternatives considered

- **Inline tests, ignore SonarQube duplication.** Rejected: the original
  motivation is the duplication signal. Suppressing it project-wide
  would also hide real duplication in production code.
- **Move tests into `tests/` integration-only.** Rejected: integration
  tests cannot access private items, which would force exposing more of
  each crate's surface area than necessary.
- **`cargo-tarpaulin` for coverage.** Rejected: Linux-only, in
  maintenance mode, slower instrumentation than llvm-cov.
- **Custom clippy lint for enforcement.** Rejected: orders of magnitude
  more code than the 5-line ripgrep check for the same outcome.
- **Self-hosted SonarQube.** Rejected per user direction: SonarCloud
  SaaS chosen.

## Open items resolved during brainstorming

- Naming: `_test` (singular), matching user example.
- SonarQube target: SonarCloud (SaaS).
- Enforcement: CI check (ripgrep-based `just check-test-layout`).
- CI shape: separate `coverage` job in existing `ci.yml`.

## Next step

Invoke the writing-plans skill to produce a detailed implementation
plan.
