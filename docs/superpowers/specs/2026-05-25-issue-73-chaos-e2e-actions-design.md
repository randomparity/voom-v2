# Issue 73 Chaos E2E Actions Design

## Context

Issue #73 asks for an optional GitHub Actions path for `just chaos-e2e-ci`.
The command already exists and intentionally stays outside `just ci` because it
requires the Chaos Librarian Python environment plus real media tools.

The implementation should be test infrastructure only. It must not change the
default CI contract or product behavior.

## Goals

- Add a manually dispatched GitHub Actions workflow for deterministic Chaos
  Librarian E2E validation.
- Install or verify the external tools required by `just chaos-e2e-ci`: `uv`,
  Python 3.13, ffmpeg/ffprobe, and MKVToolNix.
- Initialize the pinned `third_party/chaos-librarian` submodule.
- Run `just chaos-e2e-ci` and allow the normal command output to surface
  failures.
- Document when maintainers should run the workflow.

## Non-Goals

- Do not add Chaos E2E to required push or pull request CI.
- Do not alter `just chaos-e2e-ci` behavior.
- Do not add broad caching or artifact upload before the runtime profile is
  known.
- Do not change product scan, policy, or worker behavior.

## Design

Create `.github/workflows/chaos-e2e.yml` with a single Ubuntu job triggered by
`workflow_dispatch`. The workflow checks out the repository with submodules,
installs Rust cache support and `just`, installs `uv`, installs Python 3.13 via
`uv python install 3.13`, installs ffmpeg/ffprobe 7.x from a pinned Linux
archive, installs MKVToolNix with `apt`, verifies the key tool versions, then
runs `just chaos-e2e-ci`.

Ubuntu runner packages are not sufficient for ffmpeg/ffprobe because the
`ubuntu-latest` apt package resolved to 6.1.1 during post-merge validation, below
Chaos Librarian's documented 7.0+ minimum. The workflow should download a pinned
BtbN FFmpeg build, verify its SHA-256, expose its `bin` directory through
`GITHUB_PATH`, and only then verify `ffmpeg -version` and `ffprobe -version`.

Use pinned action revisions to match the existing CI style. Keep permissions to
`contents: read` because the job only reads source and dependencies.

Add a short maintainer note under `docs/operations/chaos-e2e.md`. The note
explains that the workflow is manual, not a merge gate, and should be run before
or after changes that affect the Chaos Librarian integration, media scan,
ffprobe/ffmpeg workers, or policy execution.

## Error Handling

The workflow should rely on shell `set -e` behavior for installation and command
failures. Version verification steps should run before the expensive E2E command
so missing system tools fail clearly. `just chaos-e2e-ci` remains responsible for
Chaos-specific validation and test failure output.

## Verification

- `git diff --check`
- Review workflow syntax and trigger shape in `.github/workflows/chaos-e2e.yml`.
- `just fmt-check`
- `actionlint .github/workflows/chaos-e2e.yml`
- `just chaos-e2e-local-script-test` to keep the existing Chaos shell harness
  smoke coverage intact.
- Dispatch the manual `chaos-e2e` workflow on `main` after merge.

The full `just chaos-e2e-ci` command is expected to run in GitHub Actions after
push because it depends on the same external media tooling this workflow
installs.
