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

### This is the third recurrence

| Commit | Approach | How it broke |
|---|---|---|
| `0d0c0ce1` | Pin a dated `autobuild-*` tag + SHA-256 | BtbN garbage-collects dated tags; 404 |
| `7212262f` | Re-pin to a newer dated tag + SHA-256 | Same garbage collection; 404 again |
| `359ba425` | Rolling `latest` asset, pinned to series `n7.1` | BtbN retired the `n7.1` series; 404 |

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

Choose the **lowest** published release series whose major version meets the floor.

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
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          # Chaos Librarian documents ffmpeg/ffprobe 7.0+ as its floor
          # (third_party/chaos-librarian/README.md). BtbN retires release series,
          # so the series is resolved at runtime rather than named here; see
          # docs/adr/0078-runtime-resolved-ffmpeg-series.md.
          FFMPEG_MAJOR_FLOOR: "7"
          FFMPEG_RELEASE_BASE_URL: https://github.com/BtbN/FFmpeg-Builds/releases/download/latest
        run: |
          asset=$(
            gh api repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name' \
              | ./scripts/select-ffmpeg-asset.sh "$FFMPEG_MAJOR_FLOOR"
          )
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

Two shell details are deliberate:

- **`shell: bash`**, which GitHub runs as `bash --noprofile --norc -eo pipefail {0}`. The
  default `run:` shell is `bash -e` *without* `pipefail`, under which a failing `gh api`
  would hand the selection script empty input and surface as "no qualifying asset" —
  a true failure reported with the wrong cause. `pipefail` makes `gh`'s own error the
  failure. `shell: bash` is scoped to this one step; no other step changes.
- **No `| head -n1`** on `ffmpeg -version`. Under `pipefail`, closing the pipe after one
  line can reap `ffmpeg` with `SIGPIPE` (exit 141) and fail the step depending on how much
  of ffmpeg's output fits in the pipe buffer — a flake that would appear only sometimes.
  Bash's `${var%%$'\n'*}` takes the first line with no pipe at all. The major version is
  extracted with `[[ =~ ]]` for the same reason.

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

`select-ffmpeg-asset.sh` exits non-zero with a message on stderr when no candidate meets the
floor, naming the floor and how many candidates it considered. That is the signal that BtbN
has retired every series the project can use — a real decision for a human (raise the floor's
upstream, change distributor, vendor a build), not something the workflow should paper over.

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
5. `just select-ffmpeg-asset-selftest` exits 0 and is reached by `just ci`.
6. `actionlint` reports no finding on `.github/workflows/chaos-e2e.yml`.
7. `just ci` passes.
8. `docs/operations/chaos-e2e.md` describes what the workflow does.
9. ADR 0078 exists and has exactly one row in `docs/adr/README.md`
   (`just check-adr-index` passes).

Criteria 2–4 are the selftest's cases, so they are checked by criterion 5 on every commit.

## Verification

- `just select-ffmpeg-asset-selftest`
- `just check-adr-index && just check-adr-index-selftest`
- `actionlint`
- `zizmor .github/workflows/chaos-e2e.yml`
- `shellcheck scripts/select-ffmpeg-asset.sh scripts/select-ffmpeg-asset-selftest.sh`
- `just ci`
- Manual `workflow_dispatch` of `chaos-e2e` on the branch. This is the only end-to-end proof
  that the resolved asset downloads, extracts, and satisfies the floor, because no local
  guardrail contacts BtbN.

## Related

- ADR 0078 — runtime-resolved ffmpeg release series (this change's decision record)
- ADR 0033 — records the BtbN rolling-build problem as the reason the toxiproxy harness
  chose differently
- `docs/superpowers/specs/2026-05-25-issue-73-chaos-e2e-actions-design.md` — the original
  chaos-e2e workflow design, source of the 7.0+ floor and the apt-is-insufficient finding
