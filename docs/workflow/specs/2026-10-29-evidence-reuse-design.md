# Evidence-reuse and storage-version candidates (#798)

## Problem

#796/#797's fingerprints miss same-size edits with mtime restored (O1, O13). Change time,
random sampling and reuse are unmeasured; #791 needs one #790 report.

## Scope

- `media-evidence-fingerprint.py` gains `meta-ctime` (`meta` plus ctime ns; stat-only like
  `meta`) and `rnd64-i8`/`-i32`: 64 KiB head/tail plus k of the M = ceil(size/64 KiB) − 2
  aligned interior blocks (k ≤ M), one seed shared by before and after, modelling a
  stored block-digest table built by one full read. Parameters record ctime and
  sampling kind. Oracle seeds are known to the constructor; a corpus cell holds one secret
  seed per file. #796's defaults stay; `oracle` gains `--candidates`.
- New `media-evidence-reuse.py`: `probability` (1000 single-byte interior edits per candidate
  on 8 MiB and 64 MiB fixtures); `ctime-probe` (one 64 KiB file in a unique scratch directory
  per given filesystem, removed in `finally`; content write, mtime restore, chmod, rename;
  ctime, inode, generation seen at once and after 2 s; required `--manifest` roots are
  refused; bytes written and removal recorded as interference).
- `reuse/` records hold filesystem type, storage class, booleans, counts; the deny set
  is #796's plus ctime, generation and mount details.
- `docs/media-evidence-790-report.md` merges #796 (oracle, cost), #797 (O14/O15, eventual
  I/O) and #798: probe reuse (#788 records), cross-scan cache (stat cost vs full read),
  signals (metadata, not the excluded snapshot evidence), assumptions, residual risk; rnd64
  cost includes the table's full read. Post-freeze candidates disclosed; protocol unchanged.
  CIFS server-side change: not measured.

### Failure model

1. Deployments: one operator, one host; tmpfs, btrfs, the CIFS mount.
2. Invariants: corpora unwritten; nothing private committed; no production identity names.
3. Accepted: local ctime forgery needs root (analytic only); CIFS ChangeTime settability
   depends on the server and is unmeasured, so ctime is not adversary-resistant there.
4. Elsewhere: decision #791, lifecycle #792, tuning #789.

## Success

1. Each new candidate: one non-`invalid` outcome per O1–O13 case on tmpfs and btrfs.
2. Corpus cost and stability for the new candidates (local evicted, network subset).
3. ctime, inode and generation behaviour per probe step on tmpfs, btrfs and CIFS.
4. Per candidate and fixture, observed rate within 3 standard errors of k/M.
5. Each reuse candidate and signal has a cited figure or a not-exposed/not-measured reason.
6. The report keeps constructed misses and observed rates apart, and separates adversarial,
   ordinary-tool and silent-corruption changes; silent corruption needs periodic full reads.

## Validation

- Plans, fingerprints — `focused-test`: aligned distinct seed-reproducible blocks,
  ctime only in `meta-ctime`; red before candidates exist; green
  `python3 scripts/media-evidence-fingerprint-selftest.py`.
- Oracle — `focused-test`: no `invalid`; `meta-ctime` detects O1/O13; `rnd64-*` miss O1
  given the seed; same command.
- `probability`, `ctime-probe`, privacy — `focused-test`: synthetic runs validate, scratch is
  removed, a manifest-root directory is refused, each denied field fails; green
  `python3 scripts/media-evidence-reuse-selftest.py` (`just media-evidence-reuse-selftest`).
- Records — `focused-test`: all reuse records validate, both oracle filesystems exist.
- No production names — `focused-test`: fingerprint guard over copies of reuse files; red
  on a seeded match; same command.
- Report, `reuse/README.md` — `task-test-not-applicable`: prose, no consumer.
