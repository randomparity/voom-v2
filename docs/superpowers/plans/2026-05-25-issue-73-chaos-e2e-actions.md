# Issue 73 Chaos E2E Actions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a manual GitHub Actions workflow for `just chaos-e2e-ci` and document when maintainers should run it.

**Architecture:** The workflow is isolated from normal CI by using only `workflow_dispatch`. It checks out the pinned Chaos Librarian submodule, installs the required media/Python tools, verifies versions, then delegates behavior to the existing `just chaos-e2e-ci` recipe.

**Tech Stack:** GitHub Actions, Ubuntu runner, Rust/Cargo, `just`, `uv`, Python 3.13, ffmpeg/ffprobe, MKVToolNix.

---

### Task 1: Add Manual Chaos E2E Workflow

**Files:**
- Create: `.github/workflows/chaos-e2e.yml`

- [x] **Step 1: Add the workflow file**

Create `.github/workflows/chaos-e2e.yml` with:

```yaml
name: chaos-e2e

on:
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: chaos-e2e-${{ github.ref }}
  cancel-in-progress: false

jobs:
  chaos-e2e:
    name: chaos-e2e
    runs-on: ubuntu-latest
    timeout-minutes: 60
    steps:
      - name: Checkout
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd  # v6.0.2
        with:
          persist-credentials: false
          submodules: true

      - name: Cache cargo
        uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32  # v2.9.1

      - name: Install just
        uses: extractions/setup-just@53165ef7e734c5c07cb06b3c8e7b647c5aa16db3  # v4.0.0

      - name: Install uv
        uses: astral-sh/setup-uv@f0ec1fc3b38f5e7cd731bb6ce540c5af426746bb  # v6.1.0

      - name: Install media tools
        run: |
          sudo apt-get update
          sudo apt-get install -y ffmpeg mkvtoolnix

      - name: Verify external tools
        run: |
          uv --version
          uv python install 3.13
          uv run --python 3.13 python --version
          ffmpeg -version
          ffprobe -version
          mkvmerge --version

      - name: Run Chaos Librarian E2E
        run: just chaos-e2e-ci
```

- [x] **Step 2: Verify the workflow is manual-only**

Run: `rg -n "workflow_dispatch|pull_request|push" .github/workflows/chaos-e2e.yml`

Expected: output contains `workflow_dispatch` and does not contain `pull_request` or `push`.

### Task 2: Add Maintainer Documentation

**Files:**
- Create: `docs/operations/chaos-e2e.md`

- [x] **Step 1: Add the maintainer note**

Create `docs/operations/chaos-e2e.md` with:

````markdown
# Chaos Librarian E2E

The Chaos Librarian deterministic E2E suite runs with:

```bash
just chaos-e2e-ci
```

It is intentionally outside default `just ci` because it requires `uv`, Python
3.13, ffmpeg/ffprobe, MKVToolNix, and the pinned Chaos Librarian submodule.

Maintainers should run the manual `chaos-e2e` GitHub Actions workflow before or
after changes that affect:

- the Chaos Librarian integration;
- media scan or observed-state export behavior;
- ffprobe, ffmpeg, or artifact verification workers;
- policy report or execution paths exercised by the Chaos fixtures.

The workflow is not a required merge gate. It exists to make the heavy media
suite reproducible in a clean runner while runtime cost and tool availability
are still being characterized.
````

- [x] **Step 2: Check documentation has no placeholders**

Run: `rg -n "TBD|TODO|implement later|fill in" docs/operations/chaos-e2e.md`

Expected: no matches and exit code 1.

### Task 3: Verify the Change

**Files:**
- Validate: `.github/workflows/chaos-e2e.yml`
- Validate: `docs/operations/chaos-e2e.md`

- [x] **Step 1: Check whitespace and patch hygiene**

Run: `git diff --check`

Expected: no output and exit code 0.

- [x] **Step 2: Run format check**

Run: `just fmt-check`

Expected: exit code 0.

- [x] **Step 3: Run existing Chaos local script smoke test**

Run: `just chaos-e2e-local-script-test`

Expected: exit code 0.

- [ ] **Step 4: Commit**

Run:

```bash
git add .github/workflows/chaos-e2e.yml docs/operations/chaos-e2e.md docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md docs/superpowers/plans/2026-05-25-issue-73-chaos-e2e-actions.md
git commit -m "ci: add optional chaos e2e workflow"
```

Expected: commit succeeds.
