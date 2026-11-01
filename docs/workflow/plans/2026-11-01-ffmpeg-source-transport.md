# FFmpeg source transport — implementation plan (#814)

Goal: the PCM container build obtains its pinned FFmpeg inputs without ffmpeg.org.
Architecture: a host-side `fetch-sources.sh` tries verified URLs from `source-urls`; `build.sh`
uses it when no source directory is given; CI caches the verified directory.
Spec: `docs/workflow/specs/2026-11-01-ffmpeg-source-transport-design.md`. No ADR.
Tech stack: bash, Python 3 unittest, GitHub Actions.

Expected implementation size: 120–160 changed lines (S) — ~40 script, ~10 `build.sh`, −8
Dockerfile, ~25 workflow, ~5 justfile, ~45 tests, ~20 docs.

## Global Constraints

- `source-sha256`, FFmpeg version, `pcm-wavex.patch`, configure flags and the Dockerfile's
  digest and `VALIDSIG` checks are unchanged.
- Not touched: #820 crates; `scripts/media-evidence-*`, `docs/media-evidence-*` (#794).
- Actions pinned by full SHA: `actions/cache/restore` and `actions/cache/save` at
  `55cc8345863c7cc4c66a329aec7e433d2d1c52a9  # v6.1.0` (released 2026-06-26).
- Guardrails under `flock -s .agent/corpus.lock`: `just container-unit-check`, then `just ci`;
  workflow edits also `actionlint` and `zizmor .github/workflows/`.

## File map

| File | Change |
|---|---|
| `containers/ffmpeg/source-urls` | new: `<file> <https-url>` lines, preference order |
| `containers/ffmpeg/fetch-sources.sh` | new: verify-or-fetch into DEST |
| `containers/ffmpeg/build.sh` | no SOURCES: fetch into `$context/sources` |
| `containers/ffmpeg/Dockerfile` | drop the curl branch and `curl` package |
| `containers/ffmpeg/build_test.py` | fetch tests with stub `curl` |
| `justfile` | `container-sources DIR`; lint the new script in `container-unit-check` |
| `.github/workflows/ci.yml` | cache restore, fetch, save on miss, build with DIR |
| `containers/ffmpeg/README.md`, `docs/specs/pcm-container-audio.md` | describe transport |

## Task 1 — fetch script and tests

1. Write failing tests in `build_test.py` (`FetchSourcesTests`). Each copies
   `fetch-sources.sh` into a temporary recipe directory with synthetic `source-sha256`
   (names `a`, `b`) and `source-urls`, and puts a stub `curl` first on `PATH`. The stub
   appends the URL to a log, writes the served file for that URL to the `-o` path, or exits 22.
   Cases: first URL missing, second good → installed; every URL tampered → exit 1, stderr
   names `a`, no `a` or partial in DEST; valid DEST files → zero stub calls; tampered and
   symlinked DEST entries → replaced by the served bytes.
2. Implement `fetch-sources.sh`: `set -euo pipefail`; require a DEST directory; digest via
   `sha256sum` or `shasum -a 256`; downloads go to a `mktemp -d` directory removed by an EXIT
   trap, never into DEST; bash 3.2-compatible (`while read` loops, no `mapfile`);
   curl `--fail --silent --show-error --location --proto =https --proto-redir =https
   --connect-timeout 20 --max-time 300 --retry 2`.
3. Add `source-urls`; add `bash -n` and shellcheck for the script to `container-unit-check`
   and a `container-sources DIR` recipe. Run `just container-unit-check`. Commit.

## Task 2 — build path and CI

1. `build.sh`: replace the empty-SOURCES case with a fetch into `$context/sources`.
   Dockerfile: remove the curl branch and package; always copy from `source-inputs/`.
2. `ci.yml` pcm-container: restore (id `ffmpeg-sources`, path `${{ runner.temp }}/ffmpeg-sources`,
   key `ffmpeg-sources-${{ hashFiles('containers/ffmpeg/source-sha256') }}`), `mkdir -p` and
   `just container-sources`, save on `cache-hit != 'true'`, build with the directory.
3. Fetch the real inputs locally with `just container-sources` while ffmpeg.org is down and run
   `just container-build docker` once without SOURCES (proves the context path end to end).
4. Update README (host needs `curl` and `sha256sum`/`shasum`) and every PCM-spec passage on
   input transport (the pinned-archive bullet and the online/prefetched transport paragraph).
   Run actionlint, zizmor, `just ci`. Commit, push. Record the first `pcm-container` run
   (cache miss): its URL, whether ffmpeg.org was reachable, and the URL that served each file. That run, not a later cache-hit run, is the criterion 1 evidence.
