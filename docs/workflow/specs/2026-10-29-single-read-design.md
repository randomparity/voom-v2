# Single-read hash and probe benchmark (#801)

## Problem

Stage 5 of the frozen [protocol](../../media-evidence-protocol.md) ("combined owner-side hash
and probe in one pass", local full pass ×3) has no result. Today each file is read in full
three times: the hash worker's pass, then the probe worker's BLAKE3 re-reads before and after
ffprobe (`crates/voom-ffprobe-worker/src/handler.rs`, `probe_file`). Production stays
unchanged; #791 decides.

## Scope

- `scripts/media-evidence-candidates.py`: a variant becomes edits keyed by source file, so a
  variant may edit the probe worker as well as the hash worker. Edits still apply exactly once,
  in memory, before the detached worktree exists. New variant `single-read` = the #789 stage-2
  selection `buf1m` plus a probe-worker edit: `probe_file` replaces both `observe_file_facts`
  calls with a stat-only check (no-follow `symlink_metadata`, regular file, size and
  nanosecond mtime equal to the expected facts, pre equal to post). The stat result echoes the
  expected content hash, so the probe worker reads no file bytes; ffprobe's own reads remain.
  The edit exists only in the bench worktree.
- One full read per file is the owner-side hash pass. Piping those bytes into ffprobe is
  excluded (operator), so "single read" here means one full read plus ffprobe's own reads.
- The #788 harness runs unchanged; its records keep `reads_per_file: 3` and count
  `logical_bytes_read` as three reads. The candidate script and summary record the variant's
  full reads per file (1 vs 3) and report physical bytes, which the harness measures, as the
  I/O result.
- Cells, 3 repetitions, concurrency 4: local corpus, evicted, all files; network subset
  `f0049,f0074,f0063` as-found if the rehearsal budget allows, else `not_measured`.
- `compare` against the #788 record and the #789 `stage1-buf1m` record (and
  `stage2-c4-network` for the network cell). Records and summary live in
  `docs/media-evidence-baseline/single-read/`; `stages-1-2.md` gets a one-line pointer.

### Failure model

1. Actors: one operator on one Linux host; local corpus; network corpus shared with a live
   rehearsal; the self-test in CI.
2. Invariants: corpora read-only; production crate sources unchanged on the branch;
   committed files carry no host path or name; no captured number invented or dropped.
3. Accepted: the variant weakens detection, by design. A stat-only pre/post check cannot see
   a same-size edit with mtime restored during the probe (O10/O11 shape). This is reported
   as a separate correctness finding for #791, never netted against the speed result.
   ctime evidence is #798's; the summary cites it.
4. Elsewhere: production and defaults #791; fingerprints and lazy lifecycle #796/#797;
   reuse and ctime #798; harness changes #806.

## Success

1. `prepare single-read` builds both workers with the edits; each record's
   `git_changed_paths` names both edited files.
2. Local cell records committed and validated by the #788 validator; summary gives `compare`
   verdicts against #788 and `stage1-buf1m`, reads per file 1 vs 3, and ffprobe reads as not
   measured.
3. The correctness finding stands in its own section.
4. No crate source changes on the branch.

## Validation

- Variant edits — focused-test: every variant applies exactly once to current sources; a
  drifted probe source fails before any worktree; `single-read` contains `buf1m`'s hash edits
  and removes both full re-reads from `probe_file`.
- Variant build — integration: `prepare single-read` compiles in its worktree.
- Committed records — focused-test: the candidates self-test runs the #788 validator
  (`validate_run`) over every record in `single-read/`, since the #788 self-test reads only
  the top-level directory, and checks each names both edited files in `git_changed_paths`.
- Summary prose — task-test-not-applicable: no executable consumer reads it.
