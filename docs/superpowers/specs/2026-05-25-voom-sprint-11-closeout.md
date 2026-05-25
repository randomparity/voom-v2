# Sprint 11 Closeout: Staged Artifact Commit

## Scope

Sprint 11 delivered the local staged artifact flow: schema, durable store
repositories, lifecycle events, typed verify-artifact worker protocol, bundled
verification worker, control-plane stage/verify/commit/inspect use cases, CLI
commands, integration snapshots, and closeout verification.

The architectural design document already names Sprint 11's staged artifact
commit and verification-worker goals, and no command-contract cross-reference
needed a same-branch update.

## Acceptance Evidence

| Area | Evidence | Verification |
|---|---|---|
| Schema | `f2b4079 feat(store): add staged artifact commit schema` added artifact handle identity links, verification rows, commit rows, and migration inventory coverage. | Commit hook passed; later `just ci` passed including migration inventory tests. |
| Store repositories | `ee1237c feat(store): add staged artifact repositories` added durable stage, verification, and commit record operations with conflict and validation coverage. | Commit hook passed; later `just ci` passed. |
| Events | `f6468c7 feat(events): add staged artifact lifecycle events` added staged, verification, and commit lifecycle event payloads. | Commit hook passed; later `just ci` passed. |
| Worker protocol | `942de3d feat(worker): add verify artifact worker` added the out-of-process verify-artifact worker and no-follow file observation. | Commit hook passed; later `just ci` passed. |
| Control-plane filesystem helpers | `4ec5ee2 feat(control-plane): add artifact filesystem helpers` added regular-file observation, checked copy, and add-only promotion helpers. | Commit hook passed; later `just ci` passed. |
| Stage-copy use case | `62bc0ff feat(control-plane): stage artifact copies` added source-to-staging copy, cleanup behavior, structured command errors, and tests. | Commit hook passed; later `just ci` passed. |
| Verification use case | `f4b7c20 feat(control-plane): verify staged artifacts` added bootstrap of the built-in verify worker and durable verification persistence. | Commit hook passed; later `just ci` passed. |
| Commit and recovery | `88a0d90 feat(control-plane): commit verified staged artifacts` added add-only commit, pre-mutation rejection, recovery-required reporting, and cleanup coverage. | Commit hook passed; later `just ci` passed. |
| Inspection | `b9c432d feat(control-plane): inspect staged artifacts` added list/show read models and recovery filesystem observations. | `cargo test -p voom-control-plane artifact::inspect` passed with 9 tests; later `just ci` passed. |
| CLI commands | `453d02c feat(cli): add artifact commands` added `voom artifact stage-copy`, `verify`, `commit`, `list`, and `show`. | `cargo test -p voom-cli commands::artifact` passed with 11 tests; later `just ci` passed. |
| CLI snapshots and integration | `bb5b752 test: cover staged artifact CLI flow` added artifact envelope snapshots and control-plane flow integration coverage. | `cargo test -p voom-cli --test artifact_envelope` passed with 3 tests; `cargo test -p voom-control-plane --test staged_artifact_flow` passed with 2 tests. |
| Forbidden-marker scan | Ran `rg -n -e 'TO''DO' -e 'TB''D' -e 'place''holder' -e 'fake trans''code' -e 'in-process ver''ify' docs crates migrations`. | Non-empty output was reviewed. Hits are historical specs/plans, existing Sprint 8/9 synthetic access-mode names, SQL parameter-variable text, or the Sprint 11 design sentence saying the CLI path is not a fake media operation. No new Sprint 11 implementation code introduced unresolved markers. |
| Full CI | Ran `just ci`. | Passed: fmt-check, clippy, test, doc, deny, and audit all completed successfully. |

## Review Record

Each implementation task from Task 10 onward was reviewed by independent
subagents for spec compliance and code quality before moving on:

- Task 10 inspection: spec review found failed verification state drift; fixed in
  `b9c432d`, then spec and quality reviews approved.
- Task 11 CLI commands: spec and quality reviews approved `453d02c`.
- Task 12 integration snapshots: spec and quality reviews approved `bb5b752`.

Earlier Task 1 through Task 9 commits were likewise completed behind targeted
tests and review gates before this closeout step.

## Residual Notes

- Recovery-required inspection integration uses direct durable row setup where
  production failpoints are intentionally not exposed through the public API.
  Production recovery-required CLI output is also exercised through a real
  commit failure path in `artifact_failure_envelopes_are_actionable`.
- The forbidden-marker scan is intentionally broader than Sprint 11. Existing
  historic docs and synthetic access-mode identifiers remain in scope for the
  scan output but are not Sprint 11 regressions.
