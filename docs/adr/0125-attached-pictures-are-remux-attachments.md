# 0125 — Attached pictures are remux attachments

## Status

Accepted (2026-10-30). Implemented by #820.

## Context

ADR 0040 maps ffprobe `codec_type = "attachment"` streams to remux attachments. ffprobe does
not report image attachments that way: it exposes cover art as a `video` stream with
`disposition.attached_pic = 1`. A Matroska cover also carries `filename`/`mimetype` tags, and
an MP4 `covr` carries none. mkvmerge lists both in its `attachments` array. Remux planning
therefore treated covers as video, and the mkvtoolnix worker correctly refused the resulting
default and forced flag lists (#820).

## Decision

1. The ffprobe normalizer records `disposition.attached_pic` and writes the key only when it is
   true. Absence means "not an attached picture, or probed before this change".
2. `voom-plan`'s remux `stream_facts`, shared by the planner and the control-plane request
   builder, classifies a `video`-kind stream as `TrackTarget::Attachment` when
   `attached_pic` is true or the stream carries a `mime_type` or `filename` fact.
3. The request builder refuses a selection whose default, forced or head lists name an
   attachment, before dispatch.

## Consequences

- Covers are kept or removed only by attachment track actions. `remove attachment where not
  font` drops them.
- The stored snapshot shape changes only for files with cover art. Snapshots of other files
  are unchanged, and older snapshots remain valid.
- MP4 covers probed before this change still read as video until the file is re-probed. They
  fail at the worker as before, rather than being misplanned.
- Readers outside remux that count `kind == "video"` streams (`video_stream_count`,
  first-video lookups) keep the raw ffprobe kind. This decision does not change them.

## Considered & rejected

- **Rewrite `kind` to `attachment` in the normalizer.** judgment: it changes every snapshot
  reader's view and stops the stored kind from matching the provider's report, which is more
  blast radius than the remux defect needs.
- **Emit `attached_pic` for every stream.** verified: ffprobe 8.1.3 reports the key on every
  stream (`ffprobe -show_streams -of json` on a synthetic MKV and MP4), so this would change
  every new snapshot and every fixture that pins normalized dispositions, for no planning
  gain.
- **Drop attachments from flag lists in the mkvtoolnix worker.** judgment: the worker's
  refusal is the correct last gate; the planner owns what it plans.
- **Infer covers from the codec (`mjpeg`/`png`).** judgment: Matroska carries real MJPEG
  video tracks, so a codec rule misclassifies them.
