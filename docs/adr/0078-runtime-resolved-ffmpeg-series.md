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
thing in the URL that BtbN controls the lifetime of.

Every pin was green when it merged and decayed afterwards. That is the shape of the defect,
and it is worth stating precisely because it bounds what any fix can promise: the job depends
on a third party continuing to publish one specific artifact, and that dependency expires on
a schedule nobody here controls. No pre-merge gate can catch it, because at merge time
nothing is wrong. What a fix can do is stop the dependency from being on an artifact whose
name the repository has to predict.

The series pin also states the requirement wrongly. `third_party/chaos-librarian/README.md`
documents a floor — "ffmpeg 7.0+, ffprobe 7.0+" — while the workflow asserted equality
against `7.1`.

## Decision

The workflow names a **major-version floor**, not a series, and resolves the series at
runtime from what BtbN currently publishes.

`scripts/select-ffmpeg-asset.sh` reads BtbN's `latest` release asset names on stdin and takes
the floor as its argument. It considers only whole-line matches of
`ffmpeg-n<major>.<minor>-latest-linux64-gpl-<major>.<minor>.tar.xz` — excluding the `master`
build, which is not a release series, and the `-shared-` variants, which need a library
search path the job does not set up — and prints the **lowest** matching series, compared as
a numeric `(major, minor)` pair, whose major meets the floor. Numeric, because the obvious
lexical comparison over asset names puts `n10.0` below `n8.1` and would silently invert the
rule at ffmpeg 10; the selftest covers a double-digit major for that reason. Lowest, because
the requirement is a floor: the suite should not adopt a new ffmpeg major the day BtbN
publishes one, and the selection then moves only when BtbN retires the series in use. With
no qualifying asset it exits non-zero and says so.

The selection lives in a script rather than inline in the workflow because it is logic, not a
URL: a grammar, a floor filter, an ordering, and a no-match path, each wrong in ways reading
does not catch. `scripts/select-ffmpeg-asset-selftest.sh` proves that logic under `just ci`
against recorded catalogues — including the one that produced issue #536. It proves nothing
about what BtbN publishes on the day the job runs; that stays observable only when the job
runs. A pinned URL had no logic to test at all, which is the difference. This follows the
`<name>.sh` / `<name>-selftest.sh` pair already used by the repository's five guard scripts.

The workflow keeps acquisition: the release it reads names from, the download URL, extraction,
`GITHUB_PATH`, and a post-install assertion that the extracted binary reports a major version
at or above the same floor.

## Consequences

- BtbN retiring a series is absorbed automatically; only retiring *every* series at or above
  the floor stops the job, and it stops with a diagnostic rather than a 404.
- **The first selection under this rule is a major bump.** BtbN's lowest qualifying series is
  now `n8.1`, so the suite moves from ffmpeg 7.1 to 8.1 on the next run. The Chaos Librarian
  contract documents a *minimum*; nothing states or tests an upper bound —
  `src/chaos_librarian/materializer/content/source_capabilities.py` gates only on
  `ffmpeg_available`, with no version parse anywhere. The suite's behaviour on a newer major
  is therefore unverified, and a break would surface as a red weekly job that reads like a
  product regression. A `workflow_dispatch` run on the implementing branch is what settles
  it, and its result is recorded in Context before this record is accepted.
- The ffmpeg version the suite runs against is no longer fixed by the repository; it moves
  when BtbN retires the series in use. The floor is the only guarantee this decision makes,
  and — per the bullet above — a floor is not a compatibility statement.
- **The series pin is replaced by a pin on BtbN's filename grammar and on the `latest` tag.**
  That grammar is not stable across BtbN's own history: the autobuild assets `0d0c0ce1` and
  `7212262f` pinned had a different shape
  (`ffmpeg-n7.1.4-39-ga5faeca88f-linux64-gpl-7.1.tar.xz`). A rename in the `latest` catalogue
  matches nothing and fails the job. This is a smaller dependency than a named series — a
  grammar changes far less often than the set of series being built — but it is a dependency,
  and it is the one that would break next.
- An empty release read and a retired catalogue are distinguishable only because the workflow
  checks for them separately. The script sees names on stdin, so an API outage, a rate limit,
  a token problem and a genuine rename would otherwise all arrive as the same empty input and
  produce the same "no qualifying asset" message — naming the wrong cause. The workflow
  therefore fails distinctly when the release read yields no asset names at all.
- Live-catalogue divergence stays detectable only when the job runs. The selftest proves the
  selection logic against recorded catalogues; no local guardrail contacts BtbN. That failure
  class is unchanged by this decision — it is bounded by it, not removed.
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
- **Cache or mirror the working archive** — an `actions/cache` entry keyed on the resolved
  asset name, or a copy attached to a release in this repository. This is the only option
  considered that removes the dependency on BtbN publishing anything on the day the job runs,
  so it is rejected on its costs rather than on effectiveness. verified: GitHub evicts a cache
  entry after 7 days without a hit, and this job's cadence is a Monday cron — the cache would
  sit on the eviction boundary and miss unpredictably, restoring the network path it was
  meant to replace. judgment: mirroring instead makes this repository a redistributor of a GPL
  ffmpeg build, and leaves a binary nobody reviews going stale in a release.
- **Switch to another distributor.** judgment: every candidate is another single publisher
  with its own retention policy; the failure mode would be re-acquired rather than removed,
  and the trust question would be reopened without a better answer.
- **Keep the selection inline in the workflow.** verified: nothing gates this step — CI runs
  `just ci` (`.github/workflows/ci.yml`), `just ci` does not invoke `chaos-e2e-ci`
  (`justfile`), and the workflow triggers only on `schedule` and `workflow_dispatch`. The
  previous steps were pinned URLs, where that gap cost nothing to test around because there
  was no behaviour to test. This step has branches — grammar, floor, ordering, no-match — and
  inline shell in an ungated workflow puts every one of them beyond reach of any check until
  a Monday.
- **Do nothing and let the job stay red.** judgment: the weekly schedule exists so this
  otherwise dispatch-only job cannot silently rot; a permanently failing job that opens an
  issue every Monday is the rot, wearing the detector's clothes.
