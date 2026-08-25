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
| `7212262f` | Re-pin to a newer dated tag + SHA-256 | Green (run 27394705387); superseded 17 minutes later by `359ba425`, never pruned |
| `359ba425` | Rolling `latest` asset, pinned to series `n7.1` | Green for two months; 404 once BtbN retired `n7.1` (run 32696142015, 2026-08-24) |

Two failures, not three — `7212262f`'s pin was replaced pre-emptively rather than breaking.
The pattern that matters is not the count but the shape: **every pin was green when it
merged and decayed afterwards.** No pre-merge gate can catch that, because at merge time
nothing is wrong. A fix cannot promise to make the decay detectable earlier; what it can do
is stop the job from depending on an artifact name the repository has to predict.

`359ba425`'s commit message states its goal as "ending the recurring prune breakage." It
removed the tag pin and the digest pin but left a third pin in place — the **version
series**, encoded in the asset filename. That is the pin this design removes.

## Requirement, restated

The Chaos Librarian submodule documents its floor at
`third_party/chaos-librarian/README.md:33`:

> For materialize and run workflows: ffmpeg 7.0+, ffprobe 7.0+ [...]

The requirement is a **floor**, not an exact version. `359ba425` encoded it as an equality
against `7.1`, which is stricter than the requirement and is exactly what BtbN's rotation
invalidates. The `ubuntu-latest` apt package cannot satisfy the floor — the issue-73 design
spec records it resolving to 6.1.1 — so `apt install ffmpeg` remains unavailable to this job.

### A floor is not a compatibility statement

The documented "7.0+" says what is too old. It says nothing about whether the suite works on
an arbitrarily newer major, and nothing in the tree supplies that half:
`src/chaos_librarian/materializer/content/source_capabilities.py` gates only on
`ffmpeg_available` — there is no version parse, no upper bound, and no test exercising a
non-7.x ffmpeg.

That matters immediately rather than hypothetically. BtbN's lowest qualifying series today is
`n8.1`, so the **first run under this design moves the suite from ffmpeg 7.1 to 8.1** — a
major bump the suite has never been run against. If ffmpeg 8 changed something the
materializer depends on, the break lands as a red weekly job that reads like a product
regression; the 2026-08-10 and 2026-08-17 scheduled runs already failed inside
`Run Chaos Librarian E2E` for unrelated reasons, so that confusion is live.

This design does not assume the bump is safe. A `workflow_dispatch` run of `chaos-e2e` on the
implementing branch is what settles it, and its result is recorded in ADR 0078's Context
before the record is accepted. If ffmpeg 8 does break the suite, that is a finding about the
suite's real compatibility range — and the response is to raise the floor's upper half
deliberately, not to re-pin a series by accident.

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

Candidate assets must match, as a whole line:

```text
^ffmpeg-n(MAJOR).(MINOR)-latest-linux64-gpl-(MAJOR).(MINOR).tar.xz$
```

with the trailing `MAJOR.MINOR` equal to the one in the `n` prefix. This excludes two asset
families deliberately:

- `ffmpeg-master-latest-linux64-gpl.tar.xz` — BtbN's git-master build. Not a release series,
  and its version string carries no series to check a floor against.
- `...-gpl-shared-...` variants — the job invokes the extracted binaries directly and sets up
  no shared-library search path, so the static build is the one that works.

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
          if [ ! -s "$assets" ]; then
            echo "::error::BtbN latest release listed no assets; this is a release-read failure, not a retired series"
            exit 1
          fi
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
- **The empty-asset-list check is separate from selection.** An API outage, a rate limit, a
  token problem, and a genuine asset rename would otherwise all reach the operator as the
  same "no qualifying asset" message. Only the rename is a retired-catalogue event; the
  others are read failures, and they get their own error.
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

Two failures are kept distinct because they call for different responses.

**No asset names at all** is a release-read failure — an API outage, a rate limit, a token
problem, a changed response field. The workflow detects it before selection runs and says so.
Retrying is the response.

**Names, but none qualifying** is what the script reports: BtbN has retired every series the
project can use. Its message names the floor and how many candidates it considered. This is a
real decision for a human — raise the floor's upstream, change distributor, vendor a build —
not something the workflow should paper over.

Collapsing the two would reintroduce the defect this change is about, one layer up: an
operator handed "no qualifying asset" during a GitHub API incident would go looking at BtbN's
release page and find nothing wrong.

Exit codes: `2` for a malformed floor argument (caller error), `1` for no qualifying asset
(environment), `0` on success. This matches `scripts/check-adr-index.sh`, which reserves `2`
for its own caller-error cases.

## Threat model

The change is security-relevant: it alters how a CI job acquires a third-party executable,
and it adds an authenticated read against the GitHub API.

### Boundary inventory

**Widened — none.** No boundary in this design admits an actor who could not already reach
the equivalent one.

**Existing, restated:**

| Boundary | What crosses | Under whose control |
|---|---|---|
| BtbN release asset **contents** | An ffmpeg tarball, executed by the job | BtbN |
| BtbN release asset **names** | Text, parsed by `select-ffmpeg-asset.sh` | BtbN |
| GitHub REST API read | A release listing | GitHub |

The second row is the only one this design adds as a *parsed* input. The first row —
executing BtbN's binary — is unchanged and pre-existing; this design does not make it more
or less trusted.

### Actor model

The untrusted party is **BtbN**, the third-party build publisher. The job already executes
BtbN's ffmpeg binary against synthesized media, so BtbN is inside the job's trust boundary
for code execution and has been since `0d0c0ce1`. The design places its trust there
knowingly: `359ba425` established that no digest can pin a rolling artifact, and the
alternatives (vendoring a build, building from source, switching distributor) are recorded
as rejected in ADR 0078.

GitHub is trusted as the transport and as the API host; the job already depends on GitHub for
its own source checkout.

There is no anonymous-internet actor and no tenant actor here: `chaos-e2e` runs on
`workflow_dispatch` (write-access only) and `schedule` (repository-owned), never on
`pull_request`, so no outside contributor can trigger it.

### Control per boundary

| Boundary | Control | Leak on failure |
|---|---|---|
| Asset **names** | Whole-line anchored regex with an explicit character class; only `[0-9]` reaches arithmetic; the matched name is used as a single URL path segment, never as a shell word or a filesystem path | The name itself, in a diagnostic |
| Asset **contents** | Version floor asserted against the extracted binary's own `-version` output; TLS to `objects.githubusercontent.com` via `curl -fL` | The version string |
| GitHub API read | `GH_TOKEN` is the job token; the workflow's `contents: read` permission is unchanged and is sufficient for a public-repository read | `gh`'s own error |

The regex is the load-bearing control on the new input. Because it is whole-line anchored and
admits only `ffmpeg-n`, digits, `.`, `-latest-linux64-gpl-`, and `.tar.xz`, a hostile asset
name cannot introduce a path traversal (`/` and `.` runs are not admissible in sequence), a
shell metacharacter, or a URL that leaves the release. Extraction stays `tar -xJf` into a
job-scoped `$RUNNER_TEMP` directory, unchanged from today.

`GH_TOKEN` is passed through `env:`, never interpolated into the `run:` body, so no
template-injection surface is created.

### Explicitly out of scope

- **A malicious or compromised BtbN build.** Accepted risk, pre-existing and unchanged. The
  project has no digest it can pin (`359ba425`) and no reproducible build to compare against.
  The mitigating facts are that `chaos-e2e` is not a merge gate, holds no secrets beyond the
  job token, and runs with `contents: read`.
- **API response truncation.** If BtbN's asset count ever exceeded what the release object
  embeds, selection could see a partial list. This is not a correctness hazard: a partial
  list either still contains a qualifying series (selection succeeds, floor still enforced)
  or contains none (selection fails loudly). BtbN's `latest` release carries 49 assets as of
  2026-08-25.
- **Supply-chain review of `gh` itself.** Preinstalled on GitHub-hosted runners; already
  relied on by the `notify-failure` job in this same workflow.

## Documentation

`docs/operations/chaos-e2e.md` currently states:

> The workflow therefore installs ffmpeg/ffprobe from a pinned 7.x archive and
> verifies its checksum before running the suite.

Both halves have been false since `359ba425`, which removed the checksum verification and
the 7.x pin's stability. The sentence is corrected to describe the runtime resolution.

## Acceptance criteria

1. `.github/workflows/chaos-e2e.yml` contains no ffmpeg version series in any URL or
   filename. `rg -n 'n7\.1|n8\.1|n9\.0' .github/workflows/chaos-e2e.yml` produces no output.
2. `scripts/select-ffmpeg-asset.sh`, given the 2026-08-25 asset list on stdin and floor `7`,
   prints `ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz` and exits 0.
3. Given the same list with the `n8.1` and `n9.0` entries removed, it exits non-zero and
   prints a diagnostic naming the floor on stderr.
4. Given a list containing `ffmpeg-n10.0-latest-linux64-gpl-10.0.tar.xz` and
   `ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz`, it prints the `n9.0` asset — numeric
   comparison, not lexical.
5. Given only `master` and `-shared-` assets, it exits non-zero — neither family is a
   candidate.
6. Given a malformed floor argument, it exits `2`, distinct from the `1` in criteria 3 and 5.
7. `just select-ffmpeg-asset-selftest` exits 0 and is reached by `just ci`.
8. The workflow reports a release-read failure distinctly from a retired catalogue: with an
   empty asset list it errors naming the read, not the floor.
9. `actionlint` reports no finding on `.github/workflows/chaos-e2e.yml`.
10. `just ci` passes.
11. `docs/operations/chaos-e2e.md` describes what the workflow does.
12. ADR 0078 exists and has exactly one row in `docs/adr/README.md`
    (`just check-adr-index` passes).
13. A `workflow_dispatch` run of `chaos-e2e` on the branch completes green, proving the
    resolved `n8.1` asset downloads, extracts, satisfies the floor, **and that the Chaos
    Librarian suite passes on ffmpeg 8**. Its run URL and outcome are recorded in ADR 0078's
    Context.

Criteria 2–6 are the selftest's cases, so they are checked by criterion 7 on every commit.
Criterion 13 is the only one no local guardrail can reach.

## Verification

- `just select-ffmpeg-asset-selftest`
- `just check-adr-index && just check-adr-index-selftest`
- `actionlint`
- `zizmor .github/workflows/chaos-e2e.yml`
- `shellcheck scripts/select-ffmpeg-asset.sh scripts/select-ffmpeg-asset-selftest.sh`
- `just ci`
- Manual `workflow_dispatch` of `chaos-e2e` on the branch. This is the only end-to-end proof
  that the resolved asset downloads, extracts, satisfies the floor, **and that the suite
  passes on ffmpeg 8** — the major bump this design's first selection causes. No local
  guardrail contacts BtbN.

## Related

- ADR 0078 — runtime-resolved ffmpeg release series (this change's decision record)
- ADR 0033 — records the BtbN rolling-build problem as the reason the toxiproxy harness
  chose differently
- `docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md` — the original
  chaos-e2e workflow design, source of the 7.0+ floor and the apt-is-insufficient finding
