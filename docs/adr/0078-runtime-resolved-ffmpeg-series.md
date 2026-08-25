# ADR 0078: chaos-e2e resolves its ffmpeg release series at runtime

## Status

Accepted

## Context

`chaos-e2e` needs ffmpeg/ffprobe 7.0+, which the `ubuntu-latest` apt package does not
provide, so the job downloads a build from BtbN/FFmpeg-Builds. That download has 404'd on two
separate pins, in both cases before a single test ran:

- `0d0c0ce1` pinned a dated `autobuild-*` tag plus a SHA-256. BtbN prunes daily tags, and run
  27394271913 (2026-06-12) failed on the pruned tag — caught on a dispatch run, not a Monday.
- `7212262f` re-pinned to a newer dated tag. It went green (run 27394705387, the same day) and
  was superseded 17 minutes later by `359ba425`, before any prune reached it. It never failed.
- `359ba425` moved to the rolling `latest` release to escape tag pruning and replaced the
  digest with a runtime version assertion — but named the release series in the asset
  filename (`ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz`). BtbN retired the `n7.1` series;
  run 32090435836 (2026-08-18, dispatch) and then run 32696142015 (2026-08-24, the weekly
  job) both 404'd. Those are issues #499 and #536.

Each fix removed one pin and left another. What remains is the series pin, and it is the last
thing in the URL whose lifetime BtbN controls. Every pin was green when it merged and decayed
weeks later, so no pre-merge gate catches this: at merge time nothing is wrong.

The series pin also states the requirement wrongly. `third_party/chaos-librarian/README.md`
documents a floor — "ffmpeg 7.0+, ffprobe 7.0+" — while the workflow asserted equality
against `7.1`.

`chaos-e2e` is not a merge gate: `just ci` does not invoke `chaos-e2e-ci`, and the workflow
triggers only on `schedule` and `workflow_dispatch`.

### Open issues this decision resolves

- **#499** — the same 404, reported six days before #536. Its proposed fix repoints at daily
  tag `autobuild-2026-08-11-13-11`, which has since been pruned and now 404s itself. Both it
  and #536 are resolved by this decision.
- **#500** — the durable-fix issue, which enumerates caching, mirroring as a release asset,
  and publishing a container image. All three are dispositioned below. #500 also holds that
  "whether to stay on ffmpeg 7.1 at all... needs its own evaluation rather than being folded
  into a CI fix." **That position is explicitly overtaken**: the project's stated requirement
  is verification against 7.0 or later, any qualifying version being sufficient, with a test
  failure under a newer ffmpeg preferred as a notification over the cost of sourcing an older
  one. There is no separate version decision left to make.
- **#493** — recorded a 7.1-vs-8.0.1 divergence in
  `malformed_media_records_per_file_failure_without_execution_ticket`. It is **stale**:
  `4d8d781d` renamed that test, and the current
  `malformed_media_scan_request_stays_accepted_without_worker_side_effects`
  (`crates/voom-cli/tests/chaos_librarian_e2e.rs:372`) asserts only that the scan request is
  accepted and that one ticket exists — both independent of the ffprobe version. Closed by
  this work.

**This change does not return the weekly job to green.** `main`'s chaos-e2e also fails at the
step after the install — runs 31361822593 and 32000334810 failed in `Run Chaos Librarian E2E`
with "artifact commit path escaped storage root". That defect is owned by issue #491 and
fixed by PR #498, open and unmerged. Fixing the install moves the failure one step later
until #498 lands.

## Decision

The workflow names a **major-version floor**, not a series, and resolves the series at
runtime from what BtbN currently publishes. Nothing about the version or its digest is
pinned.

`scripts/select-ffmpeg-asset.sh` reads BtbN's `latest` release asset names on stdin and takes
the floor as its argument. It considers only whole-line matches of
`ffmpeg-n<major>.<minor>-latest-linux64-gpl-<major>.<minor>.tar.xz`, excluding two families:
the `-shared-` variants, which need a library search path the job does not set up, and the
`master` build, which is not a release series — and which Chaos Librarian would refuse
anyway, since its version normalizer returns `None` for git-snapshot strings like
`N-118412-g0ce1c8f7c5` and treats that as failing the minimum.

Among the matches it prints the **lowest** series, compared as a numeric `(major, minor)`
pair. Numeric, because a lexical comparison over asset names puts `n10.0` below `n8.1` and
would silently invert the rule at ffmpeg 10; the selftest covers a double-digit major for
that reason. Lowest, because the requirement is a floor and the oldest qualifying build is
the one to take. With no qualifying asset it exits non-zero and says so.

The selection lives in a script rather than inline so that
`scripts/select-ffmpeg-asset-selftest.sh` can prove it under `just ci`, following the
`<name>.sh` / `<name>-selftest.sh` pair already used by the repository's five guard scripts.

The workflow keeps acquisition: the release it reads names from, the download URL, extraction,
`GITHUB_PATH`, and a post-install assertion that the extracted binary reports a major version
at or above the same floor. It checks the release read for emptiness before selection runs.

## Consequences

- BtbN retiring a series is absorbed automatically; only retiring *every* series at or above
  the floor stops the job, and it stops with a diagnostic rather than a 404.
- The ffmpeg version the suite runs against is no longer fixed by the repository; it moves
  when BtbN retires the series in use. The floor is the only guarantee this decision makes.
- **The first selection under this rule is a major bump**, from 7.1 to 8.1. Chaos Librarian
  admits it:
  `third_party/chaos-librarian/src/chaos_librarian/materializer/tooling/capabilities.py`
  parses the reported version, enforces `MIN_VERSIONS["ffmpeg"] = Version("7.0")` with no
  upper bound, and its normalizer accepts BtbN's `n8.1` form. No test in either tree exercises
  a non-7.x ffmpeg, so behaviour under ffmpeg 8 is otherwise untested. The one filed
  observation of a 7.x-vs-8.x divergence in this suite, #493, no longer applies (above), so
  the residual is the untested remainder rather than a known break.
- **A test that fails under a newer ffmpeg is a wanted signal, not a defect of this design.**
  That is the trade the floor buys: the project would rather learn from a red weekly job that
  its expectations were version-coupled than spend maintenance chasing a source for an older
  build. A failure of that kind is a finding about the suite's real compatibility range.
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
- **The blast radius of a bad build is a failed test.** ffmpeg here is a test-time tool that
  synthesizes and probes media inside one non-gating weekly job; it is not linked into, nor
  shipped with, any voom binary, and the job holds no secret beyond its `contents: read`
  token. Dropping the digest therefore costs integrity evidence the project does not need at
  this boundary — which is also why `359ba425`'s finding, that no digest can pin a rolling
  artifact, settles the question rather than reopening it.
- One more script, selftest, `justfile` recipe, and `prek` hook to maintain, and `just ci`
  grows one fast step.

## Considered & rejected

- **Bump the series pin to `n8.1`.** verified: `gh api
  repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name'` on 2026-08-25 lists
  only `master`, `n8.1`, and `n9.0` `linux64-gpl` assets — `n7.1` was published when
  `359ba425` pinned it on 2026-06-11 and gone by the failing run on 2026-08-24. The same
  retirement reaches `n8.1`, making this the third instance of a fix that has already failed
  twice.
- **Pin a retained month-end `autobuild-*` tag, keeping 7.1 and restoring the SHA-256.**
  This is the strongest pin available and was not obvious: verified: BtbN retains every
  month-end tag — 37 retained releases, a complete monthly series back to
  `autobuild-2024-09-30-15-36` — while pruning dailies within about two weeks, which is why
  #499's daily pin died; `autobuild-2026-07-31-14-10` still carries
  `ffmpeg-n7.1.5-12-g1fdbca85aa-linux64-gpl-7.1.tar.xz` (`gh api .../releases --paginate`,
  2026-08-25). Being immutable, it also readmits a digest check. judgment: rejected anyway,
  because the project's requirement is any ffmpeg at or above 7.0 and it does not want to
  spend maintenance holding a specific older version in place. A pin here buys version
  stability nobody asked for and costs a recurring manual bump as 7.1 ages out of new
  snapshots.
- **Pin the series but fall back to runtime resolution when it is gone.** judgment: the
  fallback path runs only on the day it is needed, so it is the one branch never exercised
  before it matters — the same untested-branch argument that puts the selection in a
  selftested script below. It also leaves two mechanisms doing one job, and the pinned half
  still has to be maintained by hand.
- **Install ffmpeg from apt, as `ci.yml` does.** verified:
  `docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md` records the
  `ubuntu-latest` apt package resolving to 6.1.1 during post-merge validation, below the
  documented 7.0+ floor. That finding is why this job downloads a build at all.
- **Pin a SHA-256 against the rolling asset.** verified: `359ba425`'s commit message records
  that the `latest` asset's bytes change on every BtbN rebuild, so a recorded digest stops
  matching within days; that commit removed the digest check for this reason.
- **Select the highest published series rather than the lowest.** judgment: the requirement
  is a floor, and the oldest qualifying build is the one to take; selecting the highest would
  move the suite onto a new ffmpeg major the day BtbN publishes one, for no stated reason.
- **Cache or mirror the working archive** (#500's options 1 and 2) — an `actions/cache` entry
  keyed on the resolved asset name, or a copy attached to a release in this repository. This
  is the only family considered that removes the dependency on BtbN publishing anything on the
  day the job runs, and a cache miss degrades to the download path this decision builds
  anyway, so it is additive rather than a substitute that can fail. judgment: rejected on cost
  rather than effectiveness — a cache step plus a key derived from the resolved asset name,
  holding a ~126 MB entry against a repository-wide cache budget, for one non-gating weekly
  job. The mirror half would additionally make this repository a redistributor of a GPL ffmpeg
  build and leave a binary nobody reviews going stale in a release. Its stated draw, a
  pinnable digest, is not one this boundary needs (see Consequences). The cache half is also
  weaker than it looks: GitHub removes entries not accessed in over 7 days
  ([caching docs][cache-docs]), against a Monday cron.
- **Publish a container image with ffmpeg baked in** (#500's option 3). judgment: it does
  remove the per-run BtbN dependency, and unlike building from source it builds once and is
  pulled per run. It is rejected on standing cost — an image build, a registry, credentials,
  and a second artifact to keep current — for one non-gating weekly job whose worst failure
  is a red Monday.
- **Vendor a build into the repository.** verified: the `n8.1` linux64-gpl asset is
  126,529,420 bytes (`gh api ... --jq '.assets[] | .size'`, 2026-08-25) — that, in git
  history, for one non-gating weekly job.
- **Build ffmpeg from source in the job.** judgment: tens of minutes of build time on a
  hosted runner, every run, to obtain what a download provides in seconds.
- **Switch to another distributor.** verified: the closest candidate, johnvansickle's
  `ffmpeg-release-amd64-static.tar.xz`, is a versionless URL that would remove the
  series-prediction problem outright — but `curl -sI` on 2026-08-25 returns
  `last-modified: Sat, 24 Aug 2024 16:01:05 GMT`, and its release readme reports
  `version: 7.0.2`. It is a two-year-stale build that would drift below any future floor,
  trading a retirement problem for an abandonment problem.
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
