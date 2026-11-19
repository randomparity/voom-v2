# ext4/xfs reuse oracle and network client ChangeTime — design (#840)

## Problem

ADR 0120 lets a filesystem join the probe-reuse list only when the hash-to-probe O1 and mid-read
O10 constructions are detected on it. The `meta-ctime` oracle has run on btrfs and tmpfs only,
with a 20 ms wait before each edit, and its O10 row is `not-applicable` for metadata candidates,
so no record shows the guard's mid-read window at all. Whether the network share keeps a
client-sent ChangeTime was not measured.

## Scope

**Oracle, schema `media-evidence-fingerprint-oracle/2`** (`scripts/media-evidence-fingerprint.py`):

- `oracle --tick-wait SECONDS` (default `CLOCK_TICK_SECONDS`, 0.02; must be ≥ 0) replaces the
  fixed wait before each construction's change; the value is recorded as
  `parameters.tick_wait_seconds`.
- `Oracle.compare` and `stale_cache` take the reference digest first, then **prime**: set the
  mtime to its current value, which stamps ctime without changing it or the bytes, immediately
  before the before observation. With `--tick-wait 0` the primed ctime stamp and the change's
  stamp are separated only by the observation and the change itself. A read does not move
  ctime and the digest still precedes the change, so the reference outcome is unchanged.
- O10 for `meta` and `meta-ctime` becomes the guard's window: stat (the facts before ffprobe),
  a reader opens the file and reads its first 64 KiB, a same-size edit of one byte with the
  mtime restored lands while the reader holds the file, stat again; `detected` when the two
  fingerprints differ. For a stat-only guard this is O1's stat/edit/stat shape plus an open
  reader, and the report says so rather than counting it as independent evidence. `meta` is
  the control: it must miss.
- Each row gains `same_coarse_tick`: `true` when the coarse realtime clock (Linux
  `CLOCK_REALTIME_COARSE`, clock id 5; Python's `time` module does not export the name) read
  immediately before the prime equals its value immediately after the change, `false` when
  it advanced, `null` off Linux or where the row has no timed window (`not-applicable`
  outcomes and O10 for sampling candidates). A `true` row is a same-tick construction: a kernel
  stamping ctime from the coarse clock alone would give the primed and the changed ctime the
  same value, so only a fine-grained or multigrain ctime can detect it.
- Validation accepts `/1` records unchanged and requires `same_coarse_tick` (bool or null) and
  `tick_wait_seconds` (number ≥ 0) in `/2`.

**Probe, schema `media-evidence-reuse-ctime-probe/2`** (`scripts/media-evidence-reuse.py`):

- Each filesystem gains `client_change_time`. On network storage the probe sets mtime to
  `BUMPED_MTIME_NS` with `os.utime` through the mounted kernel SMB client, bracketing the call
  with the realtime clock, waits `settle` (2 s, beyond the share's 1 s attribute cache, so the
  stat reads the server's value), and classifies the settled ctime: `follows-mtime` (equals
  the mtime just set, not the ChangeTime the client sent), `client-time-or-server-stamp`
  (within the bracket ± 2 s), else `other`. Local storage records `not-applicable`: a local
  kernel takes no ctime from the caller.
- Method: the kernel client sends `ChangeTime` = its current time beside `LastWriteTime` in
  SMB2 SET_INFO FileBasicInformation whenever a utimes call sets a time
  (`fs/smb/client/inode.c`, `cifs_set_file_info`, upstream; the wire is not captured, since
  that needs root). It cannot send a chosen value, and no authenticated user-space SMB client
  is available (smbclient needs credentials readable only by root; no new dependency).
  `follows-mtime` answers the criterion: the server does not report a client-sent ChangeTime;
  it reports LastWriteTime. `client-time-or-server-stamp` cannot separate "kept" from "server
  stamped its own time", so it is reported as "not measurable with available tooling" (the
  operator's stated fallback). `not_measured` replaces `client_set_change_time` with
  `client_set_chosen_change_time` saying a chosen value was not sent.
- Validation accepts `/1` unchanged; `/2` requires `client_change_time` in the four values,
  `not-applicable` exactly on non-network storage.

**Runs** (operator-approved, fixtures only): 1 GiB sparse images in the session scratch
directory, attached with `losetup`, `mkfs.ext4` / `mkfs.xfs` defaults, mounted, mount root
chowned to the user. Oracle `--candidates meta,meta-ctime` at `--tick-wait 0.02` and `0` on each;
one `ctime-probe` over ext4, xfs and a fresh directory under the designated network scratch
share. Afterwards unmount, `losetup -d`, delete the images, confirm the network directory gone.

**Docs**: four oracle records and one probe record in `docs/media-evidence-baseline/reuse/`; the
reuse README gains the commands, mkfs options, kernel version, results and the share
disclosure (a different share on the same server as the share the 2026-10-29 notes describe);
the decision report's ctime line and `meta-ctime` row state the per-filesystem verdict.

Out of scope: `reuse_listed` and the ADR 0120 amendment (future post-verdict issue); historical
records stay byte-identical.

## Failure model

1. Actors and deployments: the local operator running the scripts on this Linux host; CI
   (Linux and macOS) running the selftests without root, loop devices or a network share.
2. Invariants and assets: no original media or corpus root is written (probe refuses a parent
   inside a manifest root; oracle parents are scratch mounts); committed records carry no path,
   timestamp, inode or host name (`privacy_problems`); every existing record keeps validating;
   every created loop device, mount, image and network directory is removed.
3. Accepted failure classes: finite fixtures do not prove every race or a privileged forger
   (ADR 0120 residual); same-tick evidence shows only that this host's kernel (7.3, multigrain
   ctime) detects same-tick edits, not that every kernel would, and selftests therefore assert
   zero-wait detection on no CI kernel; the probe's wire claim rests on upstream source, not a
   capture (needs root).
4. Covered elsewhere: production reuse allowlist and ADR amendment — future post-verdict issue.

## Success

- ext4 and xfs `/2` oracle records at both waits covering O1–O13 for `meta` and `meta-ctime`
  with no `invalid` row and `meta` missing O1 and O10 (the control). The `meta-ctime` outcomes
  are reported as measured, not gated: a miss is a recorded "bar not met". A zero-wait record
  with no `same_coarse_tick: true` row is reported as "same-tick not constructed" for that
  filesystem.
- One `/2` probe record covering ext4, xfs and the network share; the report states the
  network answer, or "not measurable with available tooling" for an ambiguous outcome.
- The decision report states per filesystem whether ADR 0120's O1/O10 bar is met, names the
  instrument (the Python oracle's stat model), and says #794's production rollout guard cases
  were not run on ext4 or xfs.
- `just media-evidence-fingerprint-selftest`, `just media-evidence-reuse-selftest` and
  `just ci` pass; every committed record validates.

## Considered and rejected

- smbclient `utimes` with a chosen ChangeTime: needs share credentials readable only by root;
  reading them is outside the operator's sudo grant.
- Keeping O10 `not-applicable` for metadata candidates: leaves ADR 0120's O10 bar unmeasurable.
- Asserting same-tick by ctime equality: a detecting kernel never shows equal ctimes, so the
  coarse clock is the only independent witness.
