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
  was superseded by `359ba425` sixteen minutes later, before any prune reached it. It never
  failed.
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

### Open issues this decision bears on

- **#499** — the same 404, reported six days before #536. Its fix is not merely proposed: it
  is committed as `57f1bb0e`, the head of PR #498, which rewrites this same install step to
  pin daily tag `autobuild-2026-08-11-13-11` — a tag that has since been pruned and 404s
  today. **The two changes edit the same lines**, so whoever rebases #498 must drop that
  commit rather than resolve the conflict toward it; taking #498's side would reinstate the
  exact 404 this decision removes. Both #499 and #536 are resolved by this decision.
- **#500** — the durable-fix issue, enumerating caching, mirroring as a release asset, and
  publishing a container image; all three are dispositioned below, on cost rather than merit.
  Its holding that "whether to stay on ffmpeg 7.1 at all... needs its own evaluation rather
  than being folded into a CI fix" is **explicitly overtaken** by the requirement in the
  Decision: any version at or above 7.0 is acceptable, so no separate version decision remains.
  **#500 is not closed by this work.** What it asks for is removing the per-run dependency on
  an upstream URL; this decision removes the dependency on a *predicted asset name* and leaves
  the job needing BtbN to publish and the API to answer on every run. Closing it would
  overstate what shipped.
- **#493** — recorded a 7.1-vs-8.0.1 divergence resting on
  `assert_eq!(scan.json["data"]["summary"]["failed"], 1)`, an assertion on one ffprobe
  version's tolerance for a damaged header. `0f146f4b` dropped that assertion and renamed the
  test to `malformed_media_scan_request_stays_accepted_without_worker_side_effects`, which
  checks only that the scan request is accepted and a ticket exists. Stale; closed by this
  work, with the evidence in its closing comment.

**This change does not return the weekly job to green.** `main`'s chaos-e2e also fails at the
step after the install — runs 31361822593 and 32000334810 failed in `Run Chaos Librarian E2E`
with "artifact commit path escaped storage root". That defect is owned by issue #491 and
fixed by PR #498 — open, and currently `CONFLICTING`, so its landing is not imminent. Fixing
the install moves the failure one step later until #498 lands.

## Decision

The workflow names a **major-version floor**, not a series, and resolves the series at
runtime from what BtbN currently publishes. Nothing about the version or its digest is
pinned.

The workflow reads asset names from `repos/BtbN/FFmpeg-Builds/releases/tags/latest` — the
release whose tag is literally `latest`, addressed by tag on purpose. GitHub's
`releases/latest` form asks for whichever release GitHub currently considers newest, which is
BtbN's to change by marking a dated autobuild; that would return a catalogue in the
`ffmpeg-n7.1.5-12-g1fdbca85aa-...` shape, match nothing, and report a wrong-endpoint bug as a
retired catalogue.

`scripts/select-ffmpeg-asset.sh` reads those names on stdin and takes
the floor as its argument. It considers only whole-line matches of
`ffmpeg-n<major>.<minor>-latest-linux64-gpl-<major>.<minor>.tar.xz`. Being anchored, that
pattern admits neither the `-shared-` variants nor the `master` build, so no separate
exclusion rule exists or should be written.

Among the matches it prints the **lowest** series, compared as a numeric `(major, minor)`
pair. Numeric, because a lexical comparison over asset names puts `n10.0` below `n8.1` and
would silently invert the rule at ffmpeg 10; the selftest covers a double-digit major for
that reason. Lowest, because the requirement is a floor and the oldest qualifying build is
the one to take.

Its failure paths are distinguished by exit code, because they call for different responses:
`3` when stdin carried no names at all — a release-read failure, retry it — and `1` when
names arrived but none qualified, which means BtbN has retired every usable series and a
human must decide something. (`2` is a malformed floor argument.) Both live in the script
rather than the workflow so the selftest covers them: they are error branches that run only
on a bad day, which is exactly the class this extraction exists to bring under test.

The selection lives in a script rather than inline so that
`scripts/select-ffmpeg-asset-selftest.sh` can prove it under `just ci`, following the
`<name>.sh` / `<name>-selftest.sh` pair that five of the repository's six guard scripts
already use.

The workflow keeps acquisition: the release it reads names from, the download URL, extraction,
`GITHUB_PATH`, and a post-install assertion that the extracted binary reports a major version
at or above the same floor.

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
  a non-7.x ffmpeg, and #493 no longer applies, so the residual is an untested remainder
  rather than a known break. Dispatching the workflow on this branch **now**, while #498 is
  still open, characterises most of that remainder ahead of merge — the install step, Chaos
  Librarian's capability gate, and the eleven tests that pass today — so those carry one new
  variable rather than two. Encoding is covered: Chaos Librarian's materializer shells out to
  ffmpeg on every scenario, and `transcode_noop_does_not_schedule_worker_mutation` materializes
  `voom-ci/hevc-noop.yaml` (`codec: hevc`, which `media_matrix.py` maps to `libx265`), so the
  dispatch does exercise ffmpeg 8's libx265 encode. **What it cannot
  reach** is narrower: voom's own `voom-ffmpeg-worker` invocation and the artifact-commit path
  around it, exercised only by
  `transcode_required_executes_real_worker_and_commits_hevc_mkv` — the single
  `TranscodeWorkerLaunch` in `crates/voom-cli/tests/chaos_librarian_e2e.rs`, and precisely the
  test #491 masks. For that one test the first Monday after #498 lands carries both new
  variables, so a failure there must not be read as #498 regressing without ruling out
  ffmpeg 8 first.
- **The dispatch run happened, and it did not go as this record predicted.** Run 32883241965
  on the implementing branch: the install step, `Verify external tools` and Chaos Librarian's
  capability gate all passed under `ffmpeg version n8.1.2-44-g7c533d0f86`, so the acquisition
  path is proven end to end. The suite, however, is **9 passed / 3 failed**, not the
  11-passed/1-failed baseline this record expected — and #491's recorded symptom does not
  appear at all. `transcode_required_executes_real_worker_and_commits_hevc_mkv` is the one
  policy-preflight failure — it alone carries "requires an unbound ffmpeg worker on owner
  node …". `transcode_noop_does_not_schedule_worker_mutation` is not: its `compliance report`
  exited 0 and it failed on a planner node status, `blocked` where `no_op` was expected. Both
  point at `main`'s own owner-node envelope rework rather than at ffmpeg 8, and for the second
  the grounds are firmer than provenance — the planner's snapshot comes from
  `probe_for_extension`, which synthesises the codec from the file extension, so no ffprobe
  output reaches that decision at all. No chaos-e2e run had exercised those commits, because
  the 404 masked every run since 2026-08-24. The third,
  `static_library_baseline_seeds_exports_and_compares`, is different: a post-materialization
  comparison finding (`D_SIDECAR_MISSING`, an `.eng.srt` sidecar observed as `.mkv`), and it is
  the one failure an ffmpeg-8 explanation is **not** excluded from. Tracked as #540.
  This is the "do nothing" bullet's argument landing exactly as written: the 404 was hiding
  the real state, and every Monday issue named the wrong cause.
- **The series pin is replaced by a pin on BtbN's filename grammar and on the `latest` tag.**
  That grammar is not stable across BtbN's own history: the autobuild assets `0d0c0ce1` and
  `7212262f` pinned had a different shape
  (`ffmpeg-n7.1.4-39-ga5faeca88f-linux64-gpl-7.1.tar.xz`). A rename in the `latest` catalogue
  matches nothing and fails the job. A grammar changes far less often than the set of series
  being built, so this is a smaller dependency — but it is the one that would break next.
- Live-catalogue divergence stays detectable only when the job runs; no local guardrail
  contacts BtbN. The selftest proves the selection logic against recorded catalogues. That
  failure class is bounded by this decision, not removed.
- **The post-install floor assertion stays inline**, outside the selftest's reach: it needs
  the extracted binary, so it cannot sit behind the stdin boundary, and no *automatic* gate
  runs it — `just ci` does not invoke this workflow. The evidence for it is a
  `workflow_dispatch` run on the implementing branch, which exercises the live catalogue read,
  the selection, the download, the extraction, `GITHUB_PATH` and the assertion together. That
  is the same path `7212262f` was validated on (run 27394705387), and it is a pre-merge check,
  not a post-merge hope.
- The job gains a GitHub REST read of BtbN's releases. It succeeds because that repository is
  public — the job token authenticates the request for rate-limit purposes but grants no
  access there, so no permission change applies and `contents: read` is unchanged.
- Asset names become a parsed untrusted input. The whole-line-anchored pattern is the control;
  it admits no shell metacharacter and no path separator. Its digit class is `[[:digit:]]`, not
  the range `[0-9]`: glibc collates a bracket range by locale, so under a UTF-8 locale a range
  admits Arabic-Indic and fullwidth digits, which then fail in `10#`. The script also exports
  `LC_ALL=C`; either construct suffices, and the selftest pins the class independently of the
  export.
- **The blast radius of a bad build is a failed test.** ffmpeg here is a test-time tool inside
  one non-gating weekly job; it is not linked into, nor shipped with, any voom binary, and the
  job holds no secret beyond its `contents: read` token.
- One more script, selftest, `justfile` recipe, and `prek` hook to maintain, and `just ci`
  grows one fast step.

## Considered & rejected

The first four bullets are all forms of pinning, which the project's requirement rules out;
their `verified:` grounds are kept because they are the evidence behind that call.

- **Bump the series pin to `n8.1`.** verified: `gh api
  repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name'` on 2026-08-25 lists
  only `master`, `n8.1`, and `n9.0` `linux64-gpl` assets — `n7.1` was published when
  `359ba425` pinned it on 2026-06-11 and gone by the failing run on 2026-08-24. The same
  retirement reaches `n8.1`, making this the third instance of a fix that has already failed
  twice.
- **Pin a retained month-end `autobuild-*` tag, keeping 7.1 and restoring the SHA-256.**
  The strongest pin available, and not obvious. verified: BtbN retains every month-end tag —
  37 retained releases, a complete monthly series back to `autobuild-2024-09-30-15-36` —
  while pruning dailies within about two weeks, which is why #499's daily pin died;
  `autobuild-2026-07-31-14-10` still carries
  `ffmpeg-n7.1.5-12-g1fdbca85aa-linux64-gpl-7.1.tar.xz` (`gh api .../releases --paginate`,
  2026-08-25). Being immutable, it readmits a digest check. judgment: it buys version
  stability nobody asked for, at the cost of a recurring manual bump as 7.1 ages out.
- **Take BtbN's versionless `master` asset.** This deserves a real answer, because it is the
  simplest thing that satisfies the outcome as stated: `ffmpeg-master-latest-linux64-gpl.tar.xz`
  names no series, so it needs no catalogue read, no selection script, no selftest, and none of
  the filename-grammar exposure this record calls the next thing to break. verified: it fails
  downstream anyway. The master build reports a git-snapshot version —
  `ffmpeg-N-125875-g5d4d3bdc61-linux64-gpl.tar.xz` in `autobuild-2026-07-31-14-10` (`gh api
  .../releases/tags/autobuild-2026-07-31-14-10 --jq '.assets[].name'`, 2026-08-25) — and
  Chaos Librarian's normalizer at `capabilities.py:55-71` returns `None` for exactly that form,
  which the caller treats as `meets_minimum=False`. It would download, extract, and then fail
  the capability gate, with no major version for the install step's assertion to parse either.
- **Pin the series but fall back to runtime resolution when it is gone.** judgment: it leaves
  two mechanisms doing one job, and the pinned half still has to be maintained by hand — a
  bump nobody is prompted to make until the fallback silently absorbs it. (Not rejected for
  having an untested branch: a selftest could feed it a catalogue with the pinned series
  absent, exactly as it does for selection.)
- **Install ffmpeg from apt, as `ci.yml` does.** verified:
  `docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md` records the
  `ubuntu-latest` apt package resolving to 6.1.1 during post-merge validation, below the
  documented 7.0+ floor. That finding is why this job downloads a build at all.
- **Pin a SHA-256 against the rolling asset.** verified: `359ba425`
  ("ci(chaos-e2e): track rolling ffmpeg 7.1 build instead of pinning a digest") removed the
  digest check on the ground that BtbN's rolling assets are rebuilt, so a recorded digest stops
  matching. The step comment that commit introduced put it as "the `latest` tag's bytes change
  on every rebuild (a pinned SHA256 can never match for long)" — cited from the commit rather
  than the workflow because **this change replaces that step**, and an immutable record must
  not point at text it deletes.
- **Select the highest published series rather than the lowest.** judgment: the requirement
  is a floor, and the oldest qualifying build is the one to take; selecting the highest would
  move the suite onto a new ffmpeg major the day BtbN publishes one, for no stated reason.
- **Cache or mirror the working archive** (#500's options 1 and 2) — an `actions/cache` entry
  keyed on the resolved asset name, or a copy attached to a release in this repository. Only
  the **mirror** half removes the dependency on BtbN publishing that day; a cache keyed on the
  resolved asset name still needs the catalogue read, and a stable-key variant lands on
  GitHub's 7-day eviction boundary against a 7-day cron ([caching docs][cache-docs]).
  judgment: rejected on cost — a ~126 MB entry against a repository-wide cache budget, for one
  non-gating weekly job — and the mirror half would additionally make this repository a
  redistributor of a GPL ffmpeg build. Its stated draw, a pinnable digest, is not one this
  boundary needs.
- **Publish a container image with ffmpeg baked in** (#500's option 3). judgment: it does
  remove the per-run BtbN dependency, and unlike building from source it builds once and is
  pulled per run. It is rejected on standing cost — an image build, a registry, credentials,
  and a second artifact to keep current — for one non-gating weekly job whose worst failure
  is a red Monday.
- **Vendor a build into the repository.** verified: the `n8.1` linux64-gpl asset is
  126,529,420 bytes (`gh api ... --jq '.assets[] | .size'`, 2026-08-25) — that, in git
  history, for one non-gating weekly job.
- **Build ffmpeg from source in the job.** judgment: a full source build on every run, to
  obtain what a download provides in seconds.
- **Switch to another distributor.** verified: the closest candidate, johnvansickle's
  `ffmpeg-release-amd64-static.tar.xz`, is a versionless URL that would remove the
  series-prediction problem outright — but `curl -sI` on 2026-08-25 returns
  `last-modified: Sat, 24 Aug 2024 16:01:05 GMT`, and its release readme reports
  `version: 7.0.2`. It is a two-year-stale build that would drift below any future floor,
  trading a retirement problem for an abandonment problem.
- **Use an existing action that already resolves BtbN at runtime.** verified:
  `AnimMouse/setup-ffmpeg` does exactly this — `gh api repos/BtbN/FFmpeg-Builds/releases/latest`
  filtered by `capture("^ffmpeg-n(?<v>[0-9]+(?:\\.[0-9]+)+)-latest-")` and ordered by
  `max_by(split(".") | map(tonumber))` (read from `scripts/version/Unix-like.sh` on `main`,
  2026-08-25). Its independent arrival at a numeric rather than lexical comparison corroborates
  that rule. judgment: rejected because it selects `max_by` — the highest series, the opposite
  of the floor rule — and a third-party composite action is supply-chain surface to review and
  SHA-pin in exchange for about fifteen lines of shell this repository can selftest.
- **Keep the selection inline in the workflow.** verified: nothing gates this step — CI runs
  `just ci` (`.github/workflows/ci.yml`), `just ci` does not invoke `chaos-e2e-ci`
  (`justfile`), and the workflow triggers only on `schedule` and `workflow_dispatch`. The
  previous steps were pinned URLs, where that gap cost nothing because there was no behaviour
  to test. This step has branches — grammar, floor, ordering, no-match — and inline shell in
  an ungated workflow puts every one of them beyond reach of any check until a Monday.
- **Do nothing and let the job stay red.** judgment: the Monday issue fires either way until
  PR #498 lands, so that is not the discriminator. What separates them is what the failure
  says: today's 404 stops the job before the suite runs, so it *masks* #491 and every Monday
  issue names the wrong cause. This change makes the remaining failure the real one.

[cache-docs]: https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching
