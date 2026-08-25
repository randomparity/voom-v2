# Issue 536 — chaos-e2e resolves its ffmpeg series at runtime

## Problem

The weekly scheduled `chaos-e2e` workflow fails before running any test. Run
[32696142015](https://github.com/randomparity/voom-v2/actions/runs/32696142015), step
`Install ffmpeg 7`:

```text
curl: (22) The requested URL returned error: 404
```

The step downloads a hard-coded URL:

```text
https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz
```

BtbN's `latest` release no longer publishes an `n7.1` asset. As of 2026-08-25 its
`linux64-gpl` assets are:

```text
ffmpeg-master-latest-linux64-gpl-shared.tar.xz
ffmpeg-master-latest-linux64-gpl.tar.xz
ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz
ffmpeg-n8.1-latest-linux64-gpl-shared-8.1.tar.xz
ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz
ffmpeg-n9.0-latest-linux64-gpl-shared-9.0.tar.xz
```

BtbN rotates which release series it builds. The `n7.1` series was retired, and the
pinned URL went with it.

### This is the second failure of the same step

| Commit | Approach | Outcome |
|---|---|---|
| `0d0c0ce1` | Pin a dated `autobuild-*` tag + SHA-256 | Green at merge; 404 once BtbN pruned the tag (run 27394271913, 2026-06-12) |
| `7212262f` | Re-pin to a newer dated tag + SHA-256 | Green (run 27394705387); superseded 16 minutes later by `359ba425`, never pruned |
| `359ba425` | Rolling `latest` asset, pinned to series `n7.1` | Green for two months; 404 once BtbN retired `n7.1` (run 32696142015, 2026-08-24) |

Two failures, not three — `7212262f`'s pin was replaced pre-emptively rather than breaking.
The pattern that matters is not the count but the shape: **every pin was green when it
merged and decayed afterwards.** No pre-merge gate can catch that, because at merge time
nothing is wrong. A fix cannot promise to make the decay detectable earlier; what it can do
is stop the job from depending on an artifact name the repository has to predict.

`359ba425`'s commit message states its goal as "ending the recurring prune breakage." It
removed the tag pin and the digest pin but left a third pin in place — the **version
series**, encoded in the asset filename. That is the pin this design removes.

### Open issues this work resolves

- **#499** (P1) — the same 404, reported six days before #536, against dispatch run
  32090435836. Its proposed fix repoints at daily tag `autobuild-2026-08-11-13-11`, which has
  since been pruned and now 404s itself. Resolved here alongside #536.
- **#500** (P2, tech-debt) — the durable-fix issue. Its three options (cache, mirror as a
  release asset, publish a container image) are each dispositioned in ADR 0078, and its
  "decide separately" clause on the ffmpeg version is answered by the requirement below.
  Resolved here.
- **#493** — stale; see "A floor is not a compatibility statement" below. Closed by this work.

## Requirement, restated

The Chaos Librarian submodule documents its floor at
`third_party/chaos-librarian/README.md:33`:

> For materialize and run workflows: ffmpeg 7.0+, ffprobe 7.0+ [...]

The requirement is a **floor**, not an exact version. `359ba425` encoded it as an equality
against `7.1`, which is stricter than the requirement and is exactly what BtbN's rotation
invalidates. The `ubuntu-latest` apt package cannot satisfy the floor — the issue-73 design
spec records it resolving to 6.1.1 — so `apt install ffmpeg` remains unavailable to this job.

### The requirement, as the project states it

Verify against **7.0 or later**. Any version satisfying that is sufficient. Take the **oldest
available** version that meets it, and pin neither the version nor its digest. A test that
fails under a newer ffmpeg is a *wanted notification* — the project would rather learn its
expectations were version-coupled than spend maintenance chasing a source for an older build.

That last clause is what settles the question #500 left open ("whether to stay on ffmpeg 7.1
at all... needs its own evaluation rather than being folded into a CI fix"). There is no
separate evaluation pending: any qualifying version is acceptable by decision, so the CI fix
is the whole of it.

### A floor is not a compatibility statement

The documented "7.0+" says what is too old. It says nothing about whether the suite works on
an arbitrarily newer major. Under the requirement above that gap is accepted deliberately
rather than closed — but it should be stated, not discovered.

Chaos Librarian does check the floor, and checks it properly:
`third_party/chaos-librarian/src/chaos_librarian/materializer/tooling/capabilities.py` defines
`MIN_VERSIONS["ffmpeg"] = Version("7.0")`, parses the reported version with
`^ffmpeg version (\S+)`, and normalizes it through
`^[nN]?(\d+(?:\.\d+){0,2})` — so BtbN's `n8.1-...` string parses to `8.1` and passes. Two
useful facts fall out. There is **no upper bound** anywhere, so nothing rejects a newer major.
And git-snapshot strings like `N-118412-g0ce1c8f7c5` normalize to `None`, which the gate
treats as failing the minimum — independent confirmation that excluding BtbN's `master` asset
from selection is right, since Chaos Librarian would refuse it at startup.

Admitting a version is not the same as working under it. No test in either tree exercises a
non-7.x ffmpeg, and BtbN's lowest qualifying series today is `n8.1`, so the **first run under
this design moves the suite from ffmpeg 7.1 to 8.1** — a major bump nothing here has been run
against. The one filed observation of a 7.x-vs-8.x divergence in this suite, **#493** (filed
2026-08-17), no longer applies. It rested on
`assert_eq!(scan.json["data"]["summary"]["failed"], 1)` — an assertion on ffprobe 7.1's
tolerance for a damaged container header, which ffprobe 8.0.1 reads instead of rejecting.
`0f146f4b` (2026-08-22, on `main`) dropped that assertion and renamed the test to
`malformed_media_scan_request_stays_accepted_without_worker_side_effects`
(`crates/voom-cli/tests/chaos_librarian_e2e.rs:372`), which now checks only that the scan
request is accepted and that one ticket exists — both ffprobe-version-independent. #493 is
stale and this work closes it.

(The earlier `4d8d781d` is *not* the resolving commit: it predates #493 by two months and
introduced the very test name the issue cites.)

So the residual is the untested remainder, not a known break. Per the requirement above it is
accepted rather than closed, and if ffmpeg 8 does surface a coupling, that red job is the
notification the project asked for.

### The dispatch run is confounded, and this change does not turn the job green

`main`'s chaos-e2e already fails at the step *after* the install. Runs 31361822593
(2026-08-10) and 32000334810 (2026-08-17) both failed in `Run Chaos Librarian E2E`, where
`transcode_required_executes_real_worker_and_commits_hevc_mkv` panics with "artifact commit
path escaped storage root" (11 passed / 1 failed). That defect is owned by **issue #491** and
fixed by **PR #498**, which is open and currently `CONFLICTING`.

Two consequences for this work, both of which it must state rather than discover:

1. Fixing the install moves the weekly failure one step later. The Monday issue keeps being
   filed until #498 lands. This design does not claim otherwise.
2. A `workflow_dispatch` run on this branch will go red at the suite step whatever ffmpeg 8.1
   does, so its **bare conclusion settles nothing**. What it does settle is everything up to
   and including the suite's per-test results: the install step, `Verify external tools`, the
   Chaos Librarian tool gate, and which tests pass. The baseline is 11 passed / 1 failed on
   ffmpeg 7.1; the same 11 passing on 8.1 is the evidence the residual can get before #498
   merges. That comparison, not a green checkmark, is what this design reads.

**What that comparison covers, and what it does not.** Encoding *is* covered. Chaos
Librarian's materializer shells out to ffmpeg for every scenario it builds, and
`transcode_noop_does_not_schedule_worker_mutation` materializes `voom-ci/hevc-noop.yaml`,
whose `codec: hevc` maps to `libx265` via
`third_party/chaos-librarian/src/chaos_librarian/media_matrix.py:47`. That test is inside the
passing eleven, so the dispatch exercises ffmpeg 8's libx265 encode.

The gap is narrower than "the encoder". `TranscodeWorkerLaunch` appears exactly once in
`crates/voom-cli/tests/chaos_librarian_e2e.rs`, inside
`transcode_required_executes_real_worker_and_commits_hevc_mkv` — the test #491 masks. So what
stays uncharacterised is **voom's own `voom-ffmpeg-worker` invocation and the artifact-commit
path around it**, not ffmpeg 8's encoding as such. For that one test the first run after #498
lands carries two new variables at once, and anyone triaging a failure there must rule out
ffmpeg 8 before concluding #498 regressed.

## Goal

The `chaos-e2e` ffmpeg install stops naming a version series. It selects a series from what
BtbN currently publishes, subject to the documented floor, and fails with an actionable
message when nothing published meets the floor.

## Non-goals

- Changing the ffmpeg distributor away from BtbN. Recorded as a rejected alternative in
  ADR 0078.
- Changing `just chaos-e2e-ci` or the Chaos Librarian suite. The job never reached them.
- Changing `ci.yml`'s apt-based ffmpeg install. That job carries no 7.0+ floor.
- Reinstating a digest pin. `359ba425` established that BtbN's rolling asset bytes change on
  every rebuild, so a digest cannot track it.

## Design

### Selection rule

Choose the **lowest** published release series whose major version meets the floor, comparing
series as a numeric `(major, minor)` pair.

Numeric, not lexical. `sort` over asset names puts `n10.0` below `n8.1`, so a lexical
comparison would silently invert the rule the first time ffmpeg reaches major 10 — returning
the *highest* series, which is the outcome this rule exists to avoid. The selftest carries a
double-digit-major case so the ordering is pinned by a test rather than by a reading.

Lowest, not highest, for two reasons. It is the most conservative version that satisfies the
stated requirement, so the suite does not silently start exercising a brand-new ffmpeg major
the moment BtbN publishes one. And it is stable: the selection only moves when BtbN retires
the series in use, which is precisely the event this design exists to absorb.

Candidate assets must match this exact ERE, as a whole line — **the dots are escaped, and
that is load-bearing, not cosmetic**:

```text
^ffmpeg-n([0-9]+)\.([0-9]+)-latest-linux64-gpl-([0-9]+)\.([0-9]+)\.tar\.xz$
```

Unescaped, `.` matches any character including `/`, so
`ffmpeg-n8/1-latest-linux64-gpl-8/1.tar.xz` would match and, interpolated as
`"$FFMPEG_RELEASE_BASE_URL/$asset"`, would leave the release path entirely — defeating the one
control the threat model calls load-bearing. A selftest case pins the escaping.

ERE has no back-references, so the prefix/suffix agreement **cannot** be expressed in the
pattern. Compare captures 1==3 and 2==4 as a separate post-match check.

**Write no separate exclusion rules.** Being whole-line anchored, this pattern already admits
neither `ffmpeg-master-latest-linux64-gpl.tar.xz` (`master` is not `n<digits>.<digits>`) nor
`ffmpeg-n8.1-latest-linux64-gpl-shared-8.1.tar.xz` (`-gpl-` must be followed immediately by
the version). Both are excluded structurally, and coding a `master` check or a `-shared-`
check on top would be dead branches in a script whose whole justification is that its branches
are few enough to selftest. The selftest asserts the exclusions hold; the script does not
implement them.

Both exclusions are wanted, for the record: the `master` build is not a release series and
carries no series to check a floor against, and the job invokes the extracted binaries
directly with no shared-library search path, so the static build is the one that works.

Requiring the trailing version to repeat the `n` prefix is a cheap self-check: if BtbN
changes its naming scheme such that the two stop agreeing, the parse is no longer trusted
and the asset is skipped rather than guessed at.

### Where the selection lives

The selection logic goes in `scripts/select-ffmpeg-asset.sh`, not inline in the workflow.

The `chaos-e2e` workflow is not a merge gate; it runs weekly on a schedule and on manual
dispatch. Logic that lives only inside it is proven only by a live dispatch run against
whatever BtbN happens to publish that day — which is how three consecutive fixes reached
`main` and then failed in production. A script with a selftest is provable by `just ci` on
every commit, including against the exact asset list that produced this issue.

This also matches the repository's established shape: five guard scripts under `scripts/`
already ship as a `<name>.sh` / `<name>-selftest.sh` pair with a `justfile` recipe wired into
`just ci` and a `prek` hook delegating to that recipe.

The split of responsibility is:

- **`scripts/select-ffmpeg-asset.sh`** — pure selection. Reads candidate asset names from
  stdin, one per line; takes the major-version floor as its single argument; writes the
  chosen asset name to stdout. No network, no filesystem, no knowledge of the download host.
- **`.github/workflows/chaos-e2e.yml`** — acquisition. Owns the release the names come from,
  the download URL prefix, extraction, `GITHUB_PATH`, and the post-install version assertion.

Keeping the host and URL in the workflow keeps the download's provenance visible where a
reviewer looks for it, and keeps the script free of anything that needs a network to test.

### Workflow step

```yaml
      - name: Install ffmpeg
        env:
          GH_TOKEN: ${{ github.token }}
          # Chaos Librarian documents ffmpeg/ffprobe 7.0+ as its floor
          # (third_party/chaos-librarian/README.md). BtbN retires release series,
          # so the series is resolved at runtime rather than named here; see
          # docs/adr/0078-runtime-resolved-ffmpeg-series.md.
          FFMPEG_MAJOR_FLOOR: "7"
          FFMPEG_RELEASE_BASE_URL: https://github.com/BtbN/FFmpeg-Builds/releases/download/latest
        run: |
          assets="$RUNNER_TEMP/ffmpeg-assets.txt"
          gh api repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name' >"$assets"
          asset=$(./scripts/select-ffmpeg-asset.sh "$FFMPEG_MAJOR_FLOOR" <"$assets")
          echo "Selected asset: $asset"
          archive="$RUNNER_TEMP/ffmpeg.tar.xz"
          install_dir="$RUNNER_TEMP/ffmpeg"
          curl -fL "$FFMPEG_RELEASE_BASE_URL/$asset" -o "$archive"
          mkdir -p "$install_dir"
          tar -xJf "$archive" -C "$install_dir" --strip-components=1
          echo "$install_dir/bin" >> "$GITHUB_PATH"
          version_output=$("$install_dir/bin/ffmpeg" -version)
          installed_version=${version_output%%$'\n'*}
          echo "Installed: $installed_version"
          if [[ ! $installed_version =~ ^ffmpeg\ version\ n?([0-9]+)\. ]] \
            || ((10#${BASH_REMATCH[1]} < FFMPEG_MAJOR_FLOOR)); then
            echo "::error::expected ffmpeg >= $FFMPEG_MAJOR_FLOOR, got: $installed_version"
            exit 1
          fi
```

The step is renamed from `Install ffmpeg 7` to `Install ffmpeg`, because the version is no
longer a property of the step.

Three details are deliberate:

- **The release read goes to a file, not down a pipe into the script.** GitHub's default
  `run:` shell is `bash -e` *without* `pipefail`, so in a pipeline a failing `gh api` would
  not abort — it would hand the script empty input, which the script reports as "no
  qualifying asset": a true failure with the wrong cause named. Writing to a file lets `-e`
  catch the `gh` failure directly and lets the emptiness check speak for itself. It also
  leaves the catalogue on disk in the runner for anyone debugging the next BtbN change. No
  pipeline remains in the step, so no `shell:` override is needed and none is added.
- **The empty-read case is the script's exit `3`, not a workflow branch.** An API outage, a
  rate limit, a token problem, and a genuine asset rename would otherwise all reach the
  operator as the same "no qualifying asset" message. Only the rename is a retired-catalogue
  event; the others are read failures. Putting the distinction in the script means the
  selftest proves it, which an inline `[ ! -s ]` would not.
- **No `| head -n1`** on `ffmpeg -version`. Closing the pipe after one line can reap
  `ffmpeg` with `SIGPIPE`, which becomes a real failure the moment anything adds `pipefail`
  — a latent flake for no benefit. Bash's `${var%%$'\n'*}` takes the first line with no pipe
  at all, and `[[ =~ ]]` extracts the major version for the same reason.

`10#` forces base-10 on the captured digits so a hypothetical `08` series cannot be read as
an invalid octal literal.

### Assertion

The post-install check asserts `installed_major >= FFMPEG_MAJOR_FLOOR`. That is the
normative requirement, stated once, in the same terms the selection uses.

It deliberately does not assert that the installed version equals the selected series. A
mismatch there would mean BtbN's asset name disagrees with its contents; the outcome that
actually matters — whether the binary meets the floor — is what the floor check covers, and
covering it twice in different terms invites the two checks to drift.

`ffprobe` is not separately asserted. It is extracted from the same archive as `ffmpeg` and
is therefore the same build; the existing `Verify external tools` step continues to run
`ffprobe -version`, which proves it resolves and executes.

### Failure behavior

Two failures are kept distinct because they call for different responses, and **both live in
the script** so the selftest covers them. They are branches that run only on a bad day, which
is the class this extraction exists to bring under test — leaving them in the ungated workflow
would put them exactly where the "keep it inline" argument says logic must not go.

| Exit | Condition | Response |
|---|---|---|
| `0` | An asset was selected | — |
| `1` | At least one non-blank line arrived, none qualifying | BtbN retired every usable series; a human decides |
| `2` | Malformed floor argument | Caller error |
| `3` | No non-blank line on stdin | Release-read failure — API outage, rate limit, token, changed field; retry |

Reserving `2` for caller error matches `scripts/check-adr-index.sh`.

**Input grammar.** Exactly one argument, matching `^[0-9]+$`. Anything else exits `2`: no
argument, the empty string, two arguments, `abc`, `-1`, and — deliberately — `7.0`. The
project states its floor as "7.0+", so a caller passing `7.0` verbatim is plausible; rejecting
it loudly beats truncating it silently, and the workflow passes `7` literally.

**Blank lines are ignored**, and each line is trimmed of surrounding whitespace before
matching. The exit `1` / exit `3` split turns on non-blank lines, not on byte length: a
degraded read that emits a lone newline must reach `3`, not `1`.

That split is the whole point. Collapsing them would reintroduce the defect this change is
about, one layer up: an operator handed "no qualifying asset" during a GitHub API incident
would go looking at BtbN's release page and find nothing wrong. The `1` message names the
floor and how many candidates it considered; the `3` message says the read returned nothing.

**One 404 remains possible and is not this bug returning.** BtbN republishes `latest`'s assets
on every rebuild, so a download can 404 seconds after a successful catalogue read. That is
transient — a re-run resolves it — and it is not the series pin rotting. Nothing is engineered
around it; it is named here so the next operator to see `curl: (22)` does not reopen a solved
problem.

**One branch cannot move.** The post-install assertion that the extracted binary reports a
major version at or above the floor needs the binary itself, so it cannot sit behind the
script's stdin boundary and no pre-merge gate reaches it. It is the last check between a wrong
archive and a green install step, and the only evidence it works comes from a run. That
residual is recorded rather than engineered around — extracting a second script to verify
three lines of version comparison would cost more than it removes.

## Threat model

**The blast radius is a failed test.** ffmpeg here is a test-time tool: it synthesizes and
probes media inside one non-gating weekly job, and is never linked into, nor shipped with,
any voom binary. The job runs only on `schedule` and `workflow_dispatch` — never on
`pull_request` — so no outside contributor can trigger it, and it holds no secret beyond a
`contents: read` job token. That bounds what follows and is why this section is short.

**What the change adds** is one parsed untrusted input and one authenticated public read.
Asset *names* are new: text from BtbN, matched by `select-ffmpeg-asset.sh`. Asset *contents*
are not new — the job has executed BtbN's ffmpeg since `0d0c0ce1`, and this design makes that
neither more nor less trusted.

**Controls.**

- *Asset names* — the whole-line-anchored pattern is the load-bearing control. Admitting only
  `ffmpeg-n`, digits, `.`, `-latest-linux64-gpl-` and `.tar.xz` means a hostile name cannot
  carry a path separator, a shell metacharacter, or a URL that leaves the release; only
  `[0-9]` reaches arithmetic, base-10 forced. The matched name is used as a single URL path
  segment.
- *Asset contents* — the floor is asserted against the extracted binary's own `-version`
  output, over TLS via `curl -fL`. Extraction stays `tar -xJf` into a job-scoped
  `$RUNNER_TEMP`, unchanged from today.
- *API read* — `GH_TOKEN` is the job token, passed via `env:` and never interpolated into the
  `run:` body, so no template-injection surface is created. `contents: read` is unchanged and
  is sufficient for a public-repository read.

**Out of scope.**

- *A malicious or compromised BtbN build.* Accepted and pre-existing. No digest can pin a
  rolling artifact (`359ba425`), and the alternatives that would restore one are rejected in
  ADR 0078 — including a month-end pin that could carry a digest, declined because holding a
  specific older version in place is maintenance the project does not want. The accepted
  consequence is bounded by the blast radius above.
- *API response truncation.* Three outcomes, none a hazard. A partial list still containing
  the lowest qualifying series behaves normally; a list containing none fails loudly; and a
  list that drops the lowest but keeps a higher one yields a higher-but-still-qualifying
  selection with no diagnostic. That third case does violate the lowest-wins rule — worth
  naming, since a two-outcome framing would look complete — but it is accepted rather than
  engineered around, because any qualifying version is sufficient by decision. BtbN's `latest`
  carries 49 assets as of 2026-08-25 and returns them unpaginated.
- *Supply-chain review of `gh` itself.* Preinstalled on GitHub-hosted runners and already
  relied on by the `notify-failure` job in this same workflow.

## Documentation

`docs/operations/chaos-e2e.md` currently states:

> The workflow therefore installs ffmpeg/ffprobe from a pinned 7.x archive and
> verifies its checksum before running the suite.

Both halves have been false since `359ba425`, which removed the checksum verification and
the 7.x pin's stability. It is replaced with:

> The Ubuntu runner's apt ffmpeg package lags Chaos Librarian's minimum. The workflow
> therefore resolves the oldest BtbN release series meeting the 7.0+ floor at run time and
> asserts the installed binary's major version against that floor; neither the version nor its
> bytes are pinned. See `docs/adr/0078-runtime-resolved-ffmpeg-series.md`.

Line 10 of that file — "ffmpeg/ffprobe 7.0+" — stays exactly as it is. It is still true, and a
reader told the file is wrong may otherwise over-correct it.

## Acceptance criteria

Every selftest criterion names its input, so the case list is derivable from this spec rather
than invented by the implementer.

1. `.github/workflows/chaos-e2e.yml` contains no ffmpeg version series in any URL or
   filename. Check: `rg -n 'ffmpeg-n[0-9]' .github/workflows/chaos-e2e.yml` produces no
   output. (Not a list of today's series names — that check would decay exactly as the pin
   did.)

Selftest cases for `scripts/select-ffmpeg-asset.sh` (criteria 2–10):

2. Given the **full 49-name catalogue** on stdin and floor `7`, prints
   `ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz`, exit `0`. The fixture is the complete output
   of `gh api repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name'` on
   2026-08-25 — **not** the six-line `linux64-gpl` excerpt in the Problem section above. It
   must therefore carry the `linuxarm64`, `win64`, `winarm64` and `lgpl` entries, so the
   selection is shown to reject them: in particular
   `ffmpeg-n8.1-latest-linuxarm64-gpl-8.1.tar.xz` ties on `(major, minor)` and must not be
   selected.
3. Given that catalogue with the `n8.1` and `n9.0` linux64-gpl entries removed, exit `1` with
   a diagnostic on stderr naming the floor and the number of non-blank input lines considered.
   ("Considered" means lines read, not lines matching the ERE — that count is zero by
   construction whenever exit `1` fires.)
4. Given `ffmpeg-n10.0-latest-linux64-gpl-10.0.tar.xz` and
   `ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz`, prints the `n9.0` asset — numeric major
   comparison, not lexical.
5. Given `ffmpeg-n8.10-latest-linux64-gpl-8.10.tar.xz` and
   `ffmpeg-n8.9-latest-linux64-gpl-8.9.tar.xz`, prints the `n8.9` asset — the minor is
   compared numerically too. An implementation that parses the major as an integer and the
   minor as a string passes criterion 4 and fails this one.
6. Given only `master` and `-shared-` assets, exit `1`. Neither family is a candidate, and
   neither is excluded by a rule the script implements — the anchored pattern admits neither.
7. Given `ffmpeg-n8/1-latest-linux64-gpl-8/1.tar.xz`, exit `1`. This pins the escaped dots:
   an unescaped `.` matches `/`, and the resulting name would leave the release path when
   interpolated into the download URL.
8. Given `ffmpeg-n8.1-latest-linux64-gpl-9.0.tar.xz` and floor `7`, exit `1`. The ERE admits
   this name — only the post-match `1==3, 2==4` comparison rejects it. Without this case an
   implementation that drops captures 3 and 4 entirely passes every other criterion, leaving
   the one rule whose purpose is to notice an upstream naming drift with no coverage at all.
9. Floor-argument grammar, each exiting `2`: no argument, the empty string, `abc`, `-1`,
   `7.0`, and two arguments.
10. Given stdin that is empty, and again given stdin holding only a newline, exit `3` with a
    message naming the release read rather than the floor.

11. `just select-ffmpeg-asset-selftest` exits 0 and is reached by `just ci`.
12. `.pre-commit-config.yaml` carries a `select-ffmpeg-asset-selftest` hook whose `entry:`
    delegates to the `just` recipe, matching the five existing guard-script pairs, and
    `prek run --all-files` passes. Only the selftest hook is wanted: the selection script
    reads no repository file, so a non-selftest hook would have nothing to check.
13. `actionlint` reports no finding on `.github/workflows/chaos-e2e.yml`.
14. `just ci` passes.
15. `docs/operations/chaos-e2e.md` no longer claims a pin or a checksum:
    `rg -n 'pinned 7\.x|verifies its checksum' docs/operations/chaos-e2e.md` produces no
    output, and the replacement sentence from the Documentation section is present. Line 10's
    "ffmpeg/ffprobe 7.0+" is unchanged.
16. ADR 0078 exists and has exactly one row in `docs/adr/README.md`
    (`just check-adr-index` passes).
17. A `workflow_dispatch` run of `chaos-e2e` on the branch reaches `Run Chaos Librarian E2E`
    with **the asset the script resolves from the live catalogue** installed — `n8.1` as of
    2026-08-25; a different series is a pass, not a failure, since resolving a different
    series is the design working. Blocking conditions: the install step,
    `Verify external tools`, and Chaos Librarian's capability gate all pass; the run produces
    per-test results; and, **if PR #498 has not landed on this branch's base**, #491's
    `transcode_required_executes_real_worker_and_commits_hevc_mkv` is among the failures. If
    #498 has landed, that test is expected to pass and the run should show 12 passed — its
    passing is the good outcome, not a criterion failure.
    Any **additional** test failure under the newer ffmpeg does **not** block this change —
    that is the wanted notification the requirement describes — but it is filed as a new
    issue before merge. The run's URL and per-test outcome go in the PR.
18. Issues #499 and #500 are closed as resolved by this work; #493 closed as stale with the
    `0f146f4b` evidence in its closing comment; and any new issue required by criterion 17 is
    filed.

Criteria 2–10 are the selftest's cases, so criterion 11 checks them on every commit.
Criterion 17 is the only one no local guardrail can reach, and it deliberately does not ask
for a green run: #491 makes that unobtainable until PR #498 merges, and demanding it would
either block this fix behind an unrelated one or invite reading a red run as this change's
failure.

## Verification

- `just select-ffmpeg-asset-selftest`
- `just check-adr-index && just check-adr-index-selftest`
- `prek run --all-files`
- `actionlint`
- `zizmor .github/workflows/chaos-e2e.yml`
- `shellcheck scripts/select-ffmpeg-asset.sh scripts/select-ffmpeg-asset-selftest.sh`
- `just ci`
- Manual `workflow_dispatch` of `chaos-e2e` on the branch, read per-test rather than by its
  conclusion (see "The dispatch run is confounded" above). This is the only end-to-end
  evidence that the resolved asset downloads, extracts, satisfies the floor, and carries the
  suite through the ffmpeg 7.1 → 8.1 bump. No local guardrail contacts BtbN. Criterion 17
  defines what blocks and what does not.

## Related

- ADR 0078 — runtime-resolved ffmpeg release series (this change's decision record)
- ADR 0033 — records the BtbN rolling-build problem as the reason the toxiproxy harness
  chose differently
- `docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md` — the original
  chaos-e2e workflow design, source of the 7.0+ floor and the apt-is-insufficient finding
