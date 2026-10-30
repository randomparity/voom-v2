# Attached pictures are remux attachments (#820)

Decision record: [ADR 0125](../../adr/0125-attached-pictures-are-remux-attachments.md).

## Problem

ffprobe reports cover art as `codec_type: video` with `disposition.attached_pic = 1`; the
Matroska form also has `filename`/`mimetype` tags, the MP4 form has none. mkvmerge lists both
as attachments. The normalizer drops `attached_pic`, and `stream_facts` (`voom-plan` remux
selection) maps the stream to `TrackTarget::Video`. The request builder then puts the cover's
`default=false` in `clear_default_streams` and `forced=false` in `clear_forced_streams`; the
worker refuses both, so the remux fails.

## Scope

- **Normalizer:** `insert_disposition` parses `attached_pic` like `default` and writes it only
  when true. Snapshots of files without cover art are unchanged, and older snapshots stay valid.
- **Classification:** `stream_facts` maps a `video`-kind stream to `Attachment` when
  `disposition.attached_pic` is true or a `mime_type`/`filename` fact is present. Planner and
  request builder already share `stream_facts`, so no caller migrates.
- **Invariant:** `selection_from_payload_and_snapshot` refuses, as a config error naming the
  list, any default, clear-default, forced, clear-forced or head list holding an
  attachment-kind stream.
- Unchanged: mkvtoolnix worker (#819); node agent, API, worker protocol (#817); readers
  of `kind == "video"` outside remux (ADR 0125 consequences).

### Failure model

- Actors and deployments: the control plane planning remux for scanned libraries; the
  owner-node ffprobe worker producing snapshots.
- Invariants and assets: persisted snapshot shape (additive, tolerated absent); worker
  attachment-list validation; ADR 0122's 60 KiB snapshot bound.
- Accepted: MP4 covers in snapshots probed before this change read as video until re-probed;
  they fail closed at the worker as today.
- Covered elsewhere: worker validation remains the last gate (ADR 0040).

## Success

1. Selections from an MKV-shaped and an MP4-shaped cover snapshot under the #794 policy
   succeed, keep the cover, and hold no attachment in the five flag/head lists.
2. Normalized output carries `attached_pic: true` only for attached pictures and stays
   within the snapshot bound.
3. Snapshot readers in `voom-plan` and the control plane accept the new key.

## Validation

- Normalizer — `focused-test`, `normalize_test.rs`: 1 yields `true`, 0 is omitted, 2 is
  malformed; red: key missing; green: `cargo test -p voom-ffprobe-worker normalize` (ADR
  0122 bound tests included).
- Classification — `focused-test`, `voom-plan` `selection_test.rs`: video with
  `attached_pic`, `mime_type`, or `filename` is Attachment; plain video stays Video; red:
  `Video`; green: `cargo test -p voom-plan remux_stream_facts`.
- Request builder — `focused-test`, control-plane `selection_test.rs`: MKV and MP4 cover
  snapshots under the #794 policy; red: cover in `clear_default_streams`; green:
  `cargo test -p voom-control-plane remux::selection`.
- Invariant — `focused-test`: the guard unit-tested with an attachment in a flag list;
  red: no error; green: as above.
- Consumer sweep — `task-test-not-applicable`: the other disposition readers read named
  keys only; evidence is listed in the PR.
