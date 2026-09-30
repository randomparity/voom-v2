# Bounded nextest comparison (#639)

## Authority and outcome

[Issue #639](https://github.com/randomparity/voom-v2/issues/639) requests a measured
runner decision, not automatic adoption. The operator approved the 14-run proposal
on 2026-09-30; [scope](https://github.com/randomparity/voom-v2/issues/639#issuecomment-5919525443)
and [approval](https://github.com/randomparity/voom-v2/issues/639#issuecomment-5919525713)
retain its provenance. Base source is `9d7ea8e8582094c214a635dd465f680684c2dbbd`.

## Global Constraints

Use Rust/Cargo 1.95.0, existing full-debug profiles, and nextest 0.9.146.
Measure native macOS arm64 and local Debian GNU/Linux arm64 separately.
Run Linux without root/effective capabilities, with native on-disk `.test-tmp` storage.
Keep default control-plane and all-feature workspace passes, ignored-test policy,
separate Cargo doctests, prebuilt workers, and serial coverage.
Limit measurement to 14 logical full-suite invocations and 120 minutes of suite execution.
No failed-run replacements, retries, production repairs, profile changes or concurrent suites.

## Ownership and alternatives

The existing justfile owns local test orchestration and CI calls it. No production
ownership moves. Use temporary experiment configuration first; publish a report either
way. Change runner recipes/CI/config only if the approved gate passes. In that case
record ADR 0100 and its coupled index row, retain the two feature passes, invoke
`cargo test --doc` for each feature selection, and leave coverage/hook recipes intact.
A no-go adds only this specification and the measurement report.

The selected matrix compares existing Cargo parallelism with two bounded nextest
limits. A 4-only matrix would be cheaper but was not selected. Unbounded tuning is
outside the approved experiment. Process-local caches will be reinitialized per test;
that overhead belongs in measured execution, not in a claimed theoretical speedup.

## Schedule and decision

For each OS, screen Cargo with explicit 18 test threads, nextest with 4, then nextest
with 8. Freeze one common nextest limit maximizing the smaller of the two platform
screening execution improvements; ties select 4. Then run N/C/C/N on each OS, using
that frozen limit. This gives three Cargo and three selected-nextest observations
plus one unselected-nextest observation per OS: 14 total. Screening observations count.
Run one complete invocation at a time; retain failures and stop on a prerequisite,
coverage or integrity failure. A test failure disqualifies adoption; diagnose it before
any further dependent work, with no replacement sample or excluded repair.

A logical invocation consists of completed all-feature/all-target worker prebuild,
default control-plane binary tests and doctests, then all-feature workspace binary tests
and doctests. Cargo uses `--lib --bins --tests` for binary tests and `--doc` separately;
nextest runs the same binary selection with retries zero. Normal ignored tests remain
ignored. Opt-in stress, scale, chaos, hardware and network cases are inventoried, not enabled.

Measure compilation separately: each OS/runner gets an initially empty target directory,
registry downloads excluded, with prebuild and both no-run feature passes timed. Repeat
that compilation sequence once warm. Cargo and nextest use matching source/toolchain/
profiles/features; record artifacts and flags. Full suite invocations use warm targets.
No-run/listing commands do not execute suites or consume the 14-run budget.

For full invocations record prebuild/build overhead, binary execution, doctest duration
and total wall time. Cargo runs its actual test commands, preserving
its environment, dynamic-library setup and per-package cwd. Sum Cargo's per-binary elapsed
output for binary execution, disclosing its two-decimal precision; retain separately timed
no-run compilation and Cargo's build elapsed output. Nextest execution uses
its completed run duration. Warm total is direct measured full orchestration, not a sum
of unrelated cold samples. Instrumentation overhead is disclosed equally.

Go requires successful complete observations and exact inventory/integrity reconciliation,
no retries, at least 10% median execution and 5% median warm-total improvement on each OS,
and at most 5% extra cold-build cost on each OS. Compare medians of the three observations
for Cargo and the selected nextest setting. Cold cost compares the complete cold compilation
sequences. Any unmet gate is a measured no-go; no additional favorable samples are allowed.
Small sample sizes and non-isolated hosts do not establish confidence intervals or universal
speed claims. Hosted Ubuntu/macOS compatibility is separate from local Linux evidence.

## Resource policy and evidence

Nextest's process-per-test model loses the mutex shared by four tests in
`workflow/durable_workflow_test.rs`. Assign that module a group with maximum concurrency 1.
The `commit_use_lease_gate`, `recover_commit_gate`, and `staged_artifact_flow` integration
binaries mutate the shared ffprobe sibling: reserve all configured test slots for them.
Other control-plane and CLI tests reserve two slots each. The specific overrides precede
the general override. Existing cross-process filesystem locks remain unchanged.
Conformance's fixed output paths are used within one sequential test; suites cannot overlap.
An unresolved collision or global-state assumption blocks adoption, not a test exclusion.

List tests by feature pass, package/target identity and exact name, retaining ignored status.
Compare Cargo harness lists with nextest JSON lists before runs; reconcile each run's passed,
failed and ignored occurrences, not just totals. Separately list/execute Cargo doctests.
Platform-only differences are explained from source. Check real-media prerequisites before
measurement and retain successful test output so runtime skip messages cannot masquerade as
coverage. Hash the 23 prebuilt executable workers after prebuild and after each test phase;
bytes must remain unchanged. Confirm actual database paths with the existing pinned-temp test
and filesystem type with the platform tool. Remove a name/change a worker in owned evidence
copies to prove the reconciliation and integrity checkers fail.

## Failure model

- Actors and deployments: local operator running one experiment; non-root local macOS and
  Debian arm64; hosted Ubuntu/macOS release checks if adopting. No simultaneous suite actors.
- Invariants and assets: the named two-pass test/ignored/doctest inventory; unchanged worker
  bytes; native pinned database storage; bounded resources; truthful failures and timings.
- Accepted failure classes: timing uncertainty beyond these observations (report ranges,
  no statistical guarantee); unchanged opt-in tests remain ignored and explicitly enumerated.
- Covered elsewhere: hooks #485; wider flakes #520; ffprobe lock redesign #483; profiles #638;
  guards #637/#640; sharding #641; production changes require separate authority. Existing
  CLI worktree Git watches and conformance output-isolation findings remain campaign-owned.

## Threat model

- Boundaries added: downloaded nextest executable; temporary local command/log collector.
  Existing CI dependency surface changes only after go; no application boundary is widened.
- Actors: trust the authenticated operator and upstream nextest release; archive/log bytes
  still require validation. Fork PRs are not granted credentials.
- Controls: exact 0.9.146 official release archives with published SHA-256 verification;
  isolated tool directory; argv execution without eval; bounded run count/time; private raw
  logs; public report redacts identifying paths. Existing Cargo.lock and profiles stay fixed.
- Outside scope: compromised compiler/OS/upstream signing authority and hostile concurrent
  local users; this controlled benchmark neither introduces nor claims to solve those threats.
