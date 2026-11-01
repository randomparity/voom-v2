# 0125 — Attached pictures are remux attachments

## Status

Accepted (2026-10-30). Implemented by #820.

## Context

ADR 0040 maps ffprobe `codec_type = "attachment"` streams to remux attachments, but ffprobe
reports image attachments differently: cover art arrives as a `video` stream with
`disposition.attached_pic = 1`. A Matroska cover also carries `filename`/`mimetype` tags; an
MP4 `covr` carries none. mkvmerge lists both kinds in its `attachments` array. Remux planning
treated covers as video, so the mkvtoolnix worker correctly refused the default and forced
flag lists it was sent (#820).

## Decision

1. The ffprobe normalizer records `disposition.attached_pic` and writes the key only when it
   is true. A missing key means "not an attached picture, or probed before this change".
2. `voom-plan`'s remux `stream_facts`, shared by the planner and the control-plane request
   builder, classifies a `video`-kind stream as `TrackTarget::Attachment` when `attached_pic`
   is true, or when the stream has an image codec (`mjpeg`, `png`, `gif`, `tiff`), a `filename`,
   and an `image/` `mime_type`. Those are the only codecs ffmpeg 8.1.3 assigns to Matroska
   image attachments, and every Matroska attachment carries both tags, so this rule recovers
   covers from snapshots taken before this change. An attached picture is never a font.
3. The request builder refuses a selection whose default, clear-default, forced,
   clear-forced or head list names an attachment, before dispatch.

## Consequences

- Covers are kept or removed only by attachment track actions; `remove attachment where not
  font` drops them.
- The stored snapshot shape changes only for files with cover art. Older snapshots stay
  valid.
- An MP4 cover in a snapshot ingested before this change still reads as video. A rescan
  of an unchanged file writes no new snapshot, so that file fails at the worker, as before,
  until its content changes. A re-probe or backfill path is follow-up work (#825).
- The tag fallback trusts the media file's own tags. If a file's author gives a real
  image-codec video track (for example MJPEG) a filename and an image MIME type, remux
  treats that track as an attachment, and an attachment-removal policy drops it, in that
  file only. A file with no other video track is refused.
- Readers outside remux that count `kind == "video"` streams (`video_stream_count`,
  first-video lookups) keep the raw ffprobe kind.

## Considered & rejected

- **Do nothing; rely on the worker refusal.** judgment: every file with cover art stays
  unremuxable, and that is the defect itself.
- **Rewrite `kind` to `attachment` in the normalizer.** judgment: it changes every snapshot
  reader's view and stops the stored kind from matching what the provider reported.
- **Emit `attached_pic` for every stream.** verified: `ffprobe -show_streams -of json`
  (ffprobe 8.1.3, Linux) reports the key on every stream of a synthetic MKV and MP4, so every
  new snapshot and every fixture that pins normalized dispositions would change, with no
  planning gain.
- **Fall back to `raw.ffprobe_json` for old snapshots.** verified: ADR 0122 records that
  planning never reads `raw`, and `raw` is omitted above the snapshot bound.
- **Classify on tags alone.** verified: an H.264 track muxed with
  `ffmpeg -metadata:s:v:0 filename=poster.png -metadata:s:v:0 mimetype=image/png` probes as
  `video` with both tags (ffprobe 8.1.3), while mkvmerge 102 reports a video track. A
  tag-only rule would demote it, so the fallback also requires an image codec.
- **Drop attachments in the mkvtoolnix worker.** judgment: the worker's refusal is the
  correct last gate, and the planner owns what it plans.
- **Infer covers from the codec (`mjpeg`/`png`).** judgment: Matroska carries real MJPEG
  video tracks.
