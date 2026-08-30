# Constrained-resource scheduled CI design

## Scope and authority

Issue #582 requires a weekly and manually dispatchable Linux lane for deliberate CPU, memory, and
disk variation, the merged distributed stress harness, and the orphaned scan-scale diagnostic.
Parent epic #577 excludes deterministic simulation and performance budgets; #583 owns ENOSPC.
The operator authorized merge-ready delivery using local executable validation because GitHub
accepts `workflow_dispatch` only after the workflow exists on the default branch. Manual-dispatch
evidence and the first scheduled-run findings are therefore explicit post-merge follow-up work.

[ADR 0096](../../adr/0096-run-scheduled-resource-cells-on-isolated-runners.md) governs runner
isolation, cell factoring, and constrained-recipe ownership.

## Goals

1. Run workspace tests in four isolated weekly cells:
   - baseline: four CPUs, 16 GiB, no competing load, no write cap;
   - CPU load: baseline limits plus one competing loop per selected CPU;
   - disk: baseline CPU and memory with a 40 MB/s write cap;
   - memory: four CPUs with an 8 GiB ceiling and no other pressure.
2. Run the distributed stress harness in the baseline and disk cells.
3. Give both ignored `scan_session_scale` diagnostics one stable justfile entry point and execute
   it under baseline constraints in the scheduled lane.
4. Open a tracking issue when a scheduled run fails, while leaving manual failures visible only
   to the operator who dispatched them.
5. Document the lane, local reproduction, post-merge evidence obligations, and limitations.

This is correctness and configuration coverage. It defines no timing threshold, throughput
budget, benchmark comparison, or merge gate.

## Components

### Constrained recipes

`just test-constrained *LIMITS` passes `LIMITS` to `scripts/run-constrained.sh` before the command
separator, then runs the canonical `just test` recipe. That recipe prebuilds worker binaries and
sets `VOOM_TEST_PREBUILT_WORKERS=1` so concurrent tests never relink a binary another test is
executing. The interface accepts only options
already validated by `run-constrained.sh`; it no longer treats trailing values as Cargo test
arguments. Repository search found no caller that relies on the old trailing-argument behavior.

`just stress-constrained *LIMITS` wraps `just stress`. It preserves `just stress` as the owner of
the optional process-worker prebuild and environment contract.

`just scan-session-scale` runs:

```text
cargo test -p voom-control-plane --test scan_session_scale -- --ignored --nocapture
```

That filter selects the two ignored 100,000-row diagnostics and excludes the ordinary fixture
test. `just scan-session-scale-constrained *LIMITS` wraps that fixed recipe with the same resource
runner. None of these recipes joins `just ci`.

### Scheduled workflow

`.github/workflows/constrained-resources.yml` follows the existing `net-resilience.yml` and
`chaos-e2e.yml` pattern:

- `workflow_dispatch` and a weekly schedule;
- repository-wide `contents: read` permission;
- a four-entry Linux matrix with `fail-fast: false` and one fresh runner per cell;
- a 90-minute timeout on every matrix cell;
- pinned checkout, Rust cache, and just setup actions already used by this repository;
- the same ffmpeg and MKVToolNix package provisioning as the existing Linux all-features job;
- a cheap real-wrapper preflight using the cell's CPU, memory, and disk limits with `true` before
  compilation, so hosted cgroup or block-device incompatibility is attributable before the test
  suite starts; the CPU-load cell omits `--load` from this preflight because the tracked cleanup
  defect would otherwise leave one competitor set for the real test to double;
- one constrained workspace-test command in every cell;
- conditional stress steps for baseline and disk;
- one conditional scan-scale step for baseline;
- a dependent scheduled-only notification job with `issues: write`.

The failure issue contains the Actions run URL and names the scheduled lane. It does not attempt
deduplication: the two precedent workflows create one issue for each failed scheduled run, and
this lane follows that established operational contract.

## Data and control flow

GitHub expands the fixed matrix. Each job checks out the same commit and resolves literal
preflight and test resource-argument strings owned by the workflow. The CPU-load preflight string
omits only `--load 1`; its test string retains it. `just` places test limits before the wrapper's
command separator. `run-constrained.sh` validates the limits, resolves the backing block device
when disk throttling is requested, starts any requested CPU competitors, and enters a systemd
user scope. The recipe-owned command then runs without accepting workflow-controlled command
substitution.

The workflow never consumes issue, pull-request, or user input. Its only write occurs in the
notification job after a scheduled failure, using GitHub's job token and a body composed from
GitHub-owned repository/run values.

## Failure behavior

- A missing Linux/systemd/cgroup prerequisite fails the cell with `run-constrained.sh`'s existing
  exit 3 diagnostic during the real-wrapper preflight; it is not silently converted to an
  unconstrained run or conflated with a later workspace-test failure.
- An unresolved block device or unsupported write cap fails the disk cell before tests start.
- Any test, stress, or scale failure fails only its matrix cell; `fail-fast: false` preserves the
  remaining cell evidence.
- A cell that stops making progress is failed by the 90-minute job timeout. This is an operational
  cost and notification bound, not a test-duration acceptance threshold; timeout failure flows to
  the same dependent scheduled-notification job as any other cell failure.
- A scheduled failure causes the notification job to file a tracking issue. A notification
  failure remains visible as a failed job rather than masking the resource failure.
- A manual failure does not create an issue because the dispatcher already has the run record.

## Security and trust boundaries

The change adds no public runtime entry point. It does add a scheduled CI job with repository
checkout and a scheduled-failure issue-write capability.

- GitHub schedule/dispatch control is trusted repository control; no untrusted matrix or command
  input is accepted.
- Checkout keeps `persist-credentials: false`; test jobs receive only `contents: read`.
- Only the notification job receives `issues: write`, and only when a scheduled run has failed.
- The issue title is literal. The body interpolates GitHub-owned server, repository, and numeric
  run identifiers through environment variables and quotes them as one CLI argument.
- Existing repository code executes on a disposable runner under the same trust model as other
  scheduled heavy workflows. Fork pull-request code cannot trigger this workflow.

Secrets, fork execution, dependency changes, broader workflow permissions, and self-hosted runner
hardening are outside scope.

## Verification and bite checks

1. Add recipe-shape assertions to a focused shell selftest before changing the `justfile`; observe
   failure, implement the recipes, then observe green. Wire that selftest into `just ci` so the
   fixed command boundary remains a repository guardrail.
2. Run the existing `just run-constrained-selftest` to preserve parsing and limit validation.
3. Execute each new recipe with `--print-plan` limits where possible so the real just-to-wrapper
   argument boundary is observed without running the expensive suites.
4. Run `just scan-session-scale` once locally and report both ignored diagnostic results.
5. Run a reduced stress cell through `just stress-constrained` with the existing stress environment
   knobs and report its conservation result.
6. Run `actionlint .github/workflows/constrained-resources.yml` and `just ci`.
7. After merge, manually dispatch the workflow and report the run on #582. Report the first
   scheduled run's findings there as well; scheduled failures file their own tracking issue.

## Durable handoff

- Branch: `feat/constrained-resource-ci-582`
- Base branch: `main`
- Scope token: `q582-95e2b519`
- Guardrails: focused recipe selftest; `just run-constrained-selftest`; new recipe executions;
  `actionlint .github/workflows/constrained-resources.yml`; `just ci`.
- Post-merge obligations: manual dispatch evidence and first scheduled-run findings on issue #582.
