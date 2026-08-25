# ADR 0078: chaos-e2e resolves its ffmpeg release series at runtime

## Status

Accepted

## Context

`chaos-e2e` needs ffmpeg/ffprobe 7.0+, which the `ubuntu-latest` apt package does not
provide, so the job downloads a build from BtbN/FFmpeg-Builds. That download has 404'd
twice, each time taking the whole weekly job down before a single test ran:

- `0d0c0ce1` pinned a dated `autobuild-*` tag plus a SHA-256. BtbN garbage-collects those
  tags, and run 27394271913 (2026-06-12) failed on the pruned tag.
- `7212262f` re-pinned to a newer dated tag. It went green — run 27394705387, the same day —
  and was superseded 17 minutes later by `359ba425`, before any prune reached it. It never
  failed.
- `359ba425` moved to the rolling `latest` release to escape tag collection and replaced the
  digest with a runtime version assertion — but named the release series in the asset
  filename (`ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz`). BtbN retired the `n7.1` series and
  run 32696142015 (2026-08-24) 404'd, which is issue #536.

Each fix removed one pin and left another. What remains is the series pin, and it is the last
thing in the URL that BtbN controls the lifetime of. Every pin was green when it merged and
decayed weeks later, so no pre-merge gate can catch this: at merge time nothing is wrong.

The series pin also states the requirement wrongly. `third_party/chaos-librarian/README.md`
documents a floor — "ffmpeg 7.0+, ffprobe 7.0+" — while the workflow asserted equality
against `7.1`.

`chaos-e2e` is not a merge gate: `just ci` does not invoke `chaos-e2e-ci`, and the workflow
triggers only on `schedule` and `workflow_dispatch`.

**This change does not return the weekly job to green.** `main`'s chaos-e2e also fails at the
step *after* the install — runs 31361822593 (2026-08-10) and 32000334810 (2026-08-17) both
failed in `Run Chaos Librarian E2E`, where
`transcode_required_executes_real_worker_and_commits_hevc_mkv` panics with "artifact commit
path escaped storage root". That defect is owned by issue #491 and fixed by PR #498, which is
open and unmerged. Fixing the install moves the failure one step later until #498 lands.

## Decision

The workflow names a **major-version floor**, not a series, and resolves the series at
runtime from what BtbN currently publishes.

`scripts/select-ffmpeg-asset.sh` reads BtbN's `latest` release asset names on stdin and takes
the floor as its argument. It considers only whole-line matches of
`ffmpeg-n<major>.<minor>-latest-linux64-gpl-<major>.<minor>.tar.xz`, excluding two families:
the `-shared-` variants, which need a library search path the job does not set up, and the
`master` build, which is not a release series — and which Chaos Librarian would reject
anyway, since its version normalizer returns `None` for git-snapshot strings like
`N-118412-g0ce1c8f7c5` and treats that as failing the minimum.

Among the matches it prints the **lowest** series, compared as a numeric `(major, minor)`
pair. Numeric, because a lexical comparison over asset names puts `n10.0` below `n8.1` and
would silently invert the rule at ffmpeg 10; the selftest covers a double-digit major for
that reason. Lowest, because the requirement is a floor: the suite should not adopt a new
ffmpeg major the day BtbN publishes one, and the selection then moves only when BtbN retires
the series in use. With no qualifying asset it exits non-zero and says so.

The selection lives in a script rather than inline so that
`scripts/select-ffmpeg-asset-selftest.sh` can prove it under `just ci`, following the
`<name>.sh` / `<name>-selftest.sh` pair already used by the repository's five guard scripts.

The workflow keeps acquisition: the release it reads names from, the download URL, extraction,
`GITHUB_PATH`, and a post-install assertion that the extracted binary reports a major version
at or above the same floor. It checks the release read for emptiness before selection runs.

## Consequences

- BtbN retiring a series is absorbed automatically; only retiring *every* series at or above
  the floor stops the job, and it stops with a diagnostic rather than a 404.
- **The first selection under this rule is a major bump.** BtbN's lowest qualifying series is
  now `n8.1`, so the suite moves from ffmpeg 7.1 to 8.1 on the next run. Chaos Librarian will
  admit it: `third_party/chaos-librarian/src/chaos_librarian/materializer/tooling/capabilities.py`
  parses the reported version and enforces `MIN_VERSIONS["ffmpeg"] = Version("7.0")` with no
  upper bound, and its normalizer accepts BtbN's `n8.1` form. Admitting it is not the same as
  working under it: no test in either tree exercises a non-7.x ffmpeg, so the suite's
  behaviour on ffmpeg 8 is unverified when this record is accepted, and it is recorded here as
  an open residual rather than a settled one.
- A `workflow_dispatch` run on the implementing branch is the only available check on that
  residual, and it is **confounded**: the branch forks from `main`, so the run will go red in
  `Run Chaos Librarian E2E` for issue #491's reason whatever ffmpeg 8.1 does. Its bare
  conclusion settles nothing. What it does show is the install step, the tool-version gate,
  and the per-test results — 11 passed / 1 failed on ffmpeg 7.1 is the baseline to compare
  against, and the same 11 passing on 8.1 is the evidence this residual can get before #498
  merges.
- The ffmpeg version the suite runs against is no longer fixed by the repository; it moves
  when BtbN retires the series in use. The floor is the only guarantee this decision makes,
  and a floor is not a compatibility statement.
- **The series pin is replaced by a pin on BtbN's filename grammar and on the `latest` tag.**
  That grammar is not stable across BtbN's own history: the autobuild assets `0d0c0ce1` and
  `7212262f` pinned had a different shape
  (`ffmpeg-n7.1.4-39-ga5faeca88f-linux64-gpl-7.1.tar.xz`). A rename in the `latest` catalogue
  matches nothing and fails the job. A grammar changes far less often than the set of series
  being built, so this is a smaller dependency — but it is the one that would break next.
- An empty release read and a retired catalogue stay distinguishable only because the workflow
  checks for them separately. The script sees names on stdin, so an API outage, a rate limit,
  a token problem and a genuine rename would otherwise all arrive as the same empty input and
  produce the same "no qualifying asset" message, naming the wrong cause.
- Live-catalogue divergence stays detectable only when the job runs; no local guardrail
  contacts BtbN. The selftest proves the selection logic against recorded catalogues. That
  failure class is bounded by this decision, not removed.
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
  retirement reaches `n8.1`, making this the third instance of a fix that has already failed
  twice.
- **Pin the series but fall back to runtime resolution when it is gone.** This keeps the
  version in use repository-controlled and reviewable — a bump would land in a PR where a
  dispatch run could precede it — while retirement is absorbed rather than fatal. judgment:
  the fallback path runs only on the day it is needed, so it is the one branch never
  exercised before it matters, which is the same untested-branch argument that puts the
  selection in a selftested script below. It also leaves two mechanisms doing one job, and
  the pinned half still has to be maintained by hand.
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
- **Cache or mirror the working archive** — an `actions/cache` entry keyed on the resolved
  asset name, or a copy attached to a release in this repository. This is the only option
  considered that removes the dependency on BtbN publishing anything on the day the job runs,
  and a cache miss degrades to the download path this decision builds anyway, so it is
  additive rather than a substitute that can fail. judgment: it is rejected on cost, not
  effectiveness — a cache step plus a key derived from the resolved asset name, holding a
  ~126 MB entry against a repository-wide cache budget, to serve one non-gating weekly job.
  (For the mirror half: it would make this repository a redistributor of a GPL ffmpeg build
  and leave a binary nobody reviews going stale in a release.) Note the eviction rule makes
  the cache half weaker than it looks — GitHub removes entries not accessed in over 7 days
  ([caching docs][cache-docs]), against a Monday cron.
- **Vendor a build into the repository, or build ffmpeg from source in the job.** verified: the
  `n8.1` linux64-gpl asset is 126,529,420 bytes (`gh api ... --jq '.assets[] | .size'`,
  2026-08-25) — that in git history, or minutes of build time per run, to serve one
  non-gating weekly job.
- **Switch to another distributor.** verified: the closest candidate, johnvansickle's
  `ffmpeg-release-amd64-static.tar.xz`, is a versionless URL that would remove the
  series-prediction problem outright — but `curl -sI` on 2026-08-25 returns
  `last-modified: Sat, 24 Aug 2024 16:01:05 GMT`, and its release readme reports
  `version: 7.0.2`. It is a two-year-stale build that would drift below any future floor,
  so it trades a retirement problem for an abandonment problem.
- **Keep the selection inline in the workflow.** verified: nothing gates this step — CI runs
  `just ci` (`.github/workflows/ci.yml`), `just ci` does not invoke `chaos-e2e-ci`
  (`justfile`), and the workflow triggers only on `schedule` and `workflow_dispatch`. The
  previous steps were pinned URLs, where that gap cost nothing because there was no behaviour
  to test. This step has branches — grammar, floor, ordering, no-match — and inline shell in
  an ungated workflow puts every one of them beyond reach of any check until a Monday.
- **Do nothing and let the job stay red.** judgment: the weekly schedule exists so this
  otherwise dispatch-only job cannot silently rot; a permanently failing job that opens an
  issue every Monday is the rot, wearing the detector's clothes.

[cache-docs]: https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching
