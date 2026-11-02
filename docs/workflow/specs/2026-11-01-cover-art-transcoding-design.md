# Cover art during video transcoding (#833)

## Problem and authority

One primary video plus an attached picture is counted as two videos. Projection and worker
probing also select the first video, which can be the cover. The command maps `0:v:0`.
Issue #833 and the operator-approved campaign direction require excluding covers, preserving
supported covers, and explicitly blocking unsupported cases; genuine multiple videos remain
unsupported. Exact exclusions are empty. #787 and #339 require source preservation and
explicit classification. ADR [0129](../../adr/0129-cover-art-transcoding.md) records the choice.

## Architecture and data flow

Retain existing ownership: control-plane projection derives planning facts, the planner decides
support, the worker probes actual source bytes and implements preservation. No schema, wire,
identity-verification, or worker capability change is needed. Expose the existing ADR0125 cover
classifier from remux selection and reuse it for primary snapshot counting/selection and
`video_stream_field`. Its bool disposition and bounded legacy image-tag fallback remain intact.
Do not normalize persisted stream kinds or alter attachment-removal policy.

Newly ingested cover facts work independently of #825's legacy re-probe work. Legacy Matroska
image tags retain ADR0125 classification. Historical MP4 facts without attached_pic stay
conservative; only the live worker can recognize their omitted fact.

The worker filters attached pictures from primary facts, requiring exactly one primary video.
Use FFmpeg's `0:V:0` selector to map that primary independent of cover order. Enumerate covers and generic attachment count
from the same real input probe with checked indices and recognized image codecs. Missing or
malformed cover indices/codec facts are explicit errors. Worker validation remains authoritative
when planning facts are old or requests bypass the planner.

For MKV targets, extract each cover with stream-copy `-map 0:INDEX -c copy -frames:v 1
-f data PATH`, using controlled paths in an owned private temporary directory beneath the
staging root. Attach those bytes using `-attach PATH`, preserving observed filename, MIME type,
and title; use deterministic cover names and codec-derived MIME only when source lacks them.
Retain existing generic attachment mapping. For MP4 targets, copy supported JPEG/PNG covers
as video streams after the primary, with explicit per-cover codec copy and attached_pic.
Keep encoder/filter/tag options scoped to primary `v:0` so cover packets cannot be encoded,
scaled, or assigned an HEVC/AV1 sample-entry tag. MP4 generic attachments and non-JPEG/PNG
covers are blocked. MKV supports JPEG/PNG/GIF/TIFF attached pictures; other cover codecs block.
Unsupported cover shapes block before extraction/transcoding, and planning emits the existing
UnsupportedMediaShape diagnostic when these facts are available. Compliant no-op files do not
need cover conversion. Existing MP4 non-cover compatibility checks remain intact.

After output probing, compare cover count, codec and cover representation, and compare copied
raw packet bytes against the raw extracted source using streaming file observation hashes.
Regression oracles are the original generated JPEG/PNG/GIF/TIFF bytes (including animated GIF),
so applying the same transformation on both sides cannot hide a source-packet change. MKV filename,
MIME and title must retain source facts; MP4 representation permits the container's omission of
attachment tags. Output primary facts exclude covers. Failed preservation returns an error,
preventing promotion. Temporary files are dropped on success/error/cancellation, and explicit
close surfaces cleanup failure on the completed path. Each subprocess has the existing process
timeout and kill-on-drop. Extraction runs sequentially per cover, with no new worker concurrency.

Reuse workspace tempfile3.27.0 as a normal worker dependency for owned scratch cleanup, removing
its redundant dev declaration. Current upstream version and API were checked at
https://docs.rs/tempfile/latest/tempfile/struct.TempDir.html; no version or lockfile update.

## Success

- A newly ingested snapshot with cover first projects primary codec/dimensions and count1.
- An HEVC transcode is planned with one primary plus supported covers; two genuine primaries
  remain blocked, even when covers are also present.
- Generated MKV/MP4 sources transcode to MKV/MP4 targets for supported cover shapes; cover bytes,
  counts and applicable tags survive, primary output is HEVC, and source hash is unchanged.
- Unsupported cover/container shapes fail explicitly before producing an output.
- Copy-video and ordinary cover-free command behavior remain valid.

## Failure model

1. Actors and deployments: local operators, authenticated owner-node execution, Linux/macOS CI;
   media authors control streams/tags, planner input, and the actual probed media structure.
2. Invariants/assets: source bytes, cover bytes/identity/count, one-primary contract, primary
   codec/dimensions, staging containment and promotion only after a successful verified result.
3. Accepted failure classes: unsupported image codecs/container combinations are explicit blocked
   outcomes; hard process termination may leave task staging files under existing cleanup policy.
   Historical snapshots missing MP4 cover facts remain conservative until #825 refreshes them.
4. Covered elsewhere: #825 owns reprobe/backfill; existing process timeout and kill-on-drop own
   subprocess lifetime; existing staging validation and expected/pre/post hashes own path/source
   identity; #339 owns actual library acceptance, not this synthetic regression run.

## Threat model

- Boundary inventory: existing persisted snapshot→planning and external ffprobe→worker parsing;
  image extraction and attach argv consume media-controlled metadata. No entry point is added.
- Actors: media authors are untrusted; authenticated operator supplies staging roots; configured
  executables are trusted. A compromised local executable is outside this change's authority.
- Controls: checked integer cover indices; supported codecs/container validation before writes;
  argument-array subprocesses, generated scratch basenames, private owned directories; existing
  path validation, no-overwrite, hash verification, timeout and kill-on-drop remain in force.
- Outside scope: author-forged legacy Matroska image tags retain ADR0125's accepted classification
  tradeoff; external binary compromise and production corpus execution have their existing owners.

## Validation

Focused projection/planner/worker tests precede implementation. Real FFmpeg conformance tests
use disposable generated media and assert copied image hashes, primary facts, source hash,
unsupported rejection, and genuine multi-video rejection. Run focused affected-crate tests,
`just fmt-check`, `just lint`, mandatory commit hooks and one final `just ci` before pushing.
No cached project code or Cargo runs occur without the campaign's exclusive target lease.
