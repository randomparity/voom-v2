# Evidence-reuse and storage-version candidates (#798)

## Problem

#796/#797's deterministic fingerprints miss a same-size edit with mtime restored (O1, O13).
Change time, random sampling and reuse are unmeasured.

## Scope

- `media-evidence-fingerprint.py` gains `meta-ctime` (`meta` plus ctime ns; stat-only like
  `meta`) and `rnd64-i8`/`-i32`: 64 KiB head/tail plus k of the M = ceil(size/64 KiB) − 2
  aligned interior blocks (k ≤ M), drawn by a seed shared by before and after, modelling a
  stored block-digest table whose build is one full read. Parameters record ctime and
  sampling kind. Oracle seeds are known to the constructor; a corpus cell holds one secret
  seed per file. #796's defaults stay; `oracle` gains `--candidates`.
- New `media-evidence-reuse.py`: `probability` (1000 single-byte interior edits per candidate
  on 8 MiB and 64 MiB fixtures); `ctime-probe` (one 64 KiB file in a unique scratch directory
  per given filesystem, removed in `finally`; content write, mtime restore, chmod, rename;
  ctime, inode and inode generation observed at once and after 2 s; refuses a directory
  inside a `--manifest` root; records bytes written and removal).
- `reuse/` records hold filesystem type, storage class, booleans and counts; the deny set
  is #796's plus ctime, generation and mount details.
- `docs/media-evidence-790-report.md`: probe reuse (savings from #788 records), cross-scan
  cache (#796/#798 stat cost against a full read), the signals, per-candidate deployment and
  threat assumptions, residual risk. Candidates are disclosed post-freeze; protocol
  unchanged. CIFS server-side change: not measured.

### Failure model

1. Deployments: one operator, one host; tmpfs, btrfs, the CIFS mount.
2. Invariants: corpora unwritten; nothing private committed; no production identity names.
3. Accepted: local ctime forgery needs root (analytic only); CIFS ChangeTime settability
   depends on the server and is unmeasured, so ctime is not adversary-resistant there.
4. Elsewhere: decision #791, lifecycle #792, tuning #789.

## Success

1. Each new candidate has one non-`invalid` outcome per O1–O13 case on tmpfs and btrfs.
2. Corpus cost and stability for the new candidates (local evicted, network subset).
3. ctime, inode and generation behaviour per probe step on tmpfs, btrfs and CIFS.
4. Per candidate and fixture, the observed rate is within 3 standard errors of k/M.
5. Each reuse candidate and signal has a cited figure or a not-exposed/not-measured reason.
6. The report keeps constructed misses and observed rates apart, and separates adversarial,
   ordinary-tool and silent-corruption changes; silent corruption needs periodic full reads.

## Validation

- Plans, fingerprints — `focused-test`: aligned distinct seed-reproducible blocks,
  ctime only in `meta-ctime`; red before candidates exist; green
  `python3 scripts/media-evidence-fingerprint-selftest.py`.
- Oracle — `focused-test`: no `invalid`; `meta-ctime` detects O1/O13; `rnd64-*` miss O1
  with seed knowledge; same command.
- `probability`, `ctime-probe`, privacy — `focused-test`: synthetic runs validate, scratch is
  removed, a manifest-root directory is refused, each denied field fails; green
  `python3 scripts/media-evidence-reuse-selftest.py` (`just media-evidence-reuse-selftest`).
- Records — `focused-test`: all reuse records validate, both oracle filesystems exist.
- No production names — `focused-test`: the selftest runs the fingerprint guard over copies of
  the reuse script and records; red on a seeded match; same command.
- Report, READMEs — `task-test-not-applicable`: prose, no executable consumer.
