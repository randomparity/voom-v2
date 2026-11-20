# Production guard cases on ext4 and xfs — design (#890)

## Problem

ADR 0120 lets a filesystem join the probe's reuse list (`reuse_listed` in
`crates/voom-ffprobe-worker/src/guard.rs`) only when the hash-to-probe O1 and mid-read O10
constructions are detected. #840 showed ext4 and xfs meet that bar in the Python oracle's stat
model only. #794's production guard cases — `scripts/media-evidence-rollout.py construct`,
which drives the release hash and probe workers on a scratch copy — ran on btrfs and tmpfs
(G4) and on a `fuse2fs` ext4 image (G5, fallback), never on native ext4 or xfs.

On `main` ext4 and xfs are unlisted, so the probe returns to the recompute path before it reads
the guard. A `main` build on those filesystems therefore tests the fallback, not the guard
(G5 showed exactly that: unchanged recomputed with no hash source, (a) and (c) detected by the
recomputed hash at `pre_probe`).

## Scope

- **Spike build.** Branch `spike/guard-cases-ext4-xfs-890`, from `main` at `a793d4e0`, adds
  ext4 (`0xEF53`) and xfs (`XFS_SUPER_MAGIC`) to `reuse_listed` and its unit test. It is pushed,
  never PR'd or merged. The release `voom-hash-worker` and `voom-ffprobe-worker` are built from
  its clean checkout and copied to a private bin directory; their SHA-256s are recorded. This
  follows #841's spike precedent. The PR branch does not change `guard.rs` (exclusion).
- **Capture.** Two native loop-mounted filesystems, each an 8 GiB sparse image file formatted as
  the user (default `mkfs.ext4` with the root directory owned by the user; default `mkfs.xfs`,
  root chowned after mount), mounted `rw,relatime` by the operator. For each, an empty scratch
  directory on the mount and:

  ```sh
  python3 scripts/media-evidence-rollout.py construct <f0004> --label local-corpus-f0004 \
    --scratch <mount>/construct --evict --bin-dir <spike-bin> \
    --output docs/media-evidence-baseline/rollout/<date>-g4-construct-<fs>.json
  ```

  `f0004` (6.08 GB) is the source #794's G4/G5 records used; each case copies it, so the
  original is only read. The instrument runs from this branch with a clean tree; the record's
  `git_commit` names the instrument checkout and `binaries_sha256` names the spike build.
- **Instrument.** No change. The record already carries everything the verdict needs:
  `scratch.filesystem_type`, per-case `stage`, `marker`, `hash_source`, `guard_sent`,
  `detected`, and the binaries' SHA-256s. The kernel release and mount options are not in the
  schema; the README states them (kernel 7.3.3, the same host as #840).
- **Records and docs.** Records go beside #794's construct records in
  `docs/media-evidence-baseline/rollout/`. That README gets a #890 section (spike provenance,
  setup, command, per-filesystem table). `docs/media-evidence-baseline/reuse/README.md`'s #840
  verdict and `docs/media-evidence-decision-report.md` (the `meta-ctime` row and the ctime
  disclosure) replace "production guard cases were not run on ext4 or xfs" with the result per
  filesystem. Listing stays a separate maintainer change.
- **No ownership transition.**

### Verdict rule

Per filesystem, the capture is a **guard** capture only when `unchanged` returns a result with
`hash_source: "reused"` (the probe took the guard path). Then:

- O1 = case (a) `a_restored_mtime_edit_before_probe`: met when `detected` is true with marker
  `pre_probe_guard`.
- O10 = case (b) `b_restored_mtime_edit_during_hash`: met when `detected` is true (the hash
  worker's `hash_drift`, which runs before any guard; filesystem-independent code, measured
  here on the filesystem).
- Case (c) is reported, not part of the O1/O10 bar.

The bar is **met** when O1 and O10 are both met, **not met** otherwise, and the README states
which case missed. A not-met result is committed as is; the guard is not changed. If
`unchanged` does not show `reused`, the capture did not test the guard: it is not committed as a
guard result, and the cause is investigated (wrong bin dir, wrong mount) before a recapture.

## Failure model

1. **Actors and deployments:** the operator mounting two loop images on one Linux host
   (kernel 7.3.3, Fedora 44) and a campaign worker running `construct` as the user; reviewers
   reading the committed records.
2. **Invariants and assets:** the original corpus file is never written (each case edits a
   private copy that `copy_source` checks is a different inode); committed records carry no host
   path (the instrument's `host_path_problems` check); the PR does not change `reuse_listed`;
   the verdict per filesystem matches the committed record.
3. **Accepted failure classes:**
   - The images sit on tmpfs-backed scratch, so `--evict` drops the ext4/xfs page cache but the
     backing file stays in RAM. Accepted: the verdict is pass/fail on detection, not timing.
   - One capture per filesystem, one kernel. Accepted: ADR 0120's bar is constructed detection,
     and #794's G4 used one capture per filesystem.
   - Case (b) needs its edit inside the hash read; the instrument refuses a late edit, so a host
     too fast for `--edit-delay 0.5` yields no record rather than a wrong one.
4. **Covered elsewhere:** the listing change and ADR 0120 amendment (maintainer); oracle schema
   moves (#891); kernels without multigrain ctime (not measured, stated in #840's disclosure).

## Validation

| Contract | Mode | Evidence |
|---|---|---|
| Records are valid `construct` records | focused-test | the instrument's own `validate_construct` runs before `write_new`; a failing record is not written |
| Records exercise the guard | focused-test | `unchanged.hash_source == "reused"` and case (a) marker `pre_probe_guard` read from each committed record |
| Docs state the per-filesystem result | task-test-not-applicable | prose read against the records; no executable consumer validates README text |
| PR leaves `guard.rs` unchanged | focused-test | `git diff origin/main -- crates/` is empty |
| Guardrails | focused-test | `just source-checks` exits 0 |
