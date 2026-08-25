# ADR 0078: chaos-e2e resolves its ffmpeg release series at runtime

## Status

Accepted

## Context

`chaos-e2e` needs ffmpeg/ffprobe 7.0+, which the `ubuntu-latest` apt package does not
provide, so the job downloads a build from BtbN/FFmpeg-Builds. That download has now broken
three times, each time taking the whole weekly job down before a single test ran:

- `0d0c0ce1` pinned a dated `autobuild-*` tag plus a SHA-256; BtbN garbage-collects those
  tags, so the URL 404'd.
- `7212262f` re-pinned to a newer dated tag; the same collection took it out again.
- `359ba425` moved to the rolling `latest` release to escape tag collection and replaced the
  digest with a runtime version assertion — but named the release series in the asset
  filename (`ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz`). BtbN retired the `n7.1` series, and
  the URL 404'd (issue #536).

Each fix removed one pin and left another. What remains is the series pin, and it is the last
thing in the URL that BtbN controls the lifetime of.

The series pin also states the requirement wrongly. `third_party/chaos-librarian/README.md`
documents a floor — "ffmpeg 7.0+, ffprobe 7.0+" — while the workflow asserted equality
against `7.1`.

`chaos-e2e` is not a merge gate: `just ci` does not invoke it, and the workflow triggers only
on `schedule` and `workflow_dispatch`. Nothing proves this step before a scheduled run does,
which is how all three fixes reached `main` and then failed in production.

## Decision

The workflow names a **major-version floor**, not a series, and resolves the series at
runtime from what BtbN currently publishes.

`scripts/select-ffmpeg-asset.sh` reads BtbN's `latest` release asset names on stdin and takes
the floor as its argument. It considers only whole-line matches of
`ffmpeg-n<major>.<minor>-latest-linux64-gpl-<major>.<minor>.tar.xz` — excluding the `master`
build, which is not a release series, and the `-shared-` variants, which need a library
search path the job does not set up — and prints the **lowest** matching series whose major
meets the floor. Lowest, because the requirement is a floor: the suite should not adopt a new
ffmpeg major the day BtbN publishes one, and the selection then moves only when BtbN retires
the series in use. With no qualifying asset it exits non-zero and says so, which is the
signal that a human must decide something.

The selection lives in a script rather than inline in the workflow so that
`scripts/select-ffmpeg-asset-selftest.sh` can prove it under `just ci` — including against
the asset list that produced issue #536. This follows the `<name>.sh` / `<name>-selftest.sh`
pair already used by the repository's five guard scripts.

The workflow keeps acquisition: the release it reads names from, the download URL, extraction,
`GITHUB_PATH`, and a post-install assertion that the extracted binary reports a major version
at or above the same floor.

## Consequences

- BtbN retiring a series is absorbed automatically; only retiring *every* series at or above
  the floor stops the job, and it stops with a diagnostic rather than a 404.
- The ffmpeg version the suite runs against is no longer fixed by the repository. It changes
  when BtbN retires the series in use. The floor is the only guarantee, which is what the
  Chaos Librarian contract actually states.
- The job gains a GitHub REST read of a public repository, authenticated with the job token.
  The workflow's `contents: read` permission is unchanged and sufficient.
- Asset names become a parsed untrusted input. The whole-line-anchored pattern is the control;
  it admits no shell metacharacter, no path separator, and no digits into arithmetic beyond
  `[0-9]`.
- Executing a BtbN binary remains an accepted, unchanged risk. This decision neither adds nor
  removes integrity evidence for the archive's contents — `359ba425` established that no
  digest can pin a rolling artifact.
- One more script, selftest, `justfile` recipe, and `prek` hook to maintain, and `just ci`
  grows one fast step.

## Considered & rejected

- **Bump the series pin to `n8.1`.** verified: `gh api
  repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name'` on 2026-08-25 lists
  only `master`, `n8.1`, and `n9.0` `linux64-gpl` assets — `n7.1` was published when
  `359ba425` pinned it on 2026-06-11 and gone by the failing run on 2026-08-24. The same
  retirement reaches `n8.1`, making this the fourth instance of the fix that has already
  failed three times.
- **Install ffmpeg from apt, as `ci.yml` does.** verified:
  `docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md` records the
  `ubuntu-latest` apt package resolving to 6.1.1 during post-merge validation, below the
  documented 7.0+ floor. That finding is why this job downloads a build at all.
- **Pin a SHA-256 again.** verified: `359ba425`'s commit message records that the `latest`
  asset's bytes change on every BtbN rebuild, so a recorded digest stops matching within
  days; that commit removed the digest check for this reason.
- **Select the highest published series rather than the lowest.** judgment: it would move the
  suite onto a new ffmpeg major the day BtbN publishes one, for no stated requirement — the
  contract is a floor.
- **Vendor a build into the repository, or build ffmpeg from source in the job.** judgment: a
  ~100 MB binary in git history, or minutes of build time per run, to serve one non-gating
  weekly job.
- **Switch to another distributor.** judgment: every candidate is another single publisher
  with its own retention policy; the failure mode would be re-acquired rather than removed,
  and the trust question would be reopened without a better answer.
- **Keep the selection inline in the workflow.** verified: the step's logic reached `main`
  broken three times (`0d0c0ce1`, `7212262f`, `359ba425`) because nothing gates it —
  `.github/workflows/ci.yml` runs `just ci`, and `just ci` does not invoke `chaos-e2e-ci`.
  Inline logic here is provable only by a live dispatch against that day's BtbN state.
- **Do nothing and let the job stay red.** judgment: the weekly schedule exists so this
  otherwise dispatch-only job cannot silently rot; a permanently failing job that opens an
  issue every Monday is the rot, wearing the detector's clothes.
