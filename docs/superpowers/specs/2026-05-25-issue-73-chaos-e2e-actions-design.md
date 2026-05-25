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
`uv python install 3.13`, installs ffmpeg and MKVToolNix with `apt`, verifies
the key tool versions, then runs `just chaos-e2e-ci`.

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
- `just chaos-e2e-local-script-test` to keep the existing Chaos shell harness
  smoke coverage intact.

The full `just chaos-e2e-ci` command is expected to run in GitHub Actions after
push because it depends on the same external media tooling this workflow
installs.
