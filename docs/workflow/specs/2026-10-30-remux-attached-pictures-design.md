# Attached pictures are remux attachments (#820)

Decision record: [ADR 0125](../../adr/0125-attached-pictures-are-remux-attachments.md).

## Problem

ffprobe reports cover art as `codec_type: video` with `disposition.attached_pic = 1`; the
Matroska form also has `filename`/`mimetype` tags, the MP4 form has none. mkvmerge lists both
as attachments. The normalizer drops `attached_pic`; `stream_facts` (`voom-plan` remux) maps
the stream to Video, so the request builder puts the cover's flags in `clear_default_streams`
and `clear_forced_streams`, which the worker refuses.

## Scope

- **Normalizer:** `insert_disposition` parses `attached_pic` like `default`, writing it only
  when true; other snapshots are unchanged and older ones stay valid.
- **Classification:** `stream_facts` maps a `video`-kind stream to Attachment when
  `attached_pic` is true, or it has a `filename` and an `image/` `mime_type` (pre-change
  Matroska snapshots). It records `attached_picture`; `font` is false for an attached
  picture, never blocking. Planner and request builder share `stream_facts`.
- **Invariant:** the flag lists exist only in the control-plane builder, so the guard lives
  there: it destructures `RemuxSelection` without `..` and refuses an attachment-kind stream in
  the five default/forced/head lists as a config error naming the list. A new field fails
  to compile until classified.
- Unchanged: mkvtoolnix worker (#819); node agent, API, worker protocol (#817); readers
  of `kind == "video"` outside remux (ADR 0125 consequences).

### Failure model

- Actors and deployments: the control plane planning remux for scanned libraries; the
  owner-node ffprobe worker producing snapshots.
- Invariants and assets: persisted snapshot shape (additive, tolerated absent); worker
  attachment-list validation; ADR 0122's 60 KiB snapshot bound.
- Accepted: MP4 covers in snapshots ingested before this change stay video, and fail
  closed at the worker as today, until the file's content changes. No re-probe path exists
  for unchanged files, and planning may not read `raw` (ADR 0122); follow-up #825.
- Covered elsewhere: worker validation remains the last gate (ADR 0040).

## Success

1. Selections from new-snapshot MKV- and MP4-shaped cover snapshots under the #794 policy, and
   under `remove attachment where not font`, succeed, and none of the five lists names the cover.
2. Normalized output carries `attached_pic: true` only for attached pictures, within the bound.
3. A repeat scan of an unchanged file records no new snapshot.

## Validation

- Normalizer — `focused-test`, `normalize_test.rs`: 1 → `true`, 0 omitted, 2 malformed, on
  captured ffprobe 8.1.3 JSON; red: key missing; green: `cargo test -p voom-ffprobe-worker`
  (ADR 0122 bound tests included).
- Classification — `focused-test`, `voom-plan` `selection_test.rs`: `attached_pic`, or
  `mime_type`+`filename`, → Attachment and not font; plain video and filename-only video stay
  Video; red: `Video`; green: `cargo test -p voom-plan remux`.
- Request builder and invariant — `focused-test`, control-plane `selection_test.rs`: both
  cover shapes under both policies, plus the guard given an attachment in each list; red: cover
  in `clear_default_streams`; green: `cargo test -p voom-control-plane remux::selection`.
- Repeat scan — `task-test-not-applicable`: unchanged; identity resolves by content facts and a
  snapshot is written only in `ingest_new_scanned_file` (`scan/publish.rs`).
- Consumer sweep — `task-test-not-applicable`: `rg '"disposition"'` readers (audio
  publication, audio selection, ffmpeg worker) read named keys.
