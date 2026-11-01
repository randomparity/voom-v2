# FFmpeg source inputs survive an ffmpeg.org outage (#814)

## Problem

The PCM container build downloads `ffmpeg-9.0.2.tar.xz`, its `.asc` and
`ffmpeg-devel.asc` from ffmpeg.org inside the image build. When ffmpeg.org is
unreachable (2026-10-30 onward: curl 35 and connect timeouts), the
"PCM container (Linux amd64)" job fails before any repository code runs.

## Scope

Trust stays where it is: the three `source-sha256` digests and the
`VALIDSIG FCF986EA15E6E293A5644F10B4322F04D67658D8` check in the Dockerfile.
Only the transport changes, and every transport is untrusted.

- `containers/ffmpeg/source-urls` lists, per pinned file, HTTPS URLs in
  preference order. A pin update edits it with `source-sha256`.
- `containers/ffmpeg/fetch-sources.sh DEST` makes DEST hold the three pinned
  files. A regular, non-symlink file whose digest matches is kept without
  network access. Anything else at that name is removed and the URLs are tried
  in order; a download lands in a private temporary directory outside DEST
  and is moved in only after its digest matches. A mismatch or HTTP failure
  moves to the next URL; when none yields the pinned bytes it exits 1 naming
  the file. DEST never holds a partial download. The script runs on bash 3.2
  (stock macOS): no `mapfile`, no empty-array expansion under `set -u`.
- `build.sh` without a source directory fetches into the build context with
  that script; the in-image `curl` branch and the `curl` build package are
  removed, so the image build has one input path: the supplied `sources/`.
  A supplied directory keeps its existing pre-engine validation.
- `just container-sources DIR` exposes the fetch for prefetching on a
  connected host (the README's offline path).
- CI restores `actions/cache` keyed on `hashFiles(source-sha256)`, runs
  `just container-sources` (verify-only on a hit), saves the cache on a miss
  before the image build, and builds with `just container-build docker DIR`.

URL order: snapshot.debian.org at the Dockerfile's existing snapshot timestamp
(`20261027T000000Z`) for the tarball and signature — the Debian
`ffmpeg_9.0.2.orig.tar.xz{,.asc}` are byte-identical to the pins, and the
build already depends on that host for packages — then ffmpeg.org. The key:
ffmpeg.org, then the Internet Archive capture `20260902212154id_` of
`https://ffmpeg.org/ffmpeg-devel.asc`. All three non-ffmpeg.org URLs were
fetched on 2026-11-01 while ffmpeg.org timed out; each matched its pin, and
GnuPG reported the `VALIDSIG` above.

Rejected:

- Cache alone: a cold cache (7-day unused eviction, new pins) still needs
  ffmpeg.org, which is the failure being fixed.
- A release asset on this repository: an outward write per pin update and a
  second store to keep in step; the cache plus verified mirrors meet the
  criteria without it.
- The signed GitHub tag `n9.0.2`: a different signing key and archives not
  byte-identical to the pins (issue #814), so it changes the trust anchor.

No ADR: the trust anchor is unchanged.

## Failure model

1. Actors and deployments: the `pcm-container` CI job on ubuntu-latest (push
   to main, same-repo and fork PRs); an operator running
   `just container-build` or `just container-sources` on Linux or macOS.
2. Invariants: no FFmpeg bytes reach `configure` unless their digests match
   `source-sha256` and the signature verifies with the pinned fingerprint.
3. Accepted: the build fails when the cache is cold and every URL for a file
   is unreachable or serves other bytes — fail closed, naming the file. A
   tampered cache entry costs a refetch, not trust. Host builds now need
   `curl` and `sha256sum` or `shasum` (present on the supported hosts).
4. Covered elsewhere: Debian package availability (existing snapshot pins);
   FFmpeg version, patch and configure flags (unchanged).

### Threat model

- Boundaries: every URL host and the Actions cache deliver bytes (added:
  snapshot.debian.org and web.archive.org for these files, the cache). The
  cache scope is written only by this repository's workflow runs.
- Actors: a compromised or hijacked mirror, cache entry or network path.
- Controls: digest match before a file is kept or installed; the Dockerfile
  repeats digests and GPG fingerprint before extraction; HTTPS-only
  (`--proto =https --proto-redir =https`); bounded `--max-time`.
- Out of scope: compromise of the FFmpeg release key itself.

## Success

1. With ffmpeg.org unreachable and a cold cache, the fetch obtains all three
   pinned files and the PCM job passes.
2. A source that serves wrong bytes is skipped; when no source serves the
   pinned bytes the fetch exits non-zero, naming the file, leaving it absent.
3. Valid files already in DEST cause no download; tampered or symlinked
   ones are replaced.
4. `just container-build ENGINE [SOURCES]` keeps its interface; the README
   and the PCM spec describe the new transport.

## Validation

- Criteria 2–3 — Mode: focused-test. `build_test.py` runs `fetch-sources.sh`
  from a temporary recipe with synthetic pins and a stub `curl` on `PATH`.
- Criterion 1 — Mode: CI. The PR's first `pcm-container` run (cache miss;
  later runs restore the PR-scoped entry) while ffmpeg.org is unreachable;
  its log shows which URL served each file.
- Criterion 4 — Mode: review, plus the existing `build.sh` rejection test.
