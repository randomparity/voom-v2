# 0129 — Preserve cover art during video transcoding

## Status

Accepted (2026-11-01). Implements #833's operator-approved preservation/explicit-block choice.

## Context

Attached pictures are provider video streams, but are not primary video. ADR0125 already
classifies them for remux. Video projection, planning and ffmpeg must agree while retaining
exactly-one-primary rejection. Ordinary Matroska video mapping loses attachment identity.

## Decision

Reuse ADR0125's normalized cover classification for primary planning facts. Re-probe actual
worker inputs, exclude attached pictures from primary selection/count, and map `0:V:0`.
Preserve JPEG/PNG/GIF/TIFF covers as MKV attachments via stream-copy extraction into owned
private scratch storage and `-attach`. Preserve JPEG/PNG as copied MP4 attached pictures.
Scope video encoding/filter/tag options to the primary and verify cover bytes/count/tags
before success. Explicitly block unsupported combinations before transcoding. Keep the
request/snapshot wire shapes and source identity checks unchanged.

## Consequences

Cover-bearing files can be planned and executed without silent picture loss. MKV targets
need bounded subprocess extraction and temporary disk space proportional to cover bytes.
Reuse the workspace tempfile dependency for cleanup; hard process termination follows the
existing staging-retention policy. Historical MP4 snapshots missing cover facts still need
#825's independent refresh. This extends ADR0125's raw-kind-reader consequence only for the
projection/video-transcode paths; other readers are unchanged.

## Considered & rejected

- **Do nothing or count covers as primary video.** judgment: contradicts #833's outcome.
- **Exclude covers and keep the old command.** verified: source at c1619624 maps only `0:v:0`,
  which selects a cover when it precedes the primary and drops other covers.
- **Map cover video directly into MKV.** verified: on FFmpeg8.1.3/Linux, a generated MKV
  attachment copied using `-map 0:v -c:v:0 libx265 -c:v:1 copy -disposition:v:1 attached_pic`
  probes with cover attached_pic0 instead of1. Raw stream-copy extraction using
  `-map 0:1 -c copy -frames:v 1 -f data cover.jpg` preserves exact generated JPEG/PNG/GIF/TIFF bytes on FFmpeg8.1.3/Linux.
  The image2 muxer changes GIF frame-delay bytes; the data muxer copies the original packet.
  Regression hashes compare with generated originals rather than two re-extractions.
- **Block every cover-bearing file.** judgment: loses supported MKV/MP4 transformations
  despite the operator's preservation requirement.
- **Create a scratch-lifecycle helper.** judgment: tempfile already supplies owned cleanup;
  promoting the installed workspace dependency is smaller than another filesystem abstraction.
