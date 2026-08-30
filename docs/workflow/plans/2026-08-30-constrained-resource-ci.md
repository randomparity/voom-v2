# Constrained-resource scheduled CI implementation plan

## Goal

Add a weekly and manually dispatchable Linux workflow that runs workspace tests across baseline,
CPU-load, disk-throttled, and reduced-memory cells, runs distributed stress in baseline and disk
cells, and adopts the 100,000-location scan diagnostic. Each cell owns a fixed justfile command
and runs on a fresh hosted runner under the resource wrapper. ADR 0096 governs isolation and
recipe ownership.

Tech stack: GitHub Actions YAML, `just`, Bash, Cargo/Rust tests, systemd cgroup v2.

## Global constraints

- Branch: `feat/constrained-resource-ci-582`; base: `main`; scope token: `q582-95e2b519`.
- Cells are exactly baseline (`--cpus 0-3 --memory 16G`), CPU load (baseline plus `--load 1`),
  disk (baseline plus `--write-bps 40M`), and memory (`--cpus 0-3 --memory 8G`).
- Every cell runs the canonical `just test` recipe through `just test-constrained`, preserving its
  worker-binary prebuild and `VOOM_TEST_PREBUILT_WORKERS=1` contract.
- Baseline and disk run the existing `just stress` behavior through `just stress-constrained`.
- Baseline runs both ignored `scan_session_scale` diagnostics through
  `just scan-session-scale-constrained`.
- Each matrix job has `timeout-minutes: 90` and `fail-fast: false`.
- The workflow has only `workflow_dispatch` and weekly `schedule` triggers; it is not a PR gate.
- Test jobs receive `contents: read`; only scheduled failure notification receives
  `issues: write`; checkout uses `persist-credentials: false`.
- No dependency, production Rust, database, schema, public protocol, ENOSPC, deterministic
  simulation, or performance-budget changes.
- Existing pinned action revisions are reused unchanged: `actions/checkout` at
  `3d3c42e5aac5ba805825da76410c181273ba90b1`, `Swatinem/rust-cache` at
  `258712b0b7b1ddf8bddc9fc3b0faca682b2736c3`, and `extractions/setup-just` at
  `53165ef7e734c5c07cb06b3c8e7b647c5aa16db3`.
- Local guardrails: recipe red/green probes below; `just run-constrained-selftest`;
  `just constrained-recipes-selftest`; `actionlint .github/workflows/constrained-resources.yml`;
  `just ci`.
- Post-merge obligations, not pre-merge claims: manually dispatch the default-branch workflow
  and report it on #582; report the first scheduled run's findings on #582.

## File map

- Modify `justfile`: pass wrapper limits before fixed commands and add fixed constrained stress
  and scale recipes; add the recipe-boundary selftest to `just ci`.
- Create `scripts/constrained-recipes-selftest.sh`: invoke the public just recipes through
  `--print-plan` and assert their exact limits and fixed commands.
- Create `.github/workflows/constrained-resources.yml`: schedule/dispatch, four cells, conditional
  stress/scale steps, and scheduled-failure notification.
- Create `docs/operations/constrained-resource-testing.md`: lane contract, cell table, local
  reproduction, expected failures, and post-merge evidence checklist.
- The existing `scripts/run-constrained.sh`, stress harness, and scan test are consumed unchanged.

## Task 1 — Expose fixed constrained recipes

### Interfaces

Consumes `scripts/run-constrained.sh [LIMITS] -- COMMAND`, existing `just stress`, and Cargo test
target `voom-control-plane --test scan_session_scale`. Produces these exact interfaces for Task 2:

```text
just test-constrained [LIMITS...]
just stress-constrained [LIMITS...]
just scan-session-scale
just scan-session-scale-constrained [LIMITS...]
```

### Steps

1. Run `just test-constrained --load 1 --print-plan`. Expect failure because current `justfile`
   puts `--load 1 --print-plan` after `cargo test`, proving the wrapper-option interface is absent.
2. Run `just stress-constrained --print-plan`. Expect `error: Justfile does not contain recipe`.
3. Run `just scan-session-scale`. Expect `error: Justfile does not contain recipe`.
4. Create `scripts/constrained-recipes-selftest.sh` as a Bash selftest that runs these commands
   from the repository root, captures their tab-separated plans, and fails unless each exact field
   is present:

   ```bash
   #!/usr/bin/env bash
   set -uo pipefail

   failures=0
   fail() { echo "FAIL: $1" >&2; failures=$((failures + 1)); }
   expect_field() {
       local label=$1 field=$2 want=$3
       shift 3
       local plan got
       plan=$(just "$@" 2>/dev/null)
       got=$(printf '%s\n' "$plan" | awk -F'\t' -v k="$field" '$1 == k {print $2}')
       [ "$got" = "$want" ] || fail "$label: expected $field=$want, got ${got:-<missing>}"
   }

   expect_field "test limits" load 1 test-constrained --load 1 --print-plan
   expect_field "test command" command "just test" \
       test-constrained --load 1 --print-plan
   expect_field "stress limits" write-bps 40M \
       stress-constrained --write-bps 40M --print-plan
   expect_field "stress command" command "just stress" \
       stress-constrained --write-bps 40M --print-plan
   expect_field "scale limits" memory 8G \
       scan-session-scale-constrained --memory 8G --print-plan
   expect_field "scale command" command "just scan-session-scale" \
       scan-session-scale-constrained --memory 8G --print-plan

   if [ "$failures" -gt 0 ]; then
       echo "constrained-recipes-selftest: $failures failure(s)" >&2
       exit 1
   fi
   echo "constrained-recipes-selftest: all checks passed"
   ```

5. Make the script executable. Add this recipe and include it in the root `ci` dependency list:

   ```just
   constrained-recipes-selftest:
       ./scripts/constrained-recipes-selftest.sh
   ```

6. Run `just constrained-recipes-selftest`. Expect failure because the changed and new recipes
   do not yet expose the asserted wrapper plans. This is the durable red bite check.
7. Replace the existing constrained recipe and add the three fixed recipes:

   ```just
   # Run the workspace suite under runner-like limits (Linux cgroup v2 only)
   test-constrained *LIMITS:
       ./scripts/run-constrained.sh {{ LIMITS }} -- just test

   # Run the opt-in stress harness under explicit resource limits
   stress-constrained *LIMITS:
       ./scripts/run-constrained.sh {{ LIMITS }} -- just stress

   # Run both ignored 100,000-location scan diagnostics
   scan-session-scale:
       cargo test -p voom-control-plane --test scan_session_scale -- --ignored --nocapture

   # Run the scan diagnostics under explicit resource limits
   scan-session-scale-constrained *LIMITS:
       ./scripts/run-constrained.sh {{ LIMITS }} -- just scan-session-scale
   ```

8. Run `just constrained-recipes-selftest`. Expect
   `constrained-recipes-selftest: all checks passed`.
9. Run each cheap plan proof and require the shown fields:

   ```text
   just test-constrained --load 1 --print-plan
   # load<TAB>1
   # command<TAB>just test

   just stress-constrained --write-bps 40M --print-plan
   # write-bps<TAB>40M
   # command<TAB>just stress

   just scan-session-scale-constrained --memory 8G --print-plan
   # memory<TAB>8G
   # command<TAB>just scan-session-scale
   ```

10. Run `just --dry-run scan-session-scale`. Expect the exact Cargo command from step 7.
11. Run `just run-constrained-selftest`. Expect `run-constrained-selftest: all checks passed`.
12. Commit `justfile` and `scripts/constrained-recipes-selftest.sh` with
    `test: expose constrained resource recipes`.

### Acceptance

Wrapper options reach `run-constrained.sh` before `--`; callers cannot replace any recipe-owned
command; both ignored scale tests share one reachable command; none of the expensive recipes is
added to `just ci`.

## Task 2 — Add the scheduled matrix

### Interfaces

Consumes all four Task 1 recipes. Produces the workflow name `constrained-resources`, job
`constrained`, matrix keys `name`, `preflight_limits`, `limits`, `run_stress`, and `run_scale`, plus
dependent job `notify-failure`.

### Steps

1. Run `test ! -e .github/workflows/constrained-resources.yml`. Expect exit 0, establishing the
   new workflow is absent.
2. Create `.github/workflows/constrained-resources.yml` with this structure:

   ```yaml
   name: constrained-resources

   on:
     workflow_dispatch:
     schedule:
       - cron: "0 7 * * 1"

   permissions:
     contents: read

   concurrency:
     group: constrained-resources-${{ github.ref }}
     cancel-in-progress: false

   jobs:
     constrained:
       name: constrained (${{ matrix.name }})
       runs-on: ubuntu-latest
       timeout-minutes: 90
       strategy:
         fail-fast: false
         matrix:
           include:
             - name: baseline
               preflight_limits: --cpus 0-3 --memory 16G
               limits: --cpus 0-3 --memory 16G
               run_stress: true
               run_scale: true
             - name: cpu-load
               # Do not start load generators in preflight: debt 0006 means the
               # real test would inherit them and then start a second set.
               preflight_limits: --cpus 0-3 --memory 16G
               limits: --cpus 0-3 --memory 16G --load 1
               run_stress: false
               run_scale: false
             - name: disk
               preflight_limits: --cpus 0-3 --memory 16G --write-bps 40M
               limits: --cpus 0-3 --memory 16G --write-bps 40M
               run_stress: true
               run_scale: false
             - name: memory
               preflight_limits: --cpus 0-3 --memory 8G
               limits: --cpus 0-3 --memory 8G
               run_stress: false
               run_scale: false
       steps:
         - name: Checkout
           uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
           with:
             persist-credentials: false
         - name: Cache cargo
           uses: Swatinem/rust-cache@258712b0b7b1ddf8bddc9fc3b0faca682b2736c3
         - name: Install just
           uses: extractions/setup-just@53165ef7e734c5c07cb06b3c8e7b647c5aa16db3
         - name: Install media tools
           run: |
             sudo apt-get update
             sudo apt-get install -y ffmpeg mkvtoolnix
         - name: Verify resource controls
           run: ./scripts/run-constrained.sh ${{ matrix.preflight_limits }} -- true
         - name: Run constrained workspace tests
           run: just test-constrained ${{ matrix.limits }}
         - name: Run constrained distributed stress
           if: matrix.run_stress
           run: just stress-constrained ${{ matrix.limits }}
         - name: Run constrained scan scale
           if: matrix.run_scale
           run: just scan-session-scale-constrained ${{ matrix.limits }}

     notify-failure:
       name: notify failure
       needs: constrained
       if: failure() && github.event_name == 'schedule'
       runs-on: ubuntu-latest
       permissions:
         issues: write
       steps:
         - name: Open tracking issue
           env:
             GH_TOKEN: ${{ github.token }}
             RUN_URL: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}
           run: |
             gh issue create \
               --repo "$GITHUB_REPOSITORY" \
               --title "Scheduled constrained-resource run failed" \
               --body "The weekly constrained-resource workflow failed. Run: $RUN_URL"
   ```

3. Run `actionlint .github/workflows/constrained-resources.yml`. Expect exit 0 and no output.
4. Run `git diff --check`. Expect exit 0.
5. Commit the workflow with `ci: schedule constrained resource tests`.

### Acceptance

The four literal cells and conditional steps match ADR 0096; failures do not cancel siblings;
every cell is bounded to 90 minutes; media prerequisites match the established Linux all-features
job; the real wrapper is proven before compilation; the CPU preflight omits load while the CPU
test applies `--load 1` exactly once; only scheduled failures can reach the issue-write job; no
fork or pull-request event can execute the workflow.

## Task 3 — Document and exercise the operator path

### Interfaces

Consumes the Task 1 recipe names and Task 2 cell names. Produces
`docs/operations/constrained-resource-testing.md` as the operator entry point.

### Steps

1. Create the operations document with:
   - the weekly/manual and non-merge-gate purpose;
   - a table of the four exact cell limits and which optional suites run;
   - Linux/systemd/cgroup-v2 prerequisites and exit-3 fail-loud behavior;
   - local commands for each constrained recipe;
   - the scheduled-failure issue behavior;
   - the known disposable-runner containment of debt record 0006;
   - a post-merge checklist linking #582 for manual dispatch and first schedule findings.
2. Run both scale diagnostics with `just scan-session-scale`. Expect two ignored tests to pass and
   report their elapsed time; the non-ignored fixture test must not run under `--ignored`.
3. Run a reduced real constrained stress cell:

   ```text
   VOOM_STRESS_NODES=1 VOOM_STRESS_RUNNERS_PER_NODE=1 VOOM_STRESS_TICKETS=8 \
     VOOM_STRESS_PROCESS_CRASH_PERCENT=0 \
     just stress-constrained --cpus 0 --memory 2G
   ```

   Expect the conservation test to pass with eight terminal tickets, no leaked lease, and no
   duplicate non-abandoned execution.
4. Run `actionlint .github/workflows/constrained-resources.yml`; expect no output and exit 0.
5. Run `just ci`; expect the repository guardrails, including
   `constrained-recipes-selftest`, to pass and print `==> All CI checks passed`. This command does
   not run the opt-in stress or scale diagnostics.
6. Re-read `git diff main...HEAD` for scope, action pins, workflow permissions, fixed command
   ownership, and exact documentation values. Commit the operations document with
   `docs: document constrained resource testing`.

### Acceptance

The operator can reproduce every cell from the document; the locally executed opt-in evidence is
exactly the scale recipe plus one reduced baseline stress cell; workflow preflights for all four
hosted cells remain explicit post-merge evidence. The workflow is linted and repository guardrails
are green. If local systemd or cgroup prerequisites are absent, record the exact exit-3 diagnostic
rather than claiming the constrained execution ran.

## Rollback and post-merge evidence

The change is removed by reverting its commits: deleting the workflow stops future schedules and
deleting the new recipes/document removes only opt-in test surfaces. It writes no application
state. A scheduled failure issue is external evidence and is not deleted by rollback.

Before merge, the PR must state that GitHub-hosted execution is not yet proven. After merge, the
operator manually dispatches `constrained-resources`, records its URL and cell findings on #582,
then records the first scheduled run there. A failure is evidence to diagnose, not permission to
weaken or silently bypass a constraint.
