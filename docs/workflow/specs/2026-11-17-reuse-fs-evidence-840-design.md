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
- `Oracle.compare` and `stale_cache` take the reference digest before the "before"
  observation, so with `--tick-wait 0` only the change itself lies between the before
  observation and the edit. A read does not move ctime, and the digest still precedes the
  change, so the reference outcome is unchanged.
- O10 for `meta` and `meta-ctime` becomes the guard's window: stat (the facts before ffprobe),
  a same-size edit of one byte with the mtime restored, stat again; `detected` when the two
  fingerprints differ. The construction text says so. `meta` is the control: it must miss.
- Each row gains `same_coarse_tick`: `true` when `CLOCK_REALTIME_COARSE` read immediately before
  the before observation equals its value immediately after the change, `false` when it
  advanced, `null` where the clock does not exist (non-Linux) or the row has no timed window
  (`not-applicable` outcomes and O10 for sampling candidates). A `true` row is a same-tick
  construction: a kernel stamping ctime from the coarse clock alone would give equal ctimes.
- Validation accepts `/1` records unchanged and requires `same_coarse_tick` (bool or null) and
  `tick_wait_seconds` (number ≥ 0) in `/2`.

**Probe, schema `media-evidence-reuse-ctime-probe/2`** (`scripts/media-evidence-reuse.py`):

- Each filesystem gains `client_change_time`. On network storage the probe sets mtime to
  `BUMPED_MTIME_NS` with `os.utime` through the mounted kernel SMB client, bracketing the call
  with the realtime clock, waits `settle`, and classifies the settled ctime:
  `not-kept` (equals the mtime just set), `kept-or-server-stamped` (within the bracket ± 2 s),
  else `other`. Local storage records `not-applicable`: a local kernel takes no ctime from the
  caller.
- Method: the kernel client sends `ChangeTime` = its current time beside `LastWriteTime` in
  SMB2 SET_INFO FileBasicInformation whenever a utimes call sets a time
  (`fs/smb/client/inode.c`, `cifs_set_file_info`). It cannot send a chosen value, and an
  authenticated user-space SMB client is unavailable (smbclient needs credentials this host
  does not expose to the user; no new dependency). So `kept-or-server-stamped` cannot separate
  "kept" from "server stamped its own time", while `not-kept` is conclusive. `not_measured`
  replaces `client_set_change_time` with `client_set_chosen_change_time` saying this.
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
   (ADR 0120 residual); `kept-or-server-stamped` is ambiguous by method (disclosed); coarse-tick
   evidence on a fine-grained or multigrain kernel shows only that this kernel detects same-tick
   edits, not that every kernel would.
4. Covered elsewhere: production reuse allowlist and ADR amendment — future post-verdict issue.

## Success

- ext4 and xfs `/2` oracle records at both waits: every `meta-ctime` row in O1–O13 other than
  O5/O8 detected, `meta` missing O1 and O10, no `invalid` row, and the zero-wait records report
  how many rows had `same_coarse_tick: true`.
- One `/2` probe record covering ext4, xfs and the network share with its `client_change_time`.
- The decision report states per filesystem whether ADR 0120's O1/O10 bar is met.
- `just media-evidence-fingerprint-selftest`, `just media-evidence-reuse-selftest` and
  `just ci` pass; every committed record validates.

## Considered and rejected

- smbclient `utimes` with a chosen ChangeTime: needs share credentials readable only by root;
  reading them is outside the operator's sudo grant.
- Keeping O10 `not-applicable` for metadata candidates: leaves ADR 0120's O10 bar unmeasurable.
- Asserting same-tick by ctime equality: a detecting kernel never shows equal ctimes, so the
  coarse clock is the only independent witness.
